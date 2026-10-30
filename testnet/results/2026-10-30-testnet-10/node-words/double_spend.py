"""One-off: what a real node of testnet-10 says about a second transaction on a coin in its mempool.

Two plain transactions of the payer, one coin, different fees, handed to one node together, on one
connection. Up to five coins of the payer, one at a time; it stops at the first answer that names
another transaction. Each coin goes back to the payer, less a fee. The key file is read, never shown.

    cd kaspa-covenant-starter-lab (its testnet/counter_live.py is imported)
    python PATH/double_spend.py KEY_FILE RECORD.jsonl --yes-send-to-testnet-10

Kept for the record: it needs the payer's key, which is not published, and the first of its coins is spent.
"""
import asyncio
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path.cwd()))
from kaspa import PrivateKey  # noqa: E402
from testnet import counter_live as live  # noqa: E402

PAYER = "kaspatest:qr9qu3z2cfv03506hxwct55hz00acg0wynz2c733cnhy0t9v0d4kg8gzyrlea"
COINS = [
    "da5a7ffe0f388af7b4f56e2652aa2750784e46782df90a72abdef51ca4bde74e:0",
    "7189b9f8747e12b0f8d660d675b0066e16cc001853a5d3079b56c5704677ff7b:0",
    "9fc3a1b00004c852c1a36fd69697ef699539dc33405bb286bcedf63a9be0a3f0:0",
    "bb8fec128b38ddbecf82de8636b73f1a607fa17aed9485daf3982505d48b4ff6:0",
    "4937f809214c817dcfa2ad5315cf4ba1eb45d332311365d6723747a484f9b72c:0",
]
FEERATE = 200
MARK = "already spent by transaction"


def utc() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


async def hand(client, tx) -> dict:
    started = utc()
    try:
        result = await asyncio.wait_for(client.submit_transaction({"transaction": tx, "allowOrphan": False}), 30)
        return {"handed_at_utc": started, "accepted": True, "words": str(result["transactionId"])}
    except Exception as error:
        return {"handed_at_utc": started, "accepted": False, "words": str(error) or type(error).__name__}


def signed(entry: dict, value: int, key, script, feerate: int):
    plan = live.Plan(inputs=[live.make_input(entry)], input_values=[value], fixed_outputs=[], change_script=script)
    tx, fee, _, _ = live.build(plan, feerate)
    return live.sign_transaction(tx, [key], True), fee


async def main(key_file: str, record: Path) -> int:
    assert not record.exists(), "the record exists"
    key = PrivateKey(Path(key_file).read_text(encoding="ascii").strip())
    assert key.to_keypair().to_address("testnet").to_string() == PAYER, "the key file holds another key"
    script = live.script_of(key)
    client = await live.connect()

    def note(line: dict) -> None:
        with record.open("a", encoding="utf-8", newline="\n") as file:
            file.write(json.dumps({"at_utc": utc(), "node": client.url, **line}, ensure_ascii=False) + "\n")
        print(line, flush=True)

    try:
        for number, wanted in enumerate(COINS, 1):
            answer = await client.get_utxos_by_addresses({"addresses": [PAYER]})
            entries = {f'{e["outpoint"]["transactionId"]}:{e["outpoint"]["index"]}': e for e in answer["entries"]}
            if wanted not in entries:
                note({"try": number, "coin": wanted, "skipped": "not unspent"})
                continue
            entry = entries[wanted]
            value = int(entry["utxoEntry"]["amount"])
            first, first_fee = signed(entry, value, key, script, FEERATE)
            second, second_fee = signed(entry, value, key, script, FEERATE + 1)
            assert first.id != second.id
            note({"try": number, "coin": wanted, "value": value, "first": first.id, "first_fee": first_fee, "second": second.id, "second_fee": second_fee})
            answers = await asyncio.gather(hand(client, first), hand(client, second))
            note({"try": number, "first_answer": answers[0], "second_answer": answers[1]})
            if any(MARK in item["words"] for item in answers):
                return 0
            await asyncio.sleep(3)
        return 3
    finally:
        await client.disconnect()


if sys.argv[3:] != ["--yes-send-to-testnet-10"]:
    print(__doc__)
    raise SystemExit(1)
raise SystemExit(asyncio.run(main(sys.argv[1], Path(sys.argv[2]))))
