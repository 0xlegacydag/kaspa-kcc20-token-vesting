"""A one-off for the run of 2026-10-30: the transfer that makes Alice's 10-token coin.

    python results/2026-10-30-testnet-10/collect/collect_coins.py read OUT.json     read-only: the three coins as the node reports them
    python results/2026-10-30-testnet-10/collect/collect_coins.py check TX.json     read-only: the file against the SDK and the node
    python results/2026-10-30-testnet-10/collect/collect_coins.py send TX.json RECORD.jsonl --yes-send-to-testnet-10

It holds no key. Run with the environment of the sender, from the sender's folder (testnet/),
with KCC20_HARNESS_DIR set. The coins it read and spent are gone now: `read`, `check` and `send`
stop at them, and the file it sent is collect-tx.json next to it.
"""
import asyncio
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path.cwd()))
import vesting_send as vs  # noqa: E402

CHAIN = Path(__file__).resolve().parents[2] / "2026-10-15-testnet-10" / "chain.json"
COINS = {"R1 first release": 3, "R2 second release": 3, "R3 the rest": 4}


def utc() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def the_coins() -> list:
    chain = json.loads(CHAIN.read_text(encoding="utf-8"))
    found = []
    for move in chain["moves"]:
        if move["name"] in COINS:
            output = move["outputs"][0]
            assert output["state"]["amount"] == COINS[move["name"]], move["name"]
            found.append({"transaction_id": move["transaction_id"], "index": 0, "amount": output["state"]["amount"], "value": output["value"], "script_public_key": output["script_public_key"]})
    assert len(found) == 3
    return found, chain


async def find(client, script: str, wanted: tuple) -> dict | None:
    for entry in await vs.utxos_at(client, vs.address_of_script(0, script)):
        if vs.entry_key(entry) == wanted:
            return entry
    return None


async def cmd_read(out: Path) -> int:
    assert not out.exists(), "the file exists"
    coins, chain = the_coins()
    client, info = await vs.open_node()
    try:
        print(f"via {client.url}, node version {info.get('serverVersion')}, score {vs.daa_of(info)}")
        for coin in coins:
            entry = await find(client, coin["script_public_key"], (coin["transaction_id"], coin["index"]))
            assert entry is not None, f"{coin['transaction_id']} is not unspent"
            utxo = entry["utxoEntry"]
            assert int(utxo["amount"]) == coin["value"]
            assert vs.entry_script(entry) == (0, coin["script_public_key"])
            assert utxo.get("covenantId") == chain["token"]["covenant_id"]
            coin["block_daa_score"] = int(utxo["blockDaaScore"])
            print(f"  {coin['amount']} tokens  {coin['transaction_id']}:{coin['index']}  unspent")
        request = {
            "read_at_utc": utc(),
            "node": client.url,
            "virtual_daa_score": vs.daa_of(info),
            "covenant_id": chain["token"]["covenant_id"],
            "beneficiary": chain["parties"]["beneficiary"]["x_only_public_key"],
            "alice": chain["parties"]["alice"]["x_only_public_key"],
            "payer": chain["parties"]["payer"]["x_only_public_key"],
            "coins": coins,
        }
    finally:
        await client.disconnect()
    out.write_text(json.dumps(request, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"written {out}")
    return 0


def check_file(tx: dict) -> None:
    assert tx["format"] == "collect-one-off" and tx["nothing_was_sent"] is True
    built = vs.build_transaction(tx)
    assert str(built.id) == tx["transaction_id"], "the SDK works out another id"
    differences = vs.differences_outside_the_id(built, tx)
    assert not differences, differences
    assert len(tx["inputs"]) == 3 and len(tx["outputs"]) == 2
    total_in = sum(item["spends"]["value"] for item in tx["inputs"])
    total_out = sum(item["value"] for item in tx["outputs"])
    assert total_in - total_out == tx["fee"], "the fee is not what the coins say"
    assert tx["fee"] < 100_000_000, "the fee is above 1 KAS"
    token, change = tx["outputs"]
    assert token["value"] == 100_000_000 and token["covenant_binding"]["authorizing_input"] == 0
    assert change["covenant_binding"] is None
    assert tx["token_state"]["amount"] == 10
    assert tx["lock_time"] == 0 and tx["gas"] == 0 and tx["payload"] == ""


async def checked_against_the_node(client, tx: dict) -> list:
    harness = vs.load_harness(os.environ["KCC20_HARNESS_DIR"])
    assert vs.kcc20_lock(harness, tx["token_state"]) == tx["outputs"][0]["script_public_key"], "the harness computes another lock for the new coin"
    entries = []
    for item in tx["inputs"]:
        spends = item["spends"]
        entry = await find(client, spends["script_public_key"], (item["transaction_id"], item["index"]))
        assert entry is not None, f"{item['transaction_id']}:{item['index']} is not unspent"
        assert vs.entry_script(entry) == (0, spends["script_public_key"]) and int(entry["utxoEntry"]["amount"]) == spends["value"]
        entries.append(entry)
    return entries


async def cmd_check(path: Path) -> int:
    tx = json.loads(path.read_text(encoding="utf-8"))
    check_file(tx)
    client, info = await vs.open_node()
    try:
        await checked_against_the_node(client, tx)
        print(f"via {client.url}, node version {info.get('serverVersion')}, score {vs.daa_of(info)}")
    finally:
        await client.disconnect()
    print(f"ok: id {tx['transaction_id']}, fee {tx['fee']} sompi, three coins unspent, the new lock recomputed by the harness. Nothing was sent.")
    return 0


async def cmd_send(path: Path, record: Path) -> int:
    assert not record.exists(), "the record exists"
    tx = json.loads(path.read_text(encoding="utf-8"))
    check_file(tx)
    client, info = await vs.open_node()

    def note(event: str, **more) -> None:
        line = {"at_utc": utc(), "event": event, "node": client.url, **more}
        with record.open("a", encoding="utf-8", newline="\n") as file:
            file.write(json.dumps(line, ensure_ascii=False) + "\n")
        print(line)

    try:
        entries = await checked_against_the_node(client, tx)
        note("checked", server_version=info.get("serverVersion"), score=vs.daa_of(info), transaction_id=tx["transaction_id"], fee=tx["fee"])
        built = vs.build_transaction(tx, entries)
        assert built.id == tx["transaction_id"] and not vs.differences_outside_the_id(built, tx), "built for sending it differs from the file"
        accepted, answer = await vs.hand_over(client, built, tx["transaction_id"])
        note("answer", accepted=accepted, words=answer)
        if not accepted or answer != tx["transaction_id"]:
            return 2
        token = tx["outputs"][0]
        wanted = (tx["transaction_id"], 0)
        started = time.monotonic()
        while time.monotonic() - started < 120:
            entry = await find(client, token["script_public_key"], wanted)
            if entry is not None:
                assert int(entry["utxoEntry"]["amount"]) == token["value"]
                assert entry["utxoEntry"].get("covenantId") == token["covenant_binding"]["covenant_id"]
                note("seen", outpoint=f"{wanted[0]}:0", seconds=round(time.monotonic() - started, 1), block_daa_score=int(entry["utxoEntry"]["blockDaaScore"]))
                state = {
                    "covenant_id": token["covenant_binding"]["covenant_id"],
                    "state": tx["token_state"],
                    "source": f"collect-tx.json, the transfer {tx['transaction_id']}: the beneficiary's coins of 3, 3 and 4 tokens of the run of 2026-10-15 into one coin of 10 tokens of Alice",
                    "outpoint": f"{wanted[0]}:0",
                    "value": token["value"],
                    "script_public_key": token["script_public_key"],
                }
                out = path.parent.parent / "token-state.json"
                out.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
                print(f"written {out}")
                return 0
            await asyncio.sleep(1)
        note("not seen within 120 seconds")
        return 4
    finally:
        await client.disconnect()


def main() -> int:
    command = sys.argv[1]
    if command == "read":
        return asyncio.run(cmd_read(Path(sys.argv[2])))
    if command == "check":
        return asyncio.run(cmd_check(Path(sys.argv[2])))
    if command == "send":
        if sys.argv[4:] != ["--yes-send-to-testnet-10"]:
            print("Nothing without --yes-send-to-testnet-10")
            return 1
        return asyncio.run(cmd_send(Path(sys.argv[2]), Path(sys.argv[3])))
    print(__doc__)
    return 1


raise SystemExit(main())
