"""Make the test fixture: a chain file in the format of FORMAT.md (`vesting-launch-chain/1`).

    python tests/make_fixture.py HARNESS_DIR [OUT.json]

HARNESS_DIR is the folder of `kcc20_harness.py`. What is real in the fixture
and what is made up:

* Every transaction id is the true id of the transaction's fields, worked out
  by the Kaspa SDK, lock time included. The vesting covenant id is the one the
  SDK derives from A1.
* Every KCC20 lock is the true lock of its state, recomputed by the harness,
  and `token.template_hash` is the harness's template hash of KCC20.
* The keys are made-up keys, by their public halves only (the dry-run keys of
  an earlier rehearsal). No secret key is used or needed here.
* The TokenVesting locks are made up: a pay-to-script-hash script over the
  SHA-256 of the state. The sender does not recompute them.
* Signature scripts are placeholders (the byte 0x5a repeated), and the mass
  fields, masses, fees, floors and gate answers are plausible numbers, not
  calculated ones. No transaction of the fixture would pass a node.
* The funding coin, the token coin and the token's covenant id are made up.

The moves follow the table of FORMAT.md: B2's send_after_daa is B1's, so it
may be sent from B1's time until its send_before_daa.
"""

import hashlib
import importlib.util
import json
import sys
from pathlib import Path

from kaspa import (
    CovenantBinding,
    GenesisCovenantGroup,
    Hash,
    ScriptPublicKey,
    Transaction,
    TransactionInput,
    TransactionOutpoint,
    TransactionOutput,
)

SOMPI_PER_KAS = 100_000_000
COIN = SOMPI_PER_KAS
START = 597_154_140
LEAD = 6000
CLIFF, DURATION, TOTAL = 3000, 10000, 10
ZERO32 = "00" * 32
FEE = 250_000
FEE_FLOOR = 200_000

KEYS = {
    "payer": "0ce472ae5d8993e7609ee4ef33b344f6b8499a1259374bdf528f82240985bf03",
    "alice": "ab5d2e79cfd621b1b027ffb24e2453ed7fb571ba9a841ff0e2473466cabd168d",
    "beneficiary": "ad1d02fb804c18df3434bb8e259694120512c64136d877390d9eb46707fddec2",
}


def made_up(words: str) -> str:
    return hashlib.sha256(words.encode("utf-8")).hexdigest()


FUNDING = {
    "transaction_id": made_up("fixture funding coin"),
    "index": 2,
    "value": 1_661_402_000,
    "script_public_key_version": 0,
    "script_public_key": "20" + KEYS["payer"] + "ac",
    "block_daa_score": 593_874_770,
}
TOKEN_COVID = made_up("fixture token covenant")
TOKEN_COIN_ID = made_up("fixture token coin")


def p2pk(name: str) -> str:
    return "20" + KEYS[name] + "ac"


def load_harness(folder: str):
    spec = importlib.util.spec_from_file_location("kcc20_harness", Path(folder) / "kcc20_harness.py")
    module = importlib.util.module_from_spec(spec)
    sys.dont_write_bytecode = True
    spec.loader.exec_module(module)
    return module


class Maker:
    def __init__(self, harness):
        self.harness = harness
        self.template = bytes(harness.template_hash("KCC20")).hex()
        self.vesting_id = None

    def kcc20(self, amount: int, owner: str, scheme: int) -> dict:
        return {"contract": "KCC20", "amount": amount, "owner": owner, "owner_scheme": scheme,
                "borrow_scheme": 0, "borrow_guard": ZERO32, "extension_commitment": ZERO32}

    def kcc20_lock(self, state: dict) -> str:
        fields = {k: v for k, v in state.items() if k != "contract"}
        for name in ("owner", "borrow_guard", "extension_commitment"):
            fields[name] = bytes.fromhex(fields[name])
        return bytes(self.harness.lock("KCC20", fields)).hex()

    def vesting(self, released: int) -> dict:
        return {"contract": "TokenVesting", "beneficiary": KEYS["beneficiary"], "token_covid": TOKEN_COVID,
                "token_template": self.template, "start": START, "cliff": CLIFF, "duration": DURATION, "total": TOTAL,
                "released": released}

    @staticmethod
    def vesting_lock(state: dict) -> str:
        return "aa20" + hashlib.sha256(json.dumps(state, sort_keys=True).encode("utf-8")).hexdigest() + "87"

    def vesting_out(self, released: int, authorizing: int) -> dict:
        state = self.vesting(released)
        return {"value": COIN, "script_public_key_version": 0, "script_public_key": self.vesting_lock(state),
                "covenant_binding": {"authorizing_input": authorizing, "covenant_id": self.vesting_id},
                "role": "vesting", "to": None, "state": state, "what": f"vesting coin, {released} released"}

    def token_out(self, amount: int, owner: str, authorizing: int) -> dict:
        if owner == "vault":
            state = self.kcc20(amount, self.vesting_id, 4)
            what = f"the vault: {amount} tokens owned by the vesting covenant"
        else:
            state = self.kcc20(amount, KEYS[owner], 0)
            what = f"{amount} tokens of {owner}"
        return {"value": COIN, "script_public_key_version": 0, "script_public_key": self.kcc20_lock(state),
                "covenant_binding": {"authorizing_input": authorizing, "covenant_id": TOKEN_COVID},
                "role": "token", "to": None, "state": state, "what": what}


def change_out(value: int) -> dict:
    return {"value": value, "script_public_key_version": 0, "script_public_key": p2pk("payer"),
            "covenant_binding": None, "role": "change", "to": "payer", "state": None, "what": "the payer's change"}


def sdk_transaction(move: dict, with_bindings: bool = True) -> Transaction:
    inputs = [
        TransactionInput(TransactionOutpoint(Hash(i["transaction_id"]), i["index"]), bytes.fromhex(i["signature_script"]),
                         sequence=i["sequence"], sig_op_count=i["sig_op_count"], compute_budget=i["compute_budget"])
        for i in move["inputs"]
    ]
    outputs = []
    for o in move["outputs"]:
        bound = o["covenant_binding"] if with_bindings else None
        binding = CovenantBinding(bound["authorizing_input"], Hash(bound["covenant_id"])) if bound else None
        outputs.append(TransactionOutput(o["value"], ScriptPublicKey(o["script_public_key_version"], o["script_public_key"]), binding))
    built = Transaction(move["version"], inputs, outputs, lock_time=move["lock_time"], subnetwork_id=bytes.fromhex(move["subnetwork_id"]),
                        gas=move["gas"], payload=bytes.fromhex(move["payload"]), mass=move["mass"])
    built.finalize()
    return built


def spend(coin: dict, budget: int, script_bytes: int) -> dict:
    return {
        "transaction_id": coin["key"][0], "index": coin["key"][1], "sequence": 0, "sig_op_count": 0,
        "compute_budget": budget, "signature_script": "5a" * script_bytes,
        "spends": {"from": coin["from"], "value": coin["value"], "script_public_key_version": 0,
                   "script_public_key": coin["script"], "covenant_id": coin["covenant_id"], "what": coin["what"]},
    }


def coins_of(move: dict) -> list:
    coins = []
    for number, o in enumerate(move["outputs"]):
        bound = o["covenant_binding"]
        coins.append({"key": (move["transaction_id"], number), "from": {"move": move["name"], "output": number},
                      "value": o["value"], "script": o["script_public_key"], "covenant_id": bound["covenant_id"] if bound else None,
                      "what": o["what"]})
    return coins


def transaction(name: str, kind: str, what: str, lock: int, inputs: list, outputs: list, judged: int, before=None) -> dict:
    paid_in = sum(i["spends"]["value"] for i in inputs)
    outputs = outputs + [change_out(paid_in - sum(o["value"] for o in outputs) - FEE)]
    move = {
        "name": name, "kind": kind, "what": what, "expect": "accepted" if kind == "step" else "refused",
        "transaction_id": None, "version": 1, "lock_time": lock,
        "send_after_daa": 0 if lock == 0 else lock + 20, "send_before_daa": before, "judged_at_daa": judged,
        "subnetwork_id": "00" * 20, "gas": 0, "payload": "", "mass": 117_651,
        "inputs": inputs, "outputs": outputs, "fee": FEE, "fee_floor": FEE_FLOOR,
        "masses": {"compute_mass": 30_000, "transient_mass": 40_000, "calculated_storage_mass": 117_651, "fee_mass": 117_651},
        "gate": {"scripts": True, "consensus": True, "mempool": True, "block": True, "refusal": None},
        "refusal_expected": None, "twin": None,
    }
    return move


def refused_by_script(move: dict, line: str, twin: str) -> dict:
    move["gate"] = {"scripts": False, "consensus": False, "mempool": False, "block": False,
                    "refusal": {"layer": "scripts", "input": 0, "rule": None, "message": "script ran, but verification failed"}}
    move["refusal_expected"] = {"kind": "scripts", "input": 0, "contract_line": line, "node_message_contains": "verification failed"}
    move["twin"] = {"accepted": True, "judged_at_daa": move["judged_at_daa"], "what": twin}
    return move


def make(harness) -> dict:
    m = Maker(harness)
    funding = {"key": (FUNDING["transaction_id"], FUNDING["index"]), "from": "funding", "value": FUNDING["value"],
               "script": FUNDING["script_public_key"], "covenant_id": None, "what": "the payer's funding coin"}
    alice_state = m.kcc20(TOTAL, KEYS["alice"], 0)
    token = {"key": (TOKEN_COIN_ID, 1), "from": "token_coin", "value": COIN, "script": m.kcc20_lock(alice_state),
             "covenant_id": TOKEN_COVID, "what": "Alice's 10-token coin"}

    # A1: the vesting coin gets a new covenant id, derived from the transaction itself.
    a1 = transaction("A1 vesting genesis", "step", "the payer's coin creates the vesting coin", 0,
                     [spend(funding, 10, 66)], [], START - LEAD)
    m.vesting_id = "00" * 32
    a1["outputs"].insert(0, m.vesting_out(0, 0))
    a1["outputs"][1]["value"] -= COIN
    bare = sdk_transaction(a1, with_bindings=False)
    bare.populate_genesis_covenants([GenesisCovenantGroup(authorizing_input=0, outputs=[0])])
    bare.finalize()
    m.vesting_id = bare.outputs[0].to_dict()["covenant"]["covenantId"]
    a1["outputs"][0]["covenant_binding"]["covenant_id"] = m.vesting_id
    a1["transaction_id"] = sdk_transaction(a1).id
    assert a1["transaction_id"] == bare.id
    vesting0, change1 = coins_of(a1)

    a2 = transaction("A2 fund the vault", "step", "Alice moves her 10-token coin whole to the vesting covenant", 0,
                     [spend(token, 30, 400), spend(change1, 10, 66)], [m.token_out(TOTAL, "vault", 0)], START - LEAD)
    a2["transaction_id"] = sdk_transaction(a2).id
    vault, change2 = coins_of(a2)

    def release(name, kind, what, lock, coins, released, claim, owner, left, judged=None, before=None):
        # The order of FORMAT.md's runs: the claim, the rest of the vault, the vesting coin, the change.
        outputs = [m.token_out(claim, owner, 1)]
        if left:
            outputs.append(m.token_out(left, "vault", 1))
        if released is not None:
            outputs.append(m.vesting_out(released, 0))
        vest, held, pay = coins
        move = transaction(name, kind, what, lock, [spend(vest, 40, 600), spend(held, 30, 400), spend(pay, 10, 66)], outputs,
                           lock + 20 if judged is None else judged, before)
        return move

    b1 = refused_by_script(release("B1 before the cliff", "probe", "a release of 1 token dated before the cliff", START + 1000,
                                   (vesting0, vault, change2), 1, 1, "beneficiary", 9),
                           "require(tx.locktime >= start + cliff);", "lock time at the cliff")
    b2 = release("B2 dated at the end", "probe", "all 10 tokens dated at the end of the schedule, sent long before it",
                 START + DURATION, (vesting0, vault, change2), None, 10, "beneficiary", 0,
                 judged=b1["send_after_daa"], before=START + DURATION - 2000)
    b2["send_after_daa"] = b1["send_after_daa"]  # B2 goes from B1's time on, long before its lock time
    b2["gate"] = {"scripts": True, "consensus": False, "mempool": False, "block": False,
                  "refusal": {"layer": "consensus", "input": 0, "rule": None, "message": "transaction input #0 is not finalized"}}
    b2["refusal_expected"] = {"kind": "finality", "input": 0, "contract_line": None, "node_message_contains": "is not finalized"}
    b2["twin"] = {"accepted": True, "judged_at_daa": START + DURATION + 20, "what": "judged after its lock time"}
    b3 = refused_by_script(release("B3 more than vested", "probe", "4 tokens at the cliff, where 3 have vested", START + CLIFF,
                                   (vesting0, vault, change2), 4, 4, "beneficiary", 6),
                           "require(released <= vested);", "3 tokens")
    b4 = refused_by_script(release("B4 to another key", "probe", "3 tokens at the cliff, the claim owned by the payer's key",
                                   START + CLIFF, (vesting0, vault, change2), 3, 3, "payer", 7),
                           "require(claim.owner == beneficiary);", "the claim owned by the beneficiary")
    r1 = release("R1 first release", "step", "3 tokens at the cliff", START + CLIFF, (vesting0, vault, change2), 3, 3, "beneficiary", 7)
    r1["transaction_id"] = sdk_transaction(r1).id
    _, vault1, vesting1, change3 = coins_of(r1)
    r2 = release("R2 second release", "step", "3 more (6 vested)", START + 6500, (vesting1, vault1, change3), 6, 3, "beneficiary", 4)
    r2["transaction_id"] = sdk_transaction(r2).id
    _, vault2, vesting2, change4 = coins_of(r2)
    r3 = release("R3 the rest", "step", "the 4 left, at the end; the vesting coin ends", START + DURATION,
                 (vesting2, vault2, change4), None, 4, "beneficiary", 0)
    for move in (b1, b2, b3, b4, r3):
        move["transaction_id"] = sdk_transaction(move).id
    moves = [a1, a2, b1, b2, b3, b4, r1, r2, r3]
    steps = [move for move in moves if move["kind"] == "step"]
    return {
        "format": "vesting-launch-chain/1",
        "network": "testnet-10",
        "built_at_utc": "2026-01-01T12:05:00Z",
        "nothing_was_sent": True,
        "coin_value": COIN,
        "feerate": 200,
        "fee_rule": "made up for the test fixture: every move pays 250000 sompi",
        "fee_cap_total": 8 * SOMPI_PER_KAS,
        "schedule": {"start": START, "cliff": CLIFF, "duration": DURATION, "total": TOTAL},
        "funding": {**FUNDING, "owner": "payer"},
        "token": {"covenant_id": TOKEN_COVID, "template_hash": m.template,
                  "coin": {"transaction_id": TOKEN_COIN_ID, "index": 1, "value": COIN, "script_public_key_version": 0,
                           "script_public_key": token["script"], "block_daa_score": 593_874_700},
                  "state": alice_state},
        "vesting_covenant_id": m.vesting_id,
        "parties": {name: {"x_only_public_key": key, "script_public_key": p2pk(name)} for name, key in KEYS.items()},
        "launch_checks": {"check_vesting_genesis": "ok", "check_parameters": "ok", "check_vault": "ok"},
        "moves": moves,
        "totals": {
            "fees_of_steps": FEE * len(steps),
            "fees_of_probes": FEE * (len(moves) - len(steps)),
            "tokens_to_beneficiary": TOTAL,
            "kas_locked_at_the_end": 3 * COIN,
            "change_at_the_end": r3["outputs"][-1]["value"],
        },
    }


def main(argv: list) -> None:
    if not 1 <= len(argv) <= 2:
        raise SystemExit(__doc__)
    out = Path(argv[1]) if len(argv) == 2 else Path(__file__).resolve().parent / "fixture-chain.json"
    chain = make(load_harness(argv[0]))
    with open(out, "w", encoding="utf-8", newline="\n") as file:
        file.write(json.dumps(chain, indent=1) + "\n")
    print(f"written: {out} ({len(chain['moves'])} moves)")


if __name__ == "__main__":
    main(sys.argv[1:])
