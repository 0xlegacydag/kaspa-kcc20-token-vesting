"""Offline checks of `vesting_send.py`: no network, a fake node, no keys.

Run from the folder of `vesting_send.py`:

    python -m unittest discover -s tests -v

The chain file under test is `tests/fixture-chain.json`, made by
`tests/make_fixture.py`. Its transaction ids are real ids of its fields and its
KCC20 locks are real; its signature scripts are placeholders and its
TokenVesting locks are made up, so the fake node below judges no script: it
takes or refuses what it is told to. It does judge lock times: a transaction
whose lock time is not below its DAA score is refused as not finalized.

`send` does not run without a harness. The tests hand it a stand-in that knows
the KCC20 locks of the fixture, so they need nothing outside this folder. The
tests that recompute the locks for real need the study's harness
(`kcc20_harness.py`): its folder is taken from the environment variable
KCC20_HARNESS_DIR and from nowhere else; without it those tests are skipped.
"""

import asyncio
import atexit
import contextlib
import copy
import io
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from kaspa import ScriptPublicKey, address_from_script_public_key

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import vesting_send as vs  # noqa: E402

FIXTURE = json.loads((HERE / "fixture-chain.json").read_text(encoding="utf-8"))
KAS = vs.SOMPI_PER_KAS
START = FIXTURE["schedule"]["start"]
NAMES = [move["name"] for move in FIXTURE["moves"]]
IDS = {move["name"]: move["transaction_id"] for move in FIXTURE["moves"]}
A1, A2, B1, B2, B3, B4, R1, R2, R3 = NAMES
SENT_IN_A_WHOLE_RUN = [IDS[name] for name in NAMES]
FOREIGN_KEY = "11" * 32
FOREIGN_KEY_SCRIPT = "20" + FOREIGN_KEY + "ac"

HARNESS = os.environ.get("KCC20_HARNESS_DIR") or ""
HAVE_HARNESS = bool(HARNESS) and (Path(HARNESS) / "kcc20_harness.py").is_file()
NO_HARNESS = "set KCC20_HARNESS_DIR to the folder of kcc20_harness.py to run the checks that recompute the locks"

SCRIPT_REFUSAL = "failed to verify the signature script: script ran, but verification failed"
FEE_REFUSAL = "transaction has 100 fees which is under the required amount of 200"


def plain_state(state: dict) -> str:
    return json.dumps({k: (v.hex() if isinstance(v, bytes) else v) for k, v in state.items() if k != "contract"}, sort_keys=True)


class StandInHarness:
    """Knows the KCC20 lock of every state the fixture carries, and of no other state."""

    def __init__(self, chain: dict):
        self.template = bytes.fromhex(chain["token"]["template_hash"])
        self.locks = {plain_state(chain["token"]["state"]): chain["token"]["coin"]["script_public_key"]}
        for move in chain["moves"]:
            for output in move["outputs"]:
                if output["role"] == "token" and output["state"]:
                    self.locks[plain_state(output["state"])] = output["script_public_key"]

    def lock(self, contract: str, state: dict) -> bytes:
        assert contract == "KCC20"
        return bytes.fromhex(self.locks[plain_state(state)])

    def template_hash(self, contract: str) -> bytes:
        assert contract == "KCC20"
        return self.template


STAND_IN = StandInHarness(FIXTURE)
STAND_IN_FOLDER = Path(tempfile.mkdtemp(prefix="stand-in-harness-"))
(STAND_IN_FOLDER / "kcc20_harness.py").write_text("# a stand-in for the tests\n", encoding="utf-8")
atexit.register(shutil.rmtree, STAND_IN_FOLDER, True)
REAL_LOAD = vs.load_harness
STAND_INS = {STAND_IN_FOLDER: STAND_IN}  # folder -> stand-in; more are added for the builder's chains


def load_harness(folder: str):
    return STAND_INS[Path(folder)] if Path(folder) in STAND_INS else REAL_LOAD(folder)

# The builder's own self-test chains (real signatures, made-up keys), read where the builder wrote them:
# VESTING_SELFTEST_DIR, or by default target/tmp/testnet-selftest in the root of the repository.
BUILDER_FOLDER = Path(os.environ.get("VESTING_SELFTEST_DIR") or HERE.parent.parent / "target" / "tmp" / "testnet-selftest")
BUILDER_CHAINS = [BUILDER_FOLDER / f"vesting-selftest-{rate}.chain.json" for rate in (100, 1000)]


def verify_report(chain: dict, **change) -> dict:
    report = {
        "format": "vesting-verify-report/1",
        "vesting_covenant_id": chain["vesting_covenant_id"],
        "mismatches": 0,
        "outputs": [
            {"move": move["name"], "output": number, "role": output["role"], "script_public_key": output["script_public_key"], "ok": True}
            for move in chain["moves"] for number, output in enumerate(move["outputs"]) if output["role"] in ("vesting", "token")
        ],
    }
    report.update(change)
    return report


def address(script: str) -> str:
    return address_from_script_public_key(ScriptPublicKey(0, script), "testnet").to_string()


PARTIES = {name: address(party["script_public_key"]) for name, party in FIXTURE["parties"].items()}


def party_args(parties: dict = PARTIES) -> list:
    return [word for name, at in parties.items() for word in ("--party", f"{name}={at}")]


def move_of(chain: dict, name: str) -> dict:
    return next(move for move in chain["moves"] if move["name"] == name)


class FakeNode:
    """Stands in for the RPC client: holds coins by address, a DAA score, and does with a submission what it is told.

    `how` maps a transaction id to one of: "accept" (the default for a step:
    its inputs go, its outputs appear), "refuse" (the default for a probe: a
    script refusal), "refuse-fee" (a refusal for another reason),
    "accept-hidden" (taken, its outputs never show), "lost" (not taken, no
    answer). An accepted transaction whose lock time is not below the DAA
    score is refused as not finalized. The DAA score grows by `daa_step` with
    every question the node is asked about itself.
    """

    def __init__(self, chain: dict = FIXTURE, network: str = vs.NETWORK_ID, synced: bool = True, version: str = "2.1.0",
                 daa: int | None = None, daa_step: int = 100, funded: bool = True):
        self.url = "wss://fake.example"
        self.network, self.synced, self.version = network, synced, version
        self.daa = START - 6000 if daa is None else daa
        self.daa_step = daa_step
        self.coins: dict = {}
        self.how: dict = {}
        self.probe_ids = {move["transaction_id"] for move in chain["moves"] if move["kind"] == "probe"}
        self.submitted, self.handed, self.daa_at_submission = [], [], []
        self.questions = self.connections = self.reconnects = self.reads = 0
        self.after_accept = None  # called with the id of every transaction the node takes
        self.on_question = None  # called whenever the node is asked about itself
        self.failing_reads: list = []  # exceptions the next reads of coins raise
        self.rate = 150.0
        if funded:
            funding, token = chain["funding"], chain["token"]
            self.put(funding["transaction_id"], funding["index"], funding["value"], funding["script_public_key"])
            coin = token["coin"]
            self.put(coin["transaction_id"], coin["index"], coin["value"], coin["script_public_key"], token["covenant_id"])

    def put(self, txid: str, index: int, amount: int, script: str, covenant_id: str | None = None) -> None:
        at = address(script)
        self.coins.setdefault(at, {})[(txid, index)] = {
            "address": at,
            "outpoint": {"transactionId": txid, "index": index},
            "utxoEntry": {"amount": amount, "scriptPublicKey": "0000" + script, "blockDaaScore": 1, "isCoinbase": False,
                          "covenantId": covenant_id},
        }

    def holds(self, txid: str, index: int) -> bool:
        return any((txid, index) in coins for coins in self.coins.values())

    def remove(self, txid: str, index: int) -> None:
        for coins in self.coins.values():
            coins.pop((txid, index), None)

    def apply(self, shown: dict) -> None:
        spent = [(i["previousOutpoint"]["transactionId"], i["previousOutpoint"]["index"]) for i in shown["inputs"]]
        if not all(self.holds(*key) for key in spent):
            raise RuntimeError(f"Rejected transaction {shown['id']}: transaction {shown['id']} is an orphan where orphan is disallowed")
        for key in spent:
            self.remove(*key)
        for number, output in enumerate(shown["outputs"]):
            covenant = (output.get("covenant") or {}).get("covenantId")
            self.put(shown["id"], number, output["value"], output["scriptPublicKey"]["script"], covenant)

    async def connect(self):
        self.reconnects += 1

    async def disconnect(self):
        return None

    async def get_server_info(self):
        self.questions += 1
        if self.on_question:
            self.on_question()
        info = {"networkId": self.network, "isSynced": self.synced, "serverVersion": self.version, "virtualDaaScore": self.daa}
        self.daa += self.daa_step
        return info

    async def get_fee_estimate(self):
        bucket = {"feerate": self.rate, "estimatedSeconds": 0.5}
        return {"estimate": {"priorityBucket": bucket, "normalBuckets": [bucket], "lowBuckets": [bucket]}}

    async def get_utxos_by_addresses(self, request):
        self.reads += 1
        if self.failing_reads:
            raise self.failing_reads.pop(0)
        return {"entries": list(self.coins.get(request["addresses"][0], {}).values())}

    async def submit_transaction(self, request):
        assert request["allowOrphan"] is False
        shown = request["transaction"].to_dict()
        txid = shown["id"]
        assert all(tx_input["utxo"] is not None for tx_input in shown["inputs"])
        self.submitted.append(txid)
        self.handed.append(shown)
        self.daa_at_submission.append(self.daa)
        how = self.how.get(txid, "refuse" if txid in self.probe_ids else "accept")
        if how in ("accept", "accept-hidden", "refuse") and shown["lockTime"] and self.daa <= shown["lockTime"]:
            raise RuntimeError(f"Rejected transaction {txid}: transaction input #0 is not finalized")
        if how == "accept":
            self.apply(shown)
            if self.after_accept:
                self.after_accept(txid)
            return {"transactionId": txid}
        if how == "accept-hidden":
            return {"transactionId": txid}
        if how == "lost":
            raise ConnectionError("WebSocket is not connected")
        if how == "refuse":
            raise RuntimeError(f"Rejected transaction {txid}: {SCRIPT_REFUSAL}")
        if how == "refuse-fee":
            raise RuntimeError(f"Rejected transaction {txid}: {FEE_REFUSAL}")
        if how == "rejected-under-another-id":
            raise RuntimeError(f"Rejected transaction {'ee' * 32}: something about another transaction")
        if how.startswith("raw:"):
            raise RuntimeError(how[4:].replace("TXID", txid))
        if how.startswith("say:"):
            raise RuntimeError(f"RPC Server (remote error) -> Rejected transaction {txid}: {how[4:]}")
        raise AssertionError(how)


TIMING = (("poll_seconds", 0), ("seen_polls", 3), ("lost_polls", 3), ("read_pause_seconds", 0),
          ("daa_poll_seconds", 0), ("progress_seconds", 10**9))


def run(argv: list, node: FakeNode | None = None, timing: tuple = ()) -> tuple:
    """Run the program. Returns (exit code, what it printed)."""

    async def fake_connect():
        if node is None:
            raise AssertionError("the network was reached")
        node.connections += 1
        return node, await vs.check_node(node)

    printed = io.StringIO()
    environment = {name: value for name, value in os.environ.items() if name != "KCC20_HARNESS_DIR"}
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.dict(os.environ, environment, clear=True))
        stack.enter_context(mock.patch.object(vs, "connect", fake_connect))
        stack.enter_context(mock.patch.object(vs, "load_harness", load_harness))
        for name, value in TIMING + timing:
            stack.enter_context(mock.patch.object(vs.Timing, name, value))
        stack.enter_context(contextlib.redirect_stdout(printed))
        code = vs.main(argv)
    return code, printed.getvalue()


class Folder(unittest.TestCase):
    """A temporary folder per test, with the chain file in it."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.folder = Path(temporary.name)

    def chain_file(self, chain: dict = FIXTURE) -> str:
        path = self.folder / "chain.json"
        path.write_text(json.dumps(chain), encoding="utf-8")
        return str(path)

    def check(self, chain: dict = FIXTURE, parties: dict = PARTIES, more: tuple = (), node=None) -> tuple:
        return run(["check", self.chain_file(chain), *party_args(parties), *more], node)

    def report_file(self, report: dict) -> str:
        path = self.folder / "verify-report.json"
        path.write_text(json.dumps(report), encoding="utf-8")
        return str(path)

    def send(self, node, chain: dict = FIXTURE, flag: bool = True, record: str = "record", harness: bool = True,
             more: tuple = (), timing: tuple = (), parties: dict = PARTIES, stand_in: Path = STAND_IN_FOLDER,
             report: dict | None = None, verified: bool = True) -> tuple:
        argv = ["send", self.chain_file(chain), *party_args(parties), "--record", str(self.folder / record), *more]
        argv += ["--harness", str(stand_in)] if harness else []
        argv += ["--verify-report", self.report_file(report or verify_report(chain))] if verified else []
        return run(argv + (["--yes-send-to-testnet-10"] if flag else []), node, timing)

    def events(self, record: str = "record") -> list:
        lines = []
        for path in sorted((self.folder / record).glob("run-*.jsonl")):
            lines += [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
        return lines

    def summaries(self, record: str = "record") -> list:
        return [json.loads(path.read_text(encoding="utf-8")) for path in sorted((self.folder / record).glob("run-*-summary.json"))]


# --------------------------------------------------------------------------
# check: one broken file per check
# --------------------------------------------------------------------------


def edit(name: str, change):
    """A change to the move `name` of a chain."""
    return lambda chain: change(move_of(chain, name))


def output_of(name: str, number: int, change):
    return edit(name, lambda move: change(move["outputs"][number]))


def state_of(name: str, number: int, change):
    return output_of(name, number, lambda output: change(output["state"]))


def setter(**fields):
    return lambda item: item.update(fields)


def swap_moves(chain):
    chain["moves"][2], chain["moves"][3] = chain["moves"][3], chain["moves"][2]


def spend_from(name: str, number: int, source_name: str, source_output: int):
    """Input `number` of `name` spends output `source_output` of `source_name`, with a `spends` block that fits."""

    def change(chain):
        source = move_of(chain, source_name)
        output = source["outputs"][source_output]
        tx_input = move_of(chain, name)["inputs"][number]
        tx_input.update(transaction_id=source["transaction_id"], index=source_output)
        binding = output["covenant_binding"]
        tx_input["spends"].update({"from": {"move": source_name, "output": source_output}, "value": output["value"],
                                   "script_public_key": output["script_public_key"],
                                   "covenant_id": binding["covenant_id"] if binding else None})
    return change


def fee_above_the_hard_limit(chain):
    r3 = move_of(chain, R3)
    r3["outputs"][-1]["value"] -= 9 * KAS
    r3["fee"] += 9 * KAS
    chain["totals"]["fees_of_steps"] += 9 * KAS
    chain["fee_cap_total"] = 20 * KAS


def too_many_probes(chain):
    for number in range(7):
        extra = copy.deepcopy(move_of(chain, B1))
        extra["name"] = f"B1 copy {number}"
        chain["moves"].append(extra)


VESTING_ID = FIXTURE["vesting_covenant_id"]
TOKEN_ID = FIXTURE["token"]["covenant_id"]
PAYER_KEY = FIXTURE["parties"]["payer"]["x_only_public_key"]
BENEFICIARY_KEY = FIXTURE["parties"]["beneficiary"]["x_only_public_key"]

BROKEN = [
    # a. format, network, limits, launch checks, gates
    ("format", lambda c: c.update(format="kcc20-launch-chain/1"), "not 'vesting-launch-chain/1'"),
    ("network", lambda c: c.update(network="mainnet"), "wrong network"),
    ("nothing_was_sent", lambda c: c.update(nothing_was_sent=False), "nothing_was_sent is not true"),
    ("coin_value", lambda c: c.update(coin_value=2 * KAS), "the run allows exactly 1.00000000"),
    ("funding_above_20_kas", lambda c: c["funding"].update(value=21 * KAS), "above the hard limit of 20.00000000 KAS"),
    ("feerate_below_100", lambda c: c.update(feerate=99), "is below 100 sompi per gram"),
    ("schedule_cliff", lambda c: c["schedule"].update(cliff=2000), "schedule: cliff is 2000; this run has 3000"),
    ("schedule_total", lambda c: c["schedule"].update(total=11), "schedule: total is 11; this run has 10"),
    ("launch_check_failed", lambda c: c["launch_checks"].update(check_vault="failed"), "launch_checks: check_vault is 'failed'"),
    ("launch_check_missing", lambda c: c["launch_checks"].pop("check_parameters"), "launch_checks: check_parameters is None"),
    ("step_gate_failed", edit(R1, lambda m: m["gate"].update(mempool=False)), "the builder's gate did not pass it (mempool)"),
    ("probe_expects_acceptance", edit(B1, setter(expect="accepted")), 'a probe must say "refused"'),
    ("step_with_a_twin", edit(R1, setter(twin={"accepted": True, "judged_at_daa": 1, "what": "x"})), "a step with a refusal_expected or a twin"),
    ("twin_not_accepted", edit(B3, lambda m: m["twin"].update(accepted=False)), "its twin (the same move with the wrong thing put right) was not accepted"),
    ("too_many_probes", too_many_probes, "11 probes; the run allows at most 10"),
    # b. the moves, lock times, windows, refusals
    ("moves_out_of_order", swap_moves, "this run has exactly"),
    ("lock_time_of_r2", edit(R2, lambda m: m.update(lock_time=m["lock_time"] + 1)), f"by the schedule it is {START + 6500} (start + 6500)"),
    ("lock_time_of_a2", edit(A2, setter(lock_time=START)), "by the schedule it is 0"),
    ("lock_time_of_b1", edit(B1, setter(lock_time=START + 3000)), f"by the schedule it is {START + 1000}"),
    ("send_after_daa", edit(R1, lambda m: m.update(send_after_daa=m["send_after_daa"] + 1)), f"send_after_daa is {START + 3021}; by its rule it is {START + 3020}"),
    ("send_after_daa_of_a1", edit(A1, setter(send_after_daa=5)), "send_after_daa is 5; by its rule it is 0"),
    ("send_before_daa_on_b1", edit(B1, setter(send_before_daa=START - 1000)), "send_before_daa is"),
    ("send_before_daa_of_b2", edit(B2, setter(send_before_daa=None)), f"send_before_daa is None; by its rule it is {START + 8000}"),
    ("judged_at_daa_of_b2", edit(B2, setter(judged_at_daa=START)), f"by its rule it is {START + 1020}"),
    ("send_after_daa_of_b2", edit(B2, lambda m: m.update(send_after_daa=m["lock_time"] + 20)), f"send_after_daa is {START + 10020}; by its rule it is {START + 1020}"),
    ("judged_at_daa_of_r2", edit(R2, setter(judged_at_daa=START)), f"by its rule it is {START + 6520}"),
    ("refusal_kind", edit(B1, lambda m: m["refusal_expected"].update(kind="finality")), "this probe is refused by 'scripts'"),
    ("refusal_text_empty", edit(B3, lambda m: m["refusal_expected"].update(node_message_contains=" ")), "refusal_expected.node_message_contains is empty"),
    ("gate_layer", edit(B1, lambda m: m["gate"]["refusal"].update(layer="consensus")), "the gate refused it at the layer 'consensus'"),
    ("gate_input", edit(B4, lambda m: m["gate"]["refusal"].update(input=1)), "the gate refused input 1; refusal_expected says input 0"),
    ("gate_passed_the_refusing_layer", edit(B2, lambda m: m["gate"].update(consensus=True)), "is not false, though it refused it there"),
    ("twin_of_b2_before_its_lock_time", edit(B2, lambda m: m["twin"].update(judged_at_daa=START)), "not after its lock time"),
    # c. parties
    ("funding_owner", lambda c: c["funding"].update(owner="alice"), "the funding coin must be the payer's"),
    ("party_script", lambda c: c["parties"]["alice"].update(script_public_key=FOREIGN_KEY_SCRIPT), "is not the pay-to-public-key script of its key"),
    # d. rebuild and fixed fields
    ("id_mismatch", output_of(R1, 3, lambda o: o.update(value=o["value"] - 1)), "id mismatch"),
    ("sequence", edit(B1, lambda m: m["inputs"][0].update(sequence=1)), "sequence is 1; a transaction of this run has only 0 there"),
    ("payload", edit(A2, setter(payload="00")), "payload is '00'"),
    ("gas", edit(R2, setter(gas=1)), "gas is 1"),
    ("version", edit(R2, setter(version=0)), "version is 0"),
    ("subnetwork", edit(R2, setter(subnetwork_id="01" + "00" * 19)), "subnetwork_id is '01"),
    ("sig_op_count", edit(R1, lambda m: m["inputs"][2].update(sig_op_count=1)), "sig_op_count is 1"),
    ("empty_signature_script", edit(R2, lambda m: m["inputs"][0].update(signature_script="")), "empty signature script"),
    ("compute_budget", edit(R1, lambda m: m["inputs"][0].update(compute_budget=1001)), "the compute budget 1001 is above 1000"),
    ("mass", edit(R1, setter(mass=500_001)), "the mass field 500001 is not between 1 and 500000"),
    ("lock_time_above_the_limit", edit(R3, setter(lock_time=600_000_000_000)), "is neither 0 nor between the start"),
    # e. linkage
    ("spends_an_output_that_does_not_exist", edit(R2, lambda m: m["inputs"][0]["spends"].update({"from": {"move": R1, "output": 9}})), "output 9 of the move 'R1 first release' does not exist"),
    ("spends_a_later_step", edit(R1, lambda m: m["inputs"][0]["spends"].update({"from": {"move": R2, "output": 0}})), "is not an earlier move"),
    ("spends_a_probe", edit(R1, lambda m: m["inputs"][0]["spends"].update({"from": {"move": B1, "output": 0}})), "is a probe"),
    ("probe_spends_a_spent_coin", spend_from(B1, 2, A1, 1), "is not unspent at this place of the list"),
    ("step_spends_a_spent_coin", spend_from(R2, 2, A2, 1), "was already spent by"),
    ("spends_value", edit(R1, lambda m: m["inputs"][0]["spends"].update(value=KAS + 1)), "spends says the value is"),
    ("spends_covenant_id", edit(R1, lambda m: m["inputs"][1]["spends"].update(covenant_id=VESTING_ID)), "spends states another covenant id"),
    ("probe_without_the_vault", spend_from(B3, 1, A2, 1), "does not spend both the vesting coin"),
    # f. fees
    ("fee_below_floor", edit(R1, lambda m: m.update(fee_floor=m["fee"] + 1)), "is below its floor"),
    ("fee_not_what_the_coins_say", edit(R2, lambda m: m.update(fee=m["fee"] + 1)), "the file says the fee is"),
    ("fees_of_probes", lambda c: c["totals"].update(fees_of_probes=c["totals"]["fees_of_probes"] + 1), "the fees of the probes sum to"),
    ("fees_of_steps", lambda c: c["totals"].update(fees_of_steps=0), "the fees of the steps sum to"),
    ("fees_above_the_cap_of_the_file", lambda c: c.update(fee_cap_total=1000), "above the cap of the file"),
    ("fees_above_the_hard_limit", fee_above_the_hard_limit, "above the hard limit of 8.00000000 KAS"),
    # g. outputs
    ("change_to_a_foreign_key", output_of(R2, 3, setter(script_public_key=FOREIGN_KEY_SCRIPT)), "its script is not the payer's"),
    ("change_to_alice", output_of(R2, 3, setter(to="alice")), "change goes only to the payer"),
    ("unknown_role", output_of(R2, 3, setter(role="deposit")), "is not one of vesting, token, change"),
    ("token_bound_to_the_vesting_id", output_of(R1, 0, lambda o: o["covenant_binding"].update(covenant_id=VESTING_ID)), "a token coin bound to another covenant id"),
    ("authorizing_input_of_another_covenant", output_of(R1, 0, lambda o: o["covenant_binding"].update(authorizing_input=0)), "carries the covenant id"),
    ("covenant_coin_of_another_value", output_of(R1, 0, setter(value=2 * KAS)), "a token coin must hold"),
    ("covenant_coin_to_a_party", output_of(R1, 0, setter(to="beneficiary")), 'a token coin names a party in "to"'),
    ("foreign_plain_output", output_of(R1, 0, setter(script_public_key=FOREIGN_KEY_SCRIPT)), "pay-to-script-hash script"),
    # h. vesting covenant id
    ("vesting_covenant_id", lambda c: c.update(vesting_covenant_id="22" * 32), "the SDK derives"),
    # i. states, releases, tokens
    ("vesting_beneficiary", state_of(R1, 2, setter(beneficiary=PAYER_KEY)), "is not the key of the party 'beneficiary'"),
    ("vesting_token_covid", state_of(R1, 2, setter(token_covid=VESTING_ID)), "is not the token's covenant id"),
    ("vesting_token_template", state_of(R2, 2, setter(token_template="33" * 32)), "is not token.template_hash"),
    ("vesting_schedule", state_of(R1, 2, setter(cliff=2000)), "cliff is 2000, the schedule of the file says 3000"),
    ("vesting_released", state_of(R2, 2, setter(released=5)), "says 5 released; at this step it is 6"),
    ("vesting_released_at_genesis", state_of(A1, 0, setter(released=1)), "says 1 released; at this step it is 0"),
    ("vesting_coin_does_not_end", lambda c: move_of(c, R3)["outputs"].insert(0, copy.deepcopy(move_of(c, R2)["outputs"][2])), "at this step the vesting coin ends"),
    ("vault_owner", state_of(R1, 1, setter(owner=TOKEN_ID)), "is not the vesting covenant id"),
    ("claim_owner_not_a_party", state_of(R1, 0, setter(owner=FOREIGN_KEY)), "is not the key of a party"),
    ("owner_scheme", state_of(R1, 0, setter(owner_scheme=1)), "this run allows only 0 (a party's key) and 4"),
    ("borrow_scheme", state_of(R1, 0, setter(borrow_scheme=1)), "borrowing must be disabled"),
    ("vault_of_nine", state_of(A2, 0, setter(amount=9)), "the vault holds 9 tokens, not 10"),
    ("tokens_made_from_nothing", state_of(R1, 0, setter(amount=4)), "spends 10 tokens and makes 11"),
    ("beneficiary_short_at_the_end", state_of(R3, 0, setter(amount=3)), "the beneficiary ends with 9 tokens"),
    ("probe_claim", state_of(B3, 0, setter(amount=3)), "claims 3 tokens for ['beneficiary']; this probe claims 4"),
    ("b4_claim_owned_by_the_beneficiary", state_of(B4, 0, setter(owner=BENEFICIARY_KEY)), "this probe claims 3 for 'payer'"),
    ("outputs_out_of_order", edit(R1, lambda m: m["outputs"].insert(0, m["outputs"].pop(1))), "here comes a token coin of 3 tokens owned by the key of 'beneficiary'"),
    ("an_output_too_many", edit(R2, lambda m: m["outputs"].append(copy.deepcopy(m["outputs"][3]))), "has 5 outputs; this move has 4"),
    ("vesting_released_in_a_probe", state_of(B1, 2, setter(released=2)), "here comes the vesting coin with 1 released"),
    ("a_token_coin_of_zero", state_of(R3, 0, setter(amount=0)), "a token coin of 0 tokens"),
    ("token_coin_not_alice_s", lambda c: c["token"]["state"].update(owner=PAYER_KEY), "not owned by Alice's key"),
    ("token_coin_contract", lambda c: c["token"]["state"].update(contract="PublicMint"), "the contract is 'PublicMint', not 'KCC20'"),
    ("vesting_coin_of_another_contract", state_of(R1, 2, setter(contract="PublicMint")), "names the contract 'PublicMint', not 'TokenVesting'"),
    ("vesting_coin_with_a_kcc20_state", state_of(R1, 2, setter(contract="KCC20")), "output 2, state: the field 'amount' is missing"),
]


class Check(Folder):
    def refused(self, change, phrase: str, parties: dict = PARTIES) -> str:
        chain = copy.deepcopy(FIXTURE)
        change(chain)
        code, printed = self.check(chain, parties)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn(phrase, printed)
        self.assertIn("Nothing was sent", printed)
        return printed

    def test_the_fixture_passes(self):
        code, printed = self.check()
        self.assertEqual(code, 0, printed)
        self.assertNotIn("REFUSED", printed)
        self.assertIn("NOT CHECKED - no --harness given, no KCC20 lock was recomputed", printed)
        self.assertIn("the 6 TokenVesting locks are NOT recomputed by this program (check l)", printed)
        self.assertIn("check l  TokenVesting locks, the verifier's report      : NOT CHECKED - no --verify-report given", printed)
        self.assertIn("EXCEPT check j: the KCC20 locks were NOT recomputed", printed)
        self.assertIn("tokens        : beneficiary 10", printed)
        self.assertIn("derived by the SDK from 'A1 vesting genesis', 1 output(s)", printed)
        self.assertNotIn("can never be sent", printed)
        self.assertNotIn("note: totals.", printed)

    def test_with_the_stand_in_harness_every_kcc20_lock_and_the_template_hash_are_compared(self):
        code, printed = self.check(more=("--harness", str(STAND_IN_FOLDER)))
        self.assertEqual(code, 0, printed)
        self.assertIn("KCC20 recomputed for 14 coins, the template hash compared", printed)
        self.assertIn("check: passed (offline). EXCEPT check l: the TokenVesting locks were NOT CHECKED (no --verify-report)", printed)

    def test_a_party_address_that_does_not_match_the_file(self):
        parties = dict(PARTIES, alice=PARTIES["beneficiary"])
        self.refused(lambda chain: None, "party 'alice': the address given does not match the file", parties)

    def test_the_parties_named_must_be_the_parties_of_the_file(self):
        parties = {name: at for name, at in PARTIES.items() if name != "beneficiary"}
        self.refused(lambda chain: None, "the command line names ['alice', 'payer']", parties)

    def test_two_parties_with_one_key(self):
        def change(chain):
            chain["parties"]["alice"] = dict(chain["parties"]["beneficiary"])
        parties = dict(PARTIES, alice=PARTIES["beneficiary"])
        self.refused(change, "parties: two parties have the same key", parties)

    def test_an_address_of_another_network_is_refused_before_anything_is_read(self):
        code, printed = run(["check", "no-such-file.json", "--party", "payer=kaspa:qqq"])
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("does not start with kaspatest:", printed)

    def test_a_missing_field_is_named_and_the_other_checks_are_not_run(self):
        chain = copy.deepcopy(FIXTURE)
        del move_of(chain, R1)["twin"]
        code, printed = self.check(chain)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("step 7 ('R1 first release'): the field 'twin' is missing", printed)
        self.assertIn("the other checks were not run", printed)

    def test_a_key_given_twice_in_the_file_is_refused(self):
        path = self.folder / "chain.json"
        path.write_text('{"format": "a", "format": "b"}', encoding="utf-8")
        code, printed = run(["check", str(path), *party_args()])
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("the key 'format' is there twice in one object", printed)

    def test_a_file_that_is_not_json(self):
        path = self.folder / "chain.json"
        path.write_text("not json", encoding="utf-8")
        code, printed = run(["check", str(path), *party_args()])
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("does not read as JSON", printed)

    def test_any_file_that_follows_the_format_is_read_extra_fields_and_all(self):
        chain = copy.deepcopy(FIXTURE)
        chain["something_new"] = {"a": 1}
        chain["funding"].pop("block_daa_score")
        for name in ("tokens_to_beneficiary", "kas_locked_at_the_end", "change_at_the_end"):
            chain["totals"].pop(name)
        move_of(chain, R1)["note"] = "for a reader"
        code, printed = self.check(chain)
        self.assertEqual(code, 0, printed)

    def test_the_words_of_a_refusal_are_the_file_s_own(self):
        # A builder's B1 fails on a lock-time line: the node's words are not "verification failed".
        chain = copy.deepcopy(FIXTURE)
        move_of(chain, B1)["refusal_expected"]["node_message_contains"] = "Unsatisfied lock time"
        move_of(chain, B1)["gate"]["refusal"]["message"] = "Unsatisfied lock time: locktime requirement not satisfied"
        code, printed = self.check(chain)
        self.assertEqual(code, 0, printed)
        self.assertIn("refused with 'Unsatisfied lock time'", printed)
        node = FakeNode()
        code, printed = self.send(node, chain)
        self.assertEqual(code, vs.EXIT_OK, printed)
        # The fake node answers with "verification failed": for B1 that is now another reason.
        other = [e["probe"] for e in self.events() if e["what"] == "probe refused for another reason"]
        self.assertEqual(other, [B1])
        self.assertIn("the answer does not contain 'Unsatisfied lock time'", printed)

    def test_a_gate_message_without_the_expected_words_is_refused(self):
        chain = copy.deepcopy(FIXTURE)
        move_of(chain, B4)["gate"]["refusal"]["message"] = "the contract said no"
        code, printed = self.check(chain)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("the gate's message 'the contract said no' does not contain 'verification failed'", printed)

    def test_the_verifier_s_report_confirms_the_vesting_locks(self):
        code, printed = self.check(more=("--verify-report", self.report_file(verify_report(FIXTURE))))
        self.assertEqual(code, 0, printed)
        self.assertIn("confirms the 6 TokenVesting locks", printed)
        self.assertNotIn("EXCEPT check l", printed)

    def test_a_verifier_s_report_that_does_not_fit_the_chain_is_refused(self):
        vesting = [(m["name"], n) for m in FIXTURE["moves"] for n, o in enumerate(m["outputs"]) if o["role"] == "vesting"]
        missing = verify_report(FIXTURE)
        missing["outputs"] = [o for o in missing["outputs"] if (o["move"], o["output"]) != vesting[0]]
        other_script = verify_report(FIXTURE)
        next(o for o in other_script["outputs"] if o["role"] == "vesting")["script_public_key"] = "aa20" + "00" * 32 + "87"
        not_ok = verify_report(FIXTURE)
        not_ok["outputs"][0]["ok"] = False
        extra = verify_report(FIXTURE)
        extra["outputs"].append({"move": R3, "output": 1, "role": "vesting", "script_public_key": "aa20" + "00" * 32 + "87", "ok": True})
        cases = (
            (verify_report(FIXTURE, format="something/1"), "not 'vesting-verify-report/1'"),
            (verify_report(FIXTURE, mismatches=1), "it counts 1 mismatches"),
            (verify_report(FIXTURE, vesting_covenant_id="22" * 32), "it is about the vesting covenant 2222"),
            (missing, f"output {vesting[0][1]} of {vesting[0][0]!r}, a vesting coin, is not in the report"),
            (other_script, "carries another script in the chain than the report confirms"),
            (not_ok, "is not ok in the report"),
            (extra, "is a change output in the chain, the report says vesting"),
        )
        for report, phrase in cases:
            with self.subTest(phrase):
                code, printed = self.check(more=("--verify-report", self.report_file(report)))
                self.assertEqual(code, vs.EXIT_CHECK, printed)
                self.assertIn(phrase, printed)

    def test_a_total_for_the_reader_that_differs_is_a_note(self):
        chain = copy.deepcopy(FIXTURE)
        chain["totals"]["tokens_to_beneficiary"] = 9
        code, printed = self.check(chain)
        self.assertEqual(code, 0, printed)
        self.assertIn("note: totals.tokens_to_beneficiary is 9 in the file; counted from the coins it is 10", printed)

    def test_usage_errors_end_with_the_check_code(self):
        code, printed = run(["check"])
        self.assertEqual(code, vs.EXIT_CHECK)
        code, printed = run(["check", "x.json", "--party", "payer"])
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("--party wants NAME=ADDRESS", printed)

    def test_the_program_has_no_option_for_a_key(self):
        for command in ("check", "send", "status"):
            more = ["--record", "x"] if command == "send" else []
            code, printed = run([command, "x.json", *party_args(), *more, "--key", "00" * 32])
            self.assertEqual(code, vs.EXIT_CHECK)
            self.assertIn("unrecognized arguments: --key", printed)
        self.assertNotIn("PrivateKey", (HERE.parent / "vesting_send.py").read_text(encoding="utf-8"))


def broken_file_test(change, phrase):
    def test(self):
        self.refused(change, phrase)
    return test


for _name, _change, _phrase in BROKEN:
    setattr(Check, f"test_refused_{_name}", broken_file_test(_change, _phrase))


class Locks(Folder):
    """The KCC20 locks, recomputed from their states by the stand-in, or by the real harness when its folder is given."""

    def test_a_state_that_does_not_give_the_lock_is_refused(self):
        chain = copy.deepcopy(FIXTURE)
        move_of(chain, R2)["outputs"][0]["script_public_key"] = move_of(chain, B3)["outputs"][0]["script_public_key"]
        code, printed = self.check(chain, more=("--harness", str(STAND_IN_FOLDER)))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("the lock recomputed from the state is", printed)

    def test_a_template_hash_other_than_the_harness_s_is_refused(self):
        chain = copy.deepcopy(FIXTURE)
        chain["token"]["template_hash"] = "44" * 32
        for move in chain["moves"]:
            for output in move["outputs"]:
                if output["role"] == "vesting":
                    output["state"]["token_template"] = "44" * 32
        code, printed = self.check(chain, more=("--harness", str(STAND_IN_FOLDER)))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("is not the harness's template hash of KCC20", printed)

    def test_a_harness_that_cannot_be_loaded_is_a_refusal(self):
        code, printed = self.check(more=("--harness", str(self.folder / "nowhere")))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("the harness named by --harness could not be loaded", printed)

    @unittest.skipUnless(HAVE_HARNESS, NO_HARNESS)
    def test_the_real_harness_recomputes_every_kcc20_lock_of_the_fixture(self):
        code, printed = self.check(more=("--harness", HARNESS))
        self.assertEqual(code, 0, printed)
        self.assertIn("KCC20 recomputed for 14 coins, the template hash compared", printed)

    @unittest.skipUnless(HAVE_HARNESS, NO_HARNESS)
    def test_the_real_harness_refuses_a_claim_whose_lock_is_another_state_s(self):
        chain = copy.deepcopy(FIXTURE)
        # R2's claim of 3 tokens carries the true lock of B3's claim of 4.
        move_of(chain, R2)["outputs"][0]["script_public_key"] = move_of(chain, B3)["outputs"][0]["script_public_key"]
        code, printed = self.check(chain, more=("--harness", HARNESS))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("step 8 ('R2 second release'), output 0: the lock recomputed from the state is", printed)


# --------------------------------------------------------------------------
# Reading the network: check --online, status, prepare
# --------------------------------------------------------------------------


class Reading(Folder):
    def test_check_online_finds_the_coins_and_builds_a1_from_the_node_s_entries(self):
        node = FakeNode(daa=START - 5000)
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, 0, printed)
        self.assertIn("on chain: the funding coin and the token coin are unspent", printed)
        self.assertIn("built   : 'A1 vesting genesis' builds for sending from the node's own entries; not submitted", printed)
        self.assertIn(f"DAA     : virtual DAA score {START - 5000}; schedule start {START} (5000 to go)", printed)
        self.assertIn(f"window  : 'B2 dated at the end': open from above {START + 1020} to below {START + 8000}, 13000 left", printed)
        self.assertEqual(node.submitted, [])

    def test_check_online_refuses_when_the_token_coin_is_not_there(self):
        node = FakeNode()
        node.remove(FIXTURE["token"]["coin"]["transaction_id"], FIXTURE["token"]["coin"]["index"])
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("are not what is unspent after any number of steps", printed)

    def test_check_online_refuses_a_token_coin_of_another_covenant_id(self):
        node = FakeNode()
        coin = FIXTURE["token"]["coin"]
        node.put(coin["transaction_id"], coin["index"], coin["value"], coin["script_public_key"], "55" * 32)
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("the token coin", printed)
        self.assertIn("covenant id 5555", printed)

    def test_check_online_refuses_a_funding_coin_of_another_value(self):
        node = FakeNode()
        funding = FIXTURE["funding"]
        node.put(funding["transaction_id"], funding["index"], funding["value"] + 1, funding["script_public_key"])
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn(f"amount {funding['value'] + 1} instead of {funding['value']}", printed)

    def test_a_node_of_another_network_is_not_read(self):
        node = FakeNode(network="mainnet")
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("Refusing: the node reports network 'mainnet', not 'testnet-10'.", printed)
        self.assertEqual(node.reads, 0)

    def test_a_node_of_another_version_is_not_read(self):
        node = FakeNode(version="2.0.9")
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("Refusing: the node reports version '2.0.9', not '2.1.0'.", printed)
        self.assertEqual(node.reads, 0)

    def test_an_unsynced_node_is_not_read(self):
        node = FakeNode(synced=False)
        code, printed = self.check(more=("--online",), node=node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("Refusing: the node is not synced.", printed)
        self.assertEqual(node.reads, 0)

    def test_status_before_anything_was_sent(self):
        node = FakeNode(daa=START - 3000)
        code, printed = run(["status", self.chain_file(), *party_args()], node)
        self.assertEqual(code, 0, printed)
        self.assertIn("no step is on chain", printed)
        self.assertIn("step  'A1 vesting genesis': no trace; 0 of 2 outputs unspent; due now", printed)
        self.assertIn(f"probe 'B1 before the cliff': no trace; due in 4021 DAA", printed)
        self.assertEqual(node.submitted, [])

    def test_status_shows_how_far_the_chain_is(self):
        node = FakeNode()
        code, printed = self.send(node)
        self.assertEqual(code, 0, printed)
        code, printed = run(["status", self.chain_file(), *party_args()], node)
        self.assertEqual(code, 0, printed)
        self.assertIn("step  'R3 the rest': on chain; 2 of 2 outputs unspent; due now", printed)
        self.assertIn("probe 'B3 more than vested': no trace", printed)
        self.assertIn("on chain: the first 5 of 5 steps are on chain", printed)
        self.assertIn("token KCC20 4 tokens of beneficiary", printed)
        self.assertIn("balance: payer", printed)
        self.assertIn("DAA     : virtual DAA score", printed)


class Prepare(Folder):
    def token_state_file(self, covenant_id: str = TOKEN_ID, state: dict | None = None) -> str:
        path = self.folder / "token-state.json"
        path.write_text(json.dumps({"covenant_id": covenant_id, "state": state or FIXTURE["token"]["state"]}), encoding="utf-8")
        return str(path)

    def prepare(self, node, *more, state_file: str | None = None) -> tuple:
        funding, coin = FIXTURE["funding"], FIXTURE["token"]["coin"]
        argv = [
            "prepare", "--payer-address", PARTIES["payer"],
            "--funding", f"{funding['transaction_id']}:{funding['index']}",
            "--token-coin", f"{coin['transaction_id']}:{coin['index']}",
            "--token-state", state_file or self.token_state_file(),
            "--harness", str(STAND_IN_FOLDER), "--out", str(self.folder / "request.json"), *more,
        ]
        return run(argv, node)

    def test_the_request_names_the_two_coins_the_score_and_the_schedule(self):
        node = FakeNode(daa=597_148_140, daa_step=0)
        code, printed = self.prepare(node)
        self.assertEqual(code, 0, printed)
        request = json.loads((self.folder / "request.json").read_text(encoding="utf-8"))
        self.assertEqual(request["format"], "vesting-launch-request/1")
        self.assertEqual((request["network"], request["node_version"]), ("testnet-10", "2.1.0"))
        self.assertEqual(request["virtual_daa_score"], 597_148_140)
        self.assertEqual(request["schedule"], {"start": 597_154_140, "cliff": 3000, "duration": 10000, "total": 10})
        self.assertEqual((request["coin_value"], request["feerate"], request["fee_cap_total"]), (KAS, 200, 8 * KAS))
        self.assertEqual(request["funding"]["transaction_id"], FIXTURE["funding"]["transaction_id"])
        self.assertEqual(request["funding"]["script_public_key"], FIXTURE["funding"]["script_public_key"])
        self.assertEqual(request["token"]["covenant_id"], TOKEN_ID)
        self.assertEqual(request["token"]["coin"]["script_public_key"], FIXTURE["token"]["coin"]["script_public_key"])
        self.assertEqual(request["token"]["state"], FIXTURE["token"]["state"])
        self.assertIn("estimate: priority bucket", printed)
        self.assertEqual(node.submitted, [])

    def test_the_lead_and_the_feerate_can_be_given(self):
        node = FakeNode(daa=1000, daa_step=0)
        code, printed = self.prepare(node, "--lead", "9000", "--feerate", "150", "--fee-cap-kas", "2")
        self.assertEqual(code, 0, printed)
        request = json.loads((self.folder / "request.json").read_text(encoding="utf-8"))
        self.assertEqual((request["schedule"]["start"], request["feerate"], request["fee_cap_total"]), (10_000, 150, 2 * KAS))

    def test_limits_of_prepare(self):
        for more, phrase in ((("--feerate", "99"), "--feerate must be at least 100"), (("--fee-cap-kas", "8.5"), "at most 8.00000000"),
                             (("--lead", "0"), "--lead must be")):
            code, printed = self.prepare(FakeNode(), *more)
            self.assertEqual(code, vs.EXIT_CHECK, printed)
            self.assertIn(phrase, printed)
        self.assertFalse((self.folder / "request.json").exists())

    def test_a_missing_token_coin_is_refused(self):
        node = FakeNode()
        node.remove(FIXTURE["token"]["coin"]["transaction_id"], FIXTURE["token"]["coin"]["index"])
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("is not among the unspent coins at the address of its state", printed)
        self.assertFalse((self.folder / "request.json").exists())

    def test_a_missing_funding_coin_is_refused(self):
        node = FakeNode()
        node.remove(FIXTURE["funding"]["transaction_id"], FIXTURE["funding"]["index"])
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("is not among the unspent coins at the payer's address", printed)

    def test_a_funding_coin_above_twenty_kas_is_refused(self):
        node = FakeNode()
        funding = FIXTURE["funding"]
        node.put(funding["transaction_id"], funding["index"], 21 * KAS, funding["script_public_key"])
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("the run allows at most 20.00000000", printed)

    def test_a_token_coin_of_another_covenant_id_or_value_is_refused(self):
        coin = FIXTURE["token"]["coin"]
        node = FakeNode()
        node.put(coin["transaction_id"], coin["index"], coin["value"], coin["script_public_key"], "55" * 32)
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("the token coin carries the covenant id 5555", printed)
        node = FakeNode()
        node.put(coin["transaction_id"], coin["index"], 2 * KAS, coin["script_public_key"], TOKEN_ID)
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("the token coin holds 2.00000000 KAS", printed)

    def test_a_token_state_that_is_not_alice_s_ten_tokens_is_refused(self):
        state = dict(FIXTURE["token"]["state"], amount=3)
        code, printed = self.prepare(FakeNode(), state_file=self.token_state_file(state=state))
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("it must hold 10", printed)

    def test_an_existing_request_is_not_overwritten(self):
        (self.folder / "request.json").write_text("keep", encoding="utf-8")
        node = FakeNode()
        code, printed = self.prepare(node)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("Refusing to overwrite", printed)
        self.assertEqual((self.folder / "request.json").read_text(encoding="utf-8"), "keep")
        self.assertEqual(node.connections, 0)

    def test_a_payer_address_that_does_not_pay_to_a_key_is_refused(self):
        node = FakeNode()
        argv = ["prepare", "--payer-address", address(FIXTURE["token"]["coin"]["script_public_key"]).replace("kaspatest:", "kaspatest:"),
                "--funding", "00" * 32 + ":0", "--token-coin", "00" * 32 + ":1", "--token-state", self.token_state_file(),
                "--harness", str(STAND_IN_FOLDER), "--out", str(self.folder / "request.json")]
        code, printed = run(argv, node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("does not pay to an x-only public key", printed)
        self.assertEqual(node.connections, 0)

    def test_a_node_of_another_version_is_refused(self):
        code, printed = self.prepare(FakeNode(version="2.2.0"))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("not '2.1.0'", printed)
        self.assertFalse((self.folder / "request.json").exists())


# --------------------------------------------------------------------------
# send
# --------------------------------------------------------------------------


class Send(Folder):
    def test_the_whole_run(self):
        node = FakeNode()
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertEqual(node.submitted, SENT_IN_A_WHOLE_RUN)
        self.assertIn("finished: 4 refused as expected; 5 sent", printed)
        # What was handed over carries the lock time and what an id does not cover, as the file has it.
        sent = [move for move in FIXTURE["moves"] if move["transaction_id"] in node.submitted]
        for shown, move in zip(node.handed, sent):
            self.assertEqual((shown["lockTime"], shown["mass"]), (move["lock_time"], move["mass"]))
            for have, want in zip(shown["inputs"], move["inputs"]):
                self.assertEqual((have["signatureScript"], have["computeBudget"], have["sequence"]),
                                 (want["signature_script"], want["compute_budget"], 0))
        events = self.events()
        told = [event["what"] for event in events]
        self.assertEqual(told[:2], ["run started", "balances before"])
        self.assertEqual(told[-2:], ["balances after", "run finished"])
        self.assertEqual(told.count("step accepted"), 5)
        self.assertEqual(told.count("step seen"), 5)
        self.assertEqual(told.count("probe refused"), 4)
        refused = [event for event in events if event["what"] == "probe refused"][0]
        self.assertEqual(refused["answer"], f"Rejected transaction {IDS[B1]}: {SCRIPT_REFUSAL}")
        b2 = [event for event in events if event["what"] == "probe refused" and event["probe"] == B2][0]
        self.assertIn("transaction input #0 is not finalized", b2["answer"])
        self.assertLess(b2["daa_score"], START + 8000)
        self.assertNotIn("probe not sent", told)
        # Every hand-over is recorded with the DAA score it was made at, above the move's send_after_daa.
        moves = {move["transaction_id"]: move for move in FIXTURE["moves"]}
        submitting = [event for event in events if event["what"].endswith("submitting")]
        self.assertEqual([event["transaction_id"] for event in submitting], SENT_IN_A_WHOLE_RUN)
        for event in submitting:
            self.assertGreater(event["daa_score"], moves[event["transaction_id"]]["send_after_daa"])
        for event in events:
            self.assertEqual((event["node"], event["node_version"]), (node.url, "2.1.0"))
            self.assertRegex(event["time_utc"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        after = events[-2]["balances"]
        self.assertEqual(after["payer"]["sompi"], FIXTURE["totals"]["change_at_the_end"])
        self.assertEqual(after["beneficiary"]["sompi"], 0)  # the claims are covenant coins, not at the plain address
        summary = self.summaries()[0]
        self.assertEqual(summary["exit_code"], 0)
        self.assertEqual([(m["name"], m["outcome"]) for m in summary["moves"]][2:4],
                         [(B1, "refused as expected"), (B2, "refused as expected")])
        self.assertNotIn("5a" * 9, "".join(p.read_text(encoding="utf-8") for p in (self.folder / "record").glob("run-*")))

    def test_the_program_waits_for_the_daa_score_of_each_move(self):
        node = FakeNode(daa=START - 6000, daa_step=37)
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        waits = [event for event in self.events() if event["what"] == "waiting for DAA"]
        self.assertEqual([event["move"] for event in waits], [B1, B3, R2, R3])
        self.assertIn(f"probe 'B1 before the cliff': waiting for a DAA score above {START + 1020}", printed)
        for txid, score in zip(node.submitted, node.daa_at_submission):
            move = next(m for m in FIXTURE["moves"] if m["transaction_id"] == txid)
            self.assertGreater(score, move["send_after_daa"])

    def test_a_wait_longer_than_the_rate_predicts_stops_the_run(self):
        node = FakeNode(daa=START + 500, daa_step=0)  # the score stands still before B1's time
        code, printed = self.send(node, timing=(("daa_slack_seconds", 0.05), ("nominal_daa_rate", 10**9), ("daa_poll_seconds", 0.005)))
        self.assertEqual(code, vs.EXIT_TIMING, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])
        self.assertIn("not above", printed)
        self.assertEqual([e["what"] for e in self.events()][-2:], ["DAA wait given up", "run stopped"])
        self.assertEqual(self.summaries()[0]["exit_code"], vs.EXIT_TIMING)

    def b2_with_an_open_window(self) -> dict:
        """The other reading of FORMAT.md: B2 may go from B1's time on, until its send_before_daa.

        `check` refuses such a file (it reads the rule for send_after_daa literally), so these tests
        put its rule for the moves aside; everything else of `send` is as in a real run.
        """
        chain = copy.deepcopy(FIXTURE)
        move_of(chain, B2)["send_after_daa"] = move_of(chain, B1)["send_after_daa"]
        return chain

    def test_b2_is_sent_inside_its_window_and_refused_as_not_finalized(self):
        node = FakeNode()
        node.how[IDS[B2]] = "accept"  # the fake node still refuses it: its lock time has not passed
        with mock.patch.object(vs, "check_moves", lambda data: []):
            code, printed = self.send(node, self.b2_with_an_open_window())
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertEqual(node.submitted, [IDS[name] for name in (A1, A2, B1, B2, B3, B4, R1, R2, R3)])
        refused = [e for e in self.events() if e["what"] == "probe refused" and e["probe"] == B2][0]
        self.assertIn("transaction input #0 is not finalized", refused["answer"])
        self.assertLess(refused["daa_score"], START + 8000)

    def test_b2_is_not_sent_once_its_window_has_passed(self):
        node = FakeNode(daa=START + 8500)
        with mock.patch.object(vs, "check_moves", lambda data: []):
            code, printed = self.send(node, self.b2_with_an_open_window())
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertNotIn(IDS[B2], node.submitted)
        self.assertIn("probe 'B2 dated at the end': NOT SENT (window passed: the DAA score", printed)
        reasons = [e["reason"] for e in self.events() if e["what"] == "probe not sent"]
        self.assertTrue(reasons[0].startswith("window passed"), reasons)

    def test_b2_whose_window_closes_during_the_wait_is_not_sent(self):
        chain = self.b2_with_an_open_window()
        node = FakeNode(daa=START + 7990, daa_step=5)
        move_of(chain, B2)["send_after_daa"] = START + 7995  # not reached before START + 8000 at the first look
        with mock.patch.object(vs, "check_moves", lambda data: []):
            code, printed = self.send(node, chain)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertNotIn(IDS[B2], node.submitted)

    def test_the_window_is_checked_again_right_before_a_hand_over(self):
        move = {"send_after_daa": 100, "send_before_daa": 200}

        async def score_at(daa):
            node = FakeNode(daa=daa, daa_step=0)
            return await vs.ready_to_hand_over(node, move)

        self.assertEqual(asyncio.run(score_at(150)), 150)
        with self.assertRaises(vs.WindowPassed):
            asyncio.run(score_at(200))
        with self.assertRaises(vs.NotYet):
            asyncio.run(score_at(100))

    def test_without_the_flag_nothing_is_sent(self):
        node = FakeNode()
        code, printed = self.send(node, flag=False)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("--yes-send-to-testnet-10", printed)
        self.assertEqual((node.submitted, node.connections), ([], 0))
        self.assertFalse((self.folder / "record").exists())

    def test_send_does_not_start_without_the_verifier_s_report(self):
        node = FakeNode()
        code, printed = self.send(node, verified=False)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("`send` needs --verify-report FILE", printed)
        self.assertEqual((node.submitted, node.connections), ([], 0))
        code, printed = self.send(node, report=verify_report(FIXTURE, mismatches=2))
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertEqual((node.submitted, node.connections), ([], 0))

    def test_a_lock_file_keeps_a_second_run_out_and_goes_at_the_end(self):
        node = FakeNode()
        (self.folder / "record").mkdir()
        (self.folder / "record" / "send.lock").write_text("{}", encoding="utf-8")
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("send.lock exists", printed)
        self.assertEqual((node.submitted, node.connections), ([], 0))
        (self.folder / "record" / "send.lock").unlink()
        node.how[IDS[R1]] = "refuse"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_REFUSED, printed)
        self.assertFalse((self.folder / "record" / "send.lock").exists())

    def test_a_refusal_under_another_id_is_not_taken_for_this_move_s(self):
        node = FakeNode()
        node.how[IDS[A2]] = "rejected-under-another-id"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_NO_ANSWER, printed)
        self.assertIn("NO ANSWER", printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])

    def test_a_probe_s_words_are_read_after_its_own_id_only(self):
        node = FakeNode()
        node.how[IDS[B3]] = "raw:verification failed (said before) -> Rejected transaction TXID: fee too low"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertIn("1 refused for another reason", printed)

    def test_a_reconnect_is_recorded_and_every_line_names_the_node_of_its_moment(self):
        node = FakeNode()

        def drop_after_a1(txid):
            if txid == IDS[A1]:
                node.failing_reads.append(ConnectionError("dropped"))
                node.url = "wss://other.example"

        node.after_accept = drop_after_a1
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        events = self.events()
        reconnected = [e for e in events if e["what"] == "reconnected"]
        self.assertEqual(len(reconnected), 1)
        self.assertEqual(reconnected[0]["server_info"]["networkId"], "testnet-10")
        self.assertEqual(reconnected[0]["node"], "wss://other.example")
        self.assertEqual(events[0]["node"], "wss://fake.example")

    def test_send_does_not_start_without_a_harness(self):
        node = FakeNode()
        code, printed = self.send(node, harness=False)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertIn("`send` needs --harness", printed)
        self.assertEqual((node.submitted, node.connections), ([], 0))

    def test_a_file_that_fails_a_check_is_not_sent(self):
        chain = copy.deepcopy(FIXTURE)
        move_of(chain, R3)["outputs"][1]["script_public_key"] = FOREIGN_KEY_SCRIPT
        node = FakeNode()
        code, printed = self.send(node, chain)
        self.assertEqual(code, vs.EXIT_CHECK)
        self.assertEqual((node.submitted, node.connections), ([], 0))

    def test_another_network_an_unsynced_node_and_another_version_are_handed_nothing(self):
        for node, phrase in ((FakeNode(network="mainnet"), "network 'mainnet'"), (FakeNode(synced=False), "not synced"),
                             (FakeNode(version="2.0.0"), "version '2.0.0'")):
            code, printed = self.send(node)
            self.assertEqual(code, vs.EXIT_CHECK, printed)
            self.assertIn(phrase, printed)
            self.assertEqual(node.submitted, [])

    def test_a_node_that_turns_into_another_version_is_handed_nothing_more(self):
        node = FakeNode()
        node.after_accept = lambda txid: setattr(node, "version", "2.2.0") if txid == IDS[A2] else None
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_NODE, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])

    def test_a_forbidden_move_accepted_stops_the_run_at_once(self):
        node = FakeNode()
        node.how[IDS[B3]] = "accept"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_FORBIDDEN_ACCEPTED, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2], IDS[B1], IDS[B2], IDS[B3]])
        self.assertIn("FORBIDDEN MOVE ACCEPTED", printed)
        self.assertEqual([e["what"] for e in self.events()][-2:], ["probe accepted", "run stopped"])
        # No later run with this record starts, with or without the flag that hands a step over again.
        for more in ((), ("--again-after-a-stop",)):
            code, printed = self.send(node, more=more)
            self.assertEqual(code, vs.EXIT_FORBIDDEN_ACCEPTED, printed)
            self.assertIn("No flag overrides this", printed)
        self.assertEqual(node.submitted.count(IDS[B3]), 1)

    def test_a_refused_step_stops_the_run(self):
        node = FakeNode()
        node.how[IDS[R1]] = "refuse"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_REFUSED, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2], IDS[B1], IDS[B2], IDS[B3], IDS[B4], IDS[R1]])
        refused = [event for event in self.events() if event["what"] == "step refused"]
        self.assertEqual(refused[0]["answer"], f"Rejected transaction {IDS[R1]}: {SCRIPT_REFUSAL}")
        last = self.events()[-1]
        self.assertEqual((last["what"], last["exit_code"]), ("run stopped", vs.EXIT_REFUSED))
        summary = self.summaries()[0]
        self.assertEqual(summary["exit_code"], vs.EXIT_REFUSED)
        self.assertEqual(summary["moves"][-1], {"name": R1, "kind": "step", "transaction_id": IDS[R1], "outcome": "stopped, exit code 2"})

    def test_a_probe_refused_for_another_reason_is_recorded_and_the_run_goes_on(self):
        node = FakeNode()
        node.how[IDS[B1]] = "refuse-fee"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertEqual(node.submitted, SENT_IN_A_WHOLE_RUN)
        self.assertIn("REFUSED FOR ANOTHER REASON - the answer does not contain 'verification failed'", printed)
        other = [event for event in self.events() if event["what"] == "probe refused for another reason"]
        self.assertEqual(other[0]["answer"], f"Rejected transaction {IDS[B1]}: {FEE_REFUSAL}")
        self.assertIn("1 refused for another reason", printed)

    def test_a_probe_whose_coin_is_already_spent_stops_the_run(self):
        node = FakeNode()
        vault = (IDS[A2], 0)
        # Once B1's time has come, the vault is gone: spent by something this file does not know.
        node.on_question = lambda: node.remove(*vault) if node.daa > START + 1000 else None
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_INCONSISTENT, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])
        self.assertIn("STOP at probe 'B1 before the cliff': its inputs are not unspent on chain as expected", printed)

    def test_an_interrupt_at_a_probe_hand_over_ends_the_run_and_blocks_the_next(self):
        node = FakeNode()
        submit = node.submit_transaction

        async def cancelled_at_b3(request):
            if request["transaction"].to_dict()["id"] == IDS[B3]:
                node.submitted.append(IDS[B3])
                raise asyncio.CancelledError()
            return await submit(request)

        node.submit_transaction = cancelled_at_b3
        code, printed = self.send(node)
        self.assertEqual(code, 130, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2], IDS[B1], IDS[B2], IDS[B3]])
        last = self.events()[-2]
        self.assertEqual((last["what"], last["transaction_id"]), ("probe interrupted", IDS[B3]))
        self.assertEqual(self.events()[-1]["what"], "run interrupted")
        self.assertIn(f"transaction {IDS[B3]}, was being handed over: it may or may not have reached the node", printed)
        self.assertEqual(self.summaries()[0]["exit_code"], 130)
        node.submit_transaction = submit
        for more in ((), ("--again-after-a-stop",)):
            code, printed = self.send(node, more=more)
            self.assertEqual(code, vs.EXIT_NO_ANSWER, printed)
            self.assertIn("'probe handed over, outcome unknown' for ['B3 more than vested']", printed)
        self.assertEqual(node.submitted.count(IDS[B3]), 1)

    def test_an_interrupt_during_a_step_hand_over_is_recorded_and_the_next_run_does_not_resend(self):
        node = FakeNode()
        submit = node.submit_transaction

        async def slow_answer_and_a_cancel(request):
            if request["transaction"].to_dict()["id"] == IDS[R1]:
                answer = await submit(request)  # the node takes it
                asyncio.get_running_loop().call_later(0.01, asyncio.current_task().cancel)
                await asyncio.sleep(5)
                return answer
            return await submit(request)

        node.submit_transaction = slow_answer_and_a_cancel
        code, printed = self.send(node)
        self.assertEqual(code, 130, printed)
        last = self.events()[-2]
        self.assertEqual((last["what"], last["step"], last["transaction_id"]), ("step interrupted", R1, IDS[R1]))
        self.assertEqual(self.events()[-1]["what"], "run interrupted")
        self.assertIn("Run `status`", printed)
        node.submit_transaction = submit
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertEqual(node.submitted.count(IDS[R1]), 1)
        self.assertIn("step 'R1 first release': already done, not sent", printed)
        # The probes before R1 were refused in the first run: not sent again.
        for name in (B1, B2, B3, B4):
            self.assertEqual(node.submitted.count(IDS[name]), 1)

    def test_an_interrupt_during_a_wait_ends_the_run_and_the_next_run_goes_on(self):
        node = FakeNode()

        def interrupt_once():
            if node.daa > START + 2000 and not node.interrupted:
                node.interrupted = True
                asyncio.current_task().cancel()

        node.interrupted = False
        node.on_question = interrupt_once
        code, printed = self.send(node)
        self.assertEqual(code, 130, printed)
        self.assertIn("Interrupted. Nothing was being handed over at that moment.", printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2], IDS[B1], IDS[B2]])
        self.assertEqual(self.events()[-1]["what"], "run interrupted")
        self.assertEqual(self.summaries()[0]["exit_code"], 130)
        node.on_question = None
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertEqual(node.submitted, SENT_IN_A_WHOLE_RUN)  # nothing twice: B1 was refused in the first run

    def test_a_step_whose_outcome_never_reached_the_record_is_not_handed_over_again_by_itself(self):
        node = FakeNode()
        submit = node.submit_transaction

        async def die(request):
            if request["transaction"].to_dict()["id"] == IDS[R2] and not node.dead:
                node.dead = True
                node.submitted.append(IDS[R2])
                raise SystemExit(9)
            return await submit(request)

        node.dead = False
        node.submit_transaction = die
        with self.assertRaises(SystemExit):
            self.send(node)
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("stopped at 'R2 second release' (step handed over, outcome unknown)", printed)
        code, printed = self.send(node, more=("--again-after-a-stop",))
        self.assertEqual(code, vs.EXIT_OK, printed)
        self.assertIn("HANDED OVER AGAIN by the operator's decision", printed)
        self.assertEqual(node.submitted.count(IDS[R2]), 2)

    def test_after_a_stop_nothing_is_sent_twice_and_probes_the_chain_is_past_are_not_sent(self):
        node = FakeNode()
        node.how[IDS[R2]] = "refuse"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_REFUSED, printed)
        # Without the operator's word the refused step is not handed over again.
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_CHECK, printed)
        self.assertIn("stopped at 'R2 second release' (step refused)", printed)
        del node.how[IDS[R2]]
        code, printed = self.send(node, more=("--again-after-a-stop",))
        self.assertEqual(code, vs.EXIT_OK, printed)
        for name in (A1, A2, R1):
            self.assertEqual(node.submitted.count(IDS[name]), 1, name)
        for name in (B1, B2, B3, B4):
            self.assertEqual(node.submitted.count(IDS[name]), 1, name)
        self.assertEqual(node.submitted.count(IDS[R2]), 2)
        self.assertIn("NOT SENT (a later step is on chain already", printed)

    def test_a_step_accepted_but_not_seen_is_waited_for_and_never_sent_again(self):
        node = FakeNode()
        node.how[IDS[A2]] = "accept-hidden"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_NOT_SEEN, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_NOT_SEEN, printed)
        self.assertIn("an earlier run recorded it as accepted; not sent again", printed)
        self.assertEqual(node.submitted.count(IDS[A2]), 1)

    def test_a_lost_answer_without_outputs_stops_the_run(self):
        node = FakeNode()
        node.how[IDS[A1]] = "lost"
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_NO_ANSWER, printed)
        self.assertEqual(node.submitted, [IDS[A1]])
        self.assertEqual([e["what"] for e in self.events()][-2:], ["step no answer", "run stopped"])

    def test_a_record_that_cannot_be_written_stops_before_the_next_hand_over(self):
        node = FakeNode()
        real = vs.Record.write

        def failing(record, what, **fields):
            if what == "probe submitting":
                raise OSError(28, "No space left on device")
            return real(record, what, **fields)

        with mock.patch.object(vs.Record, "write", failing):
            code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_RECORD, printed)
        self.assertEqual(node.submitted, [IDS[A1], IDS[A2]])
        self.assertIn("RECORD NOT WRITTEN", printed)


class TheBuilderSChains(Folder):
    """The builder's self-test chains: real signatures, made-up keys. Skipped where the files are not."""

    def each(self):
        found = [path for path in BUILDER_CHAINS if path.is_file()]
        if not found:
            self.skipTest("the builder's self-test chains are not there")
        for path in found:
            chain = json.loads(path.read_text(encoding="utf-8"))
            yield path, chain, {name: address(party["script_public_key"]) for name, party in chain["parties"].items()}

    def test_they_pass_the_offline_checks(self):
        for path, chain, parties in self.each():
            with self.subTest(path.name):
                more = ("--harness", HARNESS) if HAVE_HARNESS else ()
                code, printed = self.check(chain, parties, more=more)
                self.assertEqual(code, 0, printed)
                self.assertNotIn("REFUSED", printed)

    def test_they_run_through_against_the_fake_node(self):
        for path, chain, parties in self.each():
            with self.subTest(path.name):
                folder = Path(tempfile.mkdtemp(prefix="stand-in-harness-", dir=self.folder))
                (folder / "kcc20_harness.py").write_text("# a stand-in\n", encoding="utf-8")
                STAND_INS[folder] = StandInHarness(chain)
                node = FakeNode(chain, daa=chain["schedule"]["start"] - 6000)
                for move in chain["moves"]:
                    if move["kind"] == "probe":  # the node answers each probe with the words its file expects
                        node.how[move["transaction_id"]] = "say:" + move["refusal_expected"]["node_message_contains"]
                code, printed = self.send(node, chain, record=f"record-{path.stem}", parties=parties, stand_in=folder)
                self.assertEqual(code, vs.EXIT_OK, printed)
                self.assertEqual(node.submitted, [m["transaction_id"] for m in chain["moves"]])
                self.assertIn("finished: 4 refused as expected; 5 sent", printed)
                b2 = [e for e in self.events(f"record-{path.stem}") if e["what"] == "probe refused" and e["probe"] == B2][0]
                self.assertIn("is not finalized", b2["answer"])


if __name__ == "__main__":
    unittest.main()
