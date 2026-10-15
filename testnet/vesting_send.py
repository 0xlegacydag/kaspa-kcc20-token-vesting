"""Check a prepared chain of vesting transactions and send it to Kaspa testnet-10.

TEST NETWORK ONLY. The program accepts no other network and no node version
other than 2.1.0, and it holds no keys: every signature is already inside the
chain file, made by the builder. See FORMAT.md for the two files that pass
between the builder and this program, and README.md for the order of a run.

    python vesting_send.py prepare --payer-address kaspatest:... --funding TXID:INDEX \\
        --token-coin TXID:INDEX --token-state FILE --harness DIR --out request.json
    python vesting_send.py check chain.json --party payer=kaspatest:... --party alice=... --party beneficiary=... --harness DIR
    python vesting_send.py send chain.json --party ... --harness DIR --record DIR --yes-send-to-testnet-10
    python vesting_send.py status chain.json --party ...

`prepare`, `check --online` and `status` only read the network. `send` is the
only command that submits anything, and only with its flag and with a harness
(`--harness`, or KCC20_HARNESS_DIR): the folder of `kcc20_harness.py`, by which
the lock of every KCC20 coin is recomputed from its state. The locks of the
TokenVesting coins are NOT recomputed by this program; another tool checks them.

The moves of the file are sent in their order. Before each one the program
waits until the node's virtual DAA score is above the move's `send_after_daa`.
A step must be accepted; a probe must be refused.

Exit codes: 0 finished; 1 usage or check failure; 2 a step was refused;
3 no answer to a submission; 4 submitted but not seen; 5 a forbidden move was
accepted; 6 the picture on chain does not fit the file; 7 the node could not
be asked or read after the checks had passed (nothing was submitted in that
move); 8 the record could not be written (nothing more was submitted);
9 the DAA score did not reach a move's time within the allowed wait;
130 interrupted from the keyboard.
"""

import argparse
import asyncio
import datetime
import hashlib
import importlib.util
import json
import math
import os
import sys
import time
from pathlib import Path

from kaspa import (
    Address,
    CovenantBinding,
    GenesisCovenantGroup,
    Hash,
    Resolver,
    RpcClient,
    ScriptPublicKey,
    Transaction,
    TransactionInput,
    TransactionOutpoint,
    TransactionOutput,
    UtxoEntryReference,
    address_from_script_public_key,
    pay_to_address_script,
)

# --------------------------------------------------------------------------
# Constants. None of them can be changed from the command line.
# --------------------------------------------------------------------------

NETWORK_ID = "testnet-10"
NETWORK_TYPE = "testnet"
ADDRESS_PREFIX = "kaspatest:"
NODE_VERSION = "2.1.0"

REQUEST_FORMAT = "vesting-launch-request/1"
CHAIN_FORMAT = "vesting-launch-chain/1"

SOMPI_PER_KAS = 100_000_000

# Hard limits of a run (FORMAT.md, "Limits the sender enforces").
MAX_FUNDING = 20 * SOMPI_PER_KAS  # the one plain coin the run starts from
MAX_TOTAL_FEES = 8 * SOMPI_PER_KAS  # sum of the fees of every move, steps and probes
COIN_VALUE = 1 * SOMPI_PER_KAS  # value of every new covenant coin
MAX_STEPS = 10
MAX_PROBES = 10
MAX_LOCK_TIME = 500_000_000_000  # below it a lock time is a DAA score

MIN_FEERATE = 100  # sompi per gram: the least a default node relays for
DEFAULT_FEERATE = 200
DEFAULT_LEAD = 6000
# Bounds for the fields a transaction id does not cover. They only catch nonsense: whether the
# numbers are right is for a node to say.
MAX_COMPUTE_BUDGET = 1_000  # per input
MAX_MASS = 500_000

PARTY_NAMES = ("payer", "alice", "beneficiary")
ZERO_SUBNETWORK = "00" * 20

# The schedule of the run.
CLIFF = 3000
DURATION = 10000
TOTAL = 10
SEND_MARGIN = 20  # send_after_daa = lock_time + 20
WINDOW_MARGIN = 2000  # send_before_daa = lock_time - 2000

LAUNCH_CHECKS = ("check_vesting_genesis", "check_parameters", "check_vault")

# The moves of the run in their order: (name, kind, lock time as an offset from schedule.start or
# None for a lock time of 0, the kind of refusal a probe expects).
MOVES = (
    ("A1 vesting genesis", "step", None, None),
    ("A2 fund the vault", "step", None, None),
    ("B1 before the cliff", "probe", 1000, "scripts"),
    ("B2 dated at the end", "probe", DURATION, "finality"),
    ("B3 more than vested", "probe", CLIFF, "scripts"),
    ("B4 to another key", "probe", CLIFF, "scripts"),
    ("R1 first release", "step", CLIFF, None),
    ("R2 second release", "step", 6500, None),
    ("R3 the rest", "step", DURATION, None),
)
GENESIS = "A1 vesting genesis"
VAULT_MOVE = "A2 fund the vault"
B1, B2 = "B1 before the cliff", "B2 dated at the end"
FIRST_RELEASE = "R1 first release"
# The outputs of every move, in their order: ("vesting", released), ("claim", tokens, party whose key owns them),
# ("vault", tokens owned by the vesting covenant), ("change",). A probe has the shape of its twin.
OUTPUTS = {
    "A1 vesting genesis": (("vesting", 0), ("change",)),
    "A2 fund the vault": (("vault", 10), ("change",)),
    "B1 before the cliff": (("claim", 1, "beneficiary"), ("vault", 9), ("vesting", 1), ("change",)),
    "B2 dated at the end": (("claim", 10, "beneficiary"), ("change",)),
    "B3 more than vested": (("claim", 4, "beneficiary"), ("vault", 6), ("vesting", 4), ("change",)),
    "B4 to another key": (("claim", 3, "payer"), ("vault", 7), ("vesting", 3), ("change",)),
    "R1 first release": (("claim", 3, "beneficiary"), ("vault", 7), ("vesting", 3), ("change",)),
    "R2 second release": (("claim", 3, "beneficiary"), ("vault", 4), ("vesting", 6), ("change",)),
    "R3 the rest": (("claim", 4, "beneficiary"), ("change",)),
}
END_CLAIMS = [3, 3, 4]  # the only covenant coins of the chain left unspent: the beneficiary's three token coins
# The words FORMAT.md names for each kind of refusal. Only for a reader: `send` takes the words of
# each probe's own refusal_expected.node_message_contains, and `check` accepts any non-empty text.
# What each probe claims: (tokens, the party whose key owns the claim).
CLAIMS = {
    "B1 before the cliff": (1, "beneficiary"),
    "B2 dated at the end": (10, "beneficiary"),
    "B3 more than vested": (4, "beneficiary"),
    "B4 to another key": (3, "payer"),
}
# `released` of the vesting coin a step makes; None: the step makes no vesting coin (the coin ends).
RELEASED = {"A1 vesting genesis": 0, "A2 fund the vault": 0, "R1 first release": 3, "R2 second release": 6, "R3 the rest": None}
REFUSAL_TEXT = {"scripts": "verification failed", "finality": "is not finalized"}
REFUSAL_LAYER = {"scripts": "scripts", "finality": "consensus"}

OWNER_KEY = 0  # KCC20 owner scheme: the owner is an x-only key (Schnorr)
OWNER_COVENANT = 4  # KCC20 owner scheme: the owner is a covenant id (here the vesting covenant)

REFUSAL_MARK = "Rejected transaction"  # how a node's own refusal begins

EXIT_OK = 0
EXIT_CHECK = 1
EXIT_REFUSED = 2
EXIT_NO_ANSWER = 3
EXIT_NOT_SEEN = 4
EXIT_FORBIDDEN_ACCEPTED = 5
EXIT_INCONSISTENT = 6
EXIT_NODE = 7
EXIT_RECORD = 8
EXIT_TIMING = 9


class Timing:
    """Waiting times. The tests replace them; the command line cannot."""

    poll_seconds = 1.0
    seen_polls = 120  # after an accepted submission: how long to look for the outputs
    lost_polls = 60  # after a lost answer: how long to look for the outputs
    read_attempts = 4
    read_pause_seconds = 2.0
    connect_timeout = 60
    answer_timeout = 60  # for one question to the node; a submission without an answer by then is a lost answer
    daa_poll_seconds = 5.0  # how often the DAA score is read while waiting for a move's time
    progress_seconds = 30.0  # how often the wait is told on screen
    daa_slack_seconds = 600.0  # the wait may exceed what the DAA rate predicts by this much
    nominal_daa_rate = 10.0  # DAA per second of testnet-10, used until a rate has been measured
    rate_min_seconds = 20.0  # a measured rate needs readings at least this far apart
    not_yet_rounds = 3  # how often a node behind the one asked before is waited for again
    clock = staticmethod(time.monotonic)


class Stop(Exception):
    """The program ends here with this exit code and this last line."""

    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message
        self.said = False  # whether the last line is on screen already


class WrongNode(Stop):
    """The node is not a synced testnet-10 node of version 2.1.0."""

    def __init__(self, message: str):
        super().__init__(EXIT_CHECK, message)


class NoAnswer(Exception):
    """A submission ended without the node's answer: the node may have taken the transaction."""


class NotYet(Exception):
    """Right before a hand-over the node's virtual DAA score is not above the move's send_after_daa."""


class WindowPassed(Exception):
    """Right before a hand-over the node's virtual DAA score is at or above the move's send_before_daa."""


INTERRUPTED: dict = {}  # filled when an interrupt lands on a hand-over: which transaction it was
CURRENT_RECORD = None  # the record of the `send` under way, for events met deep down (a reconnect)


def say(line: str = "") -> None:
    print(line, flush=True)


def kas(sompi: int) -> str:
    return f"{sompi / SOMPI_PER_KAS:.8f}"


def utc_now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# --------------------------------------------------------------------------
# The shape of the chain file
# --------------------------------------------------------------------------

HEX_DIGITS = set("0123456789abcdef")


def is_hex(value, size: int | None = None) -> bool:
    """Lowercase hex of whole bytes; `size` is the exact number of bytes when given."""
    if not isinstance(value, str) or len(value) % 2 or not set(value) <= HEX_DIGITS:
        return False
    return size is None or len(value) == 2 * size


def is_uint(value, bits: int) -> bool:
    return type(value) is int and 0 <= value < 2**bits


KINDS = {
    "text": (lambda v: isinstance(v, str), "a text"),
    "bool": (lambda v: isinstance(v, bool), "true or false"),
    "u8": (lambda v: is_uint(v, 8), "a whole number from 0 to 255"),
    "u16": (lambda v: is_uint(v, 16), "a whole number from 0 to 65535"),
    "u32": (lambda v: is_uint(v, 32), "a whole number that fits 32 bits"),
    "u64": (lambda v: is_uint(v, 64), "a whole number that fits 64 bits"),
    "hex": (is_hex, "lowercase hex"),
    "hex20": (lambda v: is_hex(v, 20), "40 lowercase hex characters"),
    "hex32": (lambda v: is_hex(v, 32), "64 lowercase hex characters"),
    "object": (lambda v: isinstance(v, dict), "an object"),
    "list": (lambda v: isinstance(v, list), "a list"),
    "any": (lambda v: True, "anything"),
}

TOP_SHAPE = {
    "format": "text",
    "network": "text",
    "built_at_utc": "text",
    "nothing_was_sent": "bool",
    "coin_value": "u64",
    "feerate": "u64",
    "fee_rule": "text",
    "fee_cap_total": "u64",
    "schedule": "object",
    "funding": "object",
    "token": "object",
    "vesting_covenant_id": "hex32",
    "parties": "object",
    "launch_checks": "object",
    "moves": "list",
    "totals": "object",
}
SCHEDULE_SHAPE = {"start": "u64", "cliff": "u64", "duration": "u64", "total": "u64"}
COIN_SHAPE = {
    "transaction_id": "hex32",
    "index": "u32",
    "value": "u64",
    "script_public_key_version": "u16",
    "script_public_key": "hex",
}
FUNDING_SHAPE = {**COIN_SHAPE, "owner": "text"}
TOKEN_SHAPE = {"covenant_id": "hex32", "template_hash": "hex32", "coin": "object", "state": "object"}
PARTY_SHAPE = {"x_only_public_key": "hex32", "script_public_key": "hex"}
TOTALS_SHAPE = {"fees_of_steps": "u64", "fees_of_probes": "u64"}
TRANSACTION_SHAPE = {
    "name": "text",
    "kind": "text",
    "what": "text",
    "expect": "text",
    "transaction_id": "hex32",
    "version": "u16",
    "lock_time": "u64",
    "send_after_daa": "u64",
    "send_before_daa": "u64?",
    "judged_at_daa": "u64",
    "subnetwork_id": "hex20",
    "gas": "u64",
    "payload": "hex",
    "mass": "u64",
    "inputs": "list",
    "outputs": "list",
    "fee": "u64",
    "fee_floor": "u64",
    "masses": "object",
    "gate": "object",
    "refusal_expected": "object?",
    "twin": "object?",
}
INPUT_SHAPE = {
    "transaction_id": "hex32",
    "index": "u32",
    "sequence": "u64",
    "sig_op_count": "u8",
    "compute_budget": "u16",
    "signature_script": "hex",
    "spends": "object",
}
SPENDS_SHAPE = {
    "from": "any",
    "value": "u64",
    "script_public_key_version": "u16",
    "script_public_key": "hex",
    "covenant_id": "hex32?",
}
OUTPUT_SHAPE = {
    "value": "u64",
    "script_public_key_version": "u16",
    "script_public_key": "hex",
    "covenant_binding": "object?",
    "role": "text",
    "to": "text?",
    "state": "object?",
}
BINDING_SHAPE = {"authorizing_input": "u16", "covenant_id": "hex32"}
GATE_SHAPE = {"scripts": "bool", "consensus": "bool", "mempool": "bool", "block": "bool", "refusal": "object?"}
GATE_REFUSAL_SHAPE = {"layer": "text", "input": "u32", "rule": "any", "message": "text"}
REFUSAL_EXPECTED_SHAPE = {"kind": "text", "input": "u32", "contract_line": "text?", "node_message_contains": "text"}
TWIN_SHAPE = {"accepted": "bool", "judged_at_daa": "u64", "what": "text"}
KCC20_SHAPE = {
    "contract": "text",
    "amount": "u64",
    "owner": "hex32",
    "owner_scheme": "u8",
    "borrow_scheme": "u8",
    "borrow_guard": "hex32",
    "extension_commitment": "hex32",
}
VESTING_SHAPE = {
    "contract": "text",
    "beneficiary": "hex32",
    "token_covid": "hex32",
    "token_template": "hex32",
    "start": "u64",
    "cliff": "u64",
    "duration": "u64",
    "total": "u64",
    "released": "u64",
}
STATE_SHAPES = {"KCC20": KCC20_SHAPE, "TokenVesting": VESTING_SHAPE}


def shape(problems: list, where: str, item, spec: dict) -> bool:
    """Whether `item` has every field of `spec` with a value of the right kind. Extra fields are let be."""
    if not isinstance(item, dict):
        problems.append(f"{where}: is not an object")
        return False
    fits = True
    for field, kind in spec.items():
        if field not in item:
            problems.append(f"{where}: the field {field!r} is missing")
            fits = False
            continue
        optional = kind.endswith("?")
        if item[field] is None and optional:
            continue
        test, words = KINDS[kind.rstrip("?")]
        if not test(item[field]):
            problems.append(f"{where}: the field {field!r} is not {words}{' or null' if optional else ''}")
            fits = False
    return fits


def state_shape(problems: list, where: str, state) -> bool:
    """A contract state of a known contract has every field of its kind. An unknown contract is left to check h."""
    if not isinstance(state, dict):
        problems.append(f"{where}: is not an object")
        return False
    spec = STATE_SHAPES.get(state.get("contract"))
    return shape(problems, where, state, spec) if spec else True


def label(position: int, transaction) -> str:
    name = transaction.get("name") if isinstance(transaction, dict) else None
    kind = transaction.get("kind") if isinstance(transaction, dict) and transaction.get("kind") in ("step", "probe") else "move"
    return f"{kind} {position + 1} ({name!r})" if isinstance(name, str) else f"move {position + 1}"


def valid_source(source) -> bool:
    if source in ("funding", "token_coin"):
        return True
    return isinstance(source, dict) and isinstance(source.get("move"), str) and is_uint(source.get("output"), 32)


def check_shape(data) -> list:
    """Every field the program reads is there and of the right kind. Returns the problems."""
    problems: list = []
    if not shape(problems, "file", data, TOP_SHAPE):
        return problems
    shape(problems, "schedule", data["schedule"], SCHEDULE_SHAPE)
    shape(problems, "funding", data["funding"], FUNDING_SHAPE)
    if shape(problems, "token", data["token"], TOKEN_SHAPE):
        shape(problems, "token, coin", data["token"]["coin"], COIN_SHAPE)
        state_shape(problems, "token, state", data["token"]["state"])
    shape(problems, "totals", data["totals"], TOTALS_SHAPE)
    for name, result in data["launch_checks"].items():
        if not isinstance(result, str):
            problems.append(f"launch_checks: {name!r} is not a text")
    if not data["parties"]:
        problems.append("parties: none are named")
    for name, party in data["parties"].items():
        shape(problems, f"party {name!r}", party, PARTY_SHAPE)
    for position, transaction in enumerate(data["moves"]):
        where = label(position, transaction)
        if not shape(problems, where, transaction, TRANSACTION_SHAPE):
            continue
        if shape(problems, f"{where}, gate", transaction["gate"], GATE_SHAPE) and transaction["gate"]["refusal"] is not None:
            shape(problems, f"{where}, gate refusal", transaction["gate"]["refusal"], GATE_REFUSAL_SHAPE)
        if transaction["refusal_expected"] is not None:
            shape(problems, f"{where}, refusal_expected", transaction["refusal_expected"], REFUSAL_EXPECTED_SHAPE)
        if transaction["twin"] is not None:
            shape(problems, f"{where}, twin", transaction["twin"], TWIN_SHAPE)
        if not transaction["inputs"]:
            problems.append(f"{where}: has no inputs")
        if not transaction["outputs"]:
            problems.append(f"{where}: has no outputs")
        for number, tx_input in enumerate(transaction["inputs"]):
            at = f"{where}, input {number}"
            if not shape(problems, at, tx_input, INPUT_SHAPE):
                continue
            if shape(problems, f"{at}, spends", tx_input["spends"], SPENDS_SHAPE) and not valid_source(tx_input["spends"]["from"]):
                problems.append(f'{at}, spends: "from" is neither "funding" nor "token_coin" nor a move and an output number')
        for number, output in enumerate(transaction["outputs"]):
            at = f"{where}, output {number}"
            if not shape(problems, at, output, OUTPUT_SHAPE):
                continue
            if output["covenant_binding"] is not None:
                shape(problems, f"{at}, covenant binding", output["covenant_binding"], BINDING_SHAPE)
            if output["state"] is not None:
                state_shape(problems, f"{at}, state", output["state"])
    names = [t["name"] for t in data["moves"] if isinstance(t, dict) and isinstance(t.get("name"), str)]
    for name in sorted({name for name in names if names.count(name) > 1}):
        problems.append(f"file: the name {name!r} is used by more than one move")
    return problems


# --------------------------------------------------------------------------
# The chain as coins: what each transaction spends and makes
# --------------------------------------------------------------------------


def steps_of(data: dict) -> list:
    return [move for move in data["moves"] if move["kind"] == "step"]


def funding_coin(data: dict) -> dict:
    funding = data["funding"]
    return {
        "key": (funding["transaction_id"], funding["index"]),
        "value": funding["value"],
        "version": funding["script_public_key_version"],
        "script": funding["script_public_key"],
        "covenant_id": None,
        "role": "funding",
        "to": funding["owner"],
        "state": None,
        "label": "the funding coin",
    }


def token_coin(data: dict) -> dict:
    coin = data["token"]["coin"]
    return {
        "key": (coin["transaction_id"], coin["index"]),
        "value": coin["value"],
        "version": coin["script_public_key_version"],
        "script": coin["script_public_key"],
        "covenant_id": data["token"]["covenant_id"],
        "role": "token",
        "to": None,
        "state": data["token"]["state"],
        "label": "the token coin",
    }


def output_coins(transaction: dict) -> list:
    """The coins a transaction makes, in the order of its outputs."""
    coins = []
    for number, output in enumerate(transaction["outputs"]):
        binding = output["covenant_binding"]
        coins.append(
            {
                "key": (transaction["transaction_id"], number),
                "value": output["value"],
                "version": output["script_public_key_version"],
                "script": output["script_public_key"],
                "covenant_id": binding["covenant_id"] if binding else None,
                "role": output["role"],
                "to": output["to"],
                "state": output["state"],
                "label": f"output {number} of {transaction['name']!r}",
            }
        )
    return coins


def spent_keys(transaction: dict) -> list:
    return [(tx_input["transaction_id"], tx_input["index"]) for tx_input in transaction["inputs"]]


def spent_coin(tx_input: dict) -> dict:
    """The coin an input says it spends, as its `spends` block describes it."""
    spends = tx_input["spends"]
    return {
        "key": (tx_input["transaction_id"], tx_input["index"]),
        "value": spends["value"],
        "version": spends["script_public_key_version"],
        "script": spends["script_public_key"],
        "covenant_id": spends["covenant_id"],
        "label": spends.get("what") or "a coin",
    }


def starting_coins(data: dict) -> dict:
    return {coin["key"]: coin for coin in (funding_coin(data), token_coin(data))}


def unspent_after_each_step(data: dict) -> list:
    """`result[k]`: the coins unspent once the first k steps are on chain, by outpoint. Probes change nothing."""
    current = starting_coins(data)
    result = [dict(current)]
    for step in steps_of(data):
        for key in spent_keys(step):
            current.pop(key, None)
        for coin in output_coins(step):
            current[coin["key"]] = coin
        result.append(dict(current))
    return result


def steps_before(data: dict, position: int) -> int:
    """How many steps come before the move at `position`."""
    return sum(1 for move in data["moves"][:position] if move["kind"] == "step")


def outpoint_text(key: tuple) -> str:
    return f"{key[0]}:{key[1]}"


def address_of_script(version: int, script: str) -> str:
    return address_from_script_public_key(ScriptPublicKey(version, script), NETWORK_TYPE).to_string()


# --------------------------------------------------------------------------
# Rebuilding a transaction from the fields of the file
# --------------------------------------------------------------------------


def build_transaction(transaction: dict, entries: list | None = None, with_bindings: bool = True) -> Transaction:
    """The transaction as an SDK object, from the fields of the file alone, lock time included.

    `entries` are the node's own UTXO entries of the spent coins, one per
    input, for a transaction that is about to be submitted. Without bindings
    the outputs carry no covenant binding: the form from which the SDK derives
    a genesis covenant id by itself.
    """
    inputs = []
    for number, item in enumerate(transaction["inputs"]):
        more = {}
        if entries is not None:
            more["utxo"] = UtxoEntryReference.from_dict(entries[number])
        inputs.append(
            TransactionInput(
                TransactionOutpoint(Hash(item["transaction_id"]), item["index"]),
                bytes.fromhex(item["signature_script"]),
                sequence=item["sequence"],
                sig_op_count=item["sig_op_count"],
                compute_budget=item["compute_budget"],
                **more,
            )
        )
    outputs = []
    for item in transaction["outputs"]:
        binding = None
        if with_bindings and item["covenant_binding"] is not None:
            bound = item["covenant_binding"]
            binding = CovenantBinding(bound["authorizing_input"], Hash(bound["covenant_id"]))
        script = ScriptPublicKey(item["script_public_key_version"], item["script_public_key"])
        outputs.append(TransactionOutput(item["value"], script, binding))
    built = Transaction(
        transaction["version"],
        inputs,
        outputs,
        lock_time=transaction["lock_time"],
        subnetwork_id=bytes.fromhex(transaction["subnetwork_id"]),
        gas=transaction["gas"],
        payload=bytes.fromhex(transaction["payload"]),
        mass=transaction["mass"],
    )
    # The SDK works the id out when the object is made; asked again, it reads the object as it is now.
    built.finalize()
    return built


def differences_outside_the_id(built: Transaction, transaction: dict) -> list:
    """Where the built object differs from the file in what the id does not cover, and in the lock time.

    Equal ids say nothing about the signature scripts, the mass field and the
    compute budgets, so these are read back from the object and compared. The
    lock time is covered by the id; it is read back as well, because the run
    depends on it.
    """
    shown = built.to_dict()
    found = []
    if shown.get("mass") != transaction["mass"]:
        found.append(f"the mass field reads {shown.get('mass')}, the file says {transaction['mass']}")
    if shown.get("lockTime") != transaction["lock_time"]:
        found.append(f"the lock time reads {shown.get('lockTime')}, the file says {transaction['lock_time']}")
    if len(shown["inputs"]) != len(transaction["inputs"]):
        return found + ["the number of inputs differs"]
    for number, (have, want) in enumerate(zip(shown["inputs"], transaction["inputs"])):
        if have.get("signatureScript") != want["signature_script"]:
            found.append(f"input {number}: the signature script differs from the file")
        if have.get("computeBudget") != want["compute_budget"]:
            found.append(f"input {number}: the compute budget reads {have.get('computeBudget')}, the file says {want['compute_budget']}")
        if have.get("sigOpCount") != want["sig_op_count"]:
            found.append(f"input {number}: the signature operation count differs from the file")
        if have.get("sequence") != want["sequence"]:
            found.append(f"input {number}: the sequence reads {have.get('sequence')}, the file says {want['sequence']}")
    return found


# --------------------------------------------------------------------------
# The checks of the chain file (offline)
# --------------------------------------------------------------------------


def check_basics(data: dict) -> list:
    """a. Format, network, limits, the builder's launch checks and gates."""
    problems = []
    if data["format"] != CHAIN_FORMAT:
        problems.append(f"file: the format is {data['format']!r}, not {CHAIN_FORMAT!r}")
    if data["network"] != NETWORK_ID:
        problems.append(f"file: the network is {data['network']!r}, not {NETWORK_ID!r}: wrong network")
    if data["nothing_was_sent"] is not True:
        problems.append("file: nothing_was_sent is not true: the builder does not say that it sent nothing")
    if data["coin_value"] != COIN_VALUE:
        problems.append(f"file: the coin value is {kas(data['coin_value'])} KAS, the run allows exactly {kas(COIN_VALUE)}")
    if data["funding"]["value"] > MAX_FUNDING:
        problems.append(f"funding: the coin holds {kas(data['funding']['value'])} KAS, above the hard limit of {kas(MAX_FUNDING)} KAS")
    if data["feerate"] < MIN_FEERATE:
        problems.append(f"file: the feerate {data['feerate']} is below {MIN_FEERATE} sompi per gram")
    schedule = data["schedule"]
    for field, only in (("cliff", CLIFF), ("duration", DURATION), ("total", TOTAL)):
        if schedule[field] != only:
            problems.append(f"schedule: {field} is {schedule[field]}; this run has {only}")
    if not 0 < schedule["start"] or schedule["start"] + schedule["duration"] + SEND_MARGIN >= MAX_LOCK_TIME:
        problems.append(f"schedule: the start {schedule['start']} is not a DAA score that leaves room for the schedule")
    for name in LAUNCH_CHECKS:
        if data["launch_checks"].get(name) != "ok":
            problems.append(f"launch_checks: {name} is {data['launch_checks'].get(name)!r}, not \"ok\"")
    for name, result in data["launch_checks"].items():
        if name not in LAUNCH_CHECKS and result != "ok":
            problems.append(f"launch_checks: {name} is {result!r}, not \"ok\"")
    steps = [m for m in data["moves"] if m["kind"] == "step"]
    probes = [m for m in data["moves"] if m["kind"] == "probe"]
    if not 1 <= len(steps) <= MAX_STEPS:
        problems.append(f"file: {len(steps)} steps; the run allows 1 to {MAX_STEPS}")
    if len(probes) > MAX_PROBES:
        problems.append(f"file: {len(probes)} probes; the run allows at most {MAX_PROBES}")
    for position, move in enumerate(data["moves"]):
        where = label(position, move)
        gate = move["gate"]
        if move["kind"] == "step":
            if move["expect"] != "accepted":
                problems.append(f'{where}: expect is {move["expect"]!r}, a step must say "accepted"')
            failed = [layer for layer in ("scripts", "consensus", "mempool", "block") if gate[layer] is not True]
            if failed:
                problems.append(f"{where}: the builder's gate did not pass it ({', '.join(failed)})")
            if gate["refusal"] is not None:
                problems.append(f"{where}: the builder's gate recorded a refusal")
            if move["refusal_expected"] is not None or move["twin"] is not None:
                problems.append(f"{where}: a step with a refusal_expected or a twin")
        elif move["kind"] == "probe":
            if move["expect"] != "refused":
                problems.append(f'{where}: expect is {move["expect"]!r}, a probe must say "refused"')
            if gate["refusal"] is None:
                problems.append(f"{where}: the builder's gate recorded no refusal")
            if move["refusal_expected"] is None:
                problems.append(f"{where}: a probe without refusal_expected")
            if move["twin"] is None:
                problems.append(f"{where}: a probe without a twin")
            elif move["twin"]["accepted"] is not True:
                problems.append(f"{where}: its twin (the same move with the wrong thing put right) was not accepted by the gate")
        else:
            problems.append(f'{where}: the kind is {move["kind"]!r}, neither "step" nor "probe"')
    return problems


def check_moves(data: dict) -> list:
    """b. The moves of FORMAT.md in their order; lock times, sending windows and the refusals the probes expect."""
    problems = []
    moves = data["moves"]
    names = [move["name"] for move in moves]
    wanted = [name for name, *_ in MOVES]
    if names != wanted:
        problems.append(f"moves: the file has {names}; this run has exactly {wanted}, in that order")
        return problems
    start = data["schedule"]["start"]
    by_name = {move["name"]: move for move in moves}
    for position, (move, (name, kind, offset, refusal)) in enumerate(zip(moves, MOVES)):
        where = label(position, move)
        if move["kind"] != kind:
            problems.append(f"{where}: the kind is {move['kind']!r}; this move is a {kind}")
        lock = 0 if offset is None else start + offset
        if move["lock_time"] != lock:
            problems.append(f"{where}: the lock time is {move['lock_time']}; by the schedule it is {lock}" + ("" if offset is None else f" (start + {offset})"))
        # FORMAT.md: 0 for a lock time of 0, otherwise lock time + 20, except B2, which goes from B1's time on.
        if name == B2:
            after = by_name[B1]["send_after_daa"]
        else:
            after = 0 if move["lock_time"] == 0 else move["lock_time"] + SEND_MARGIN
        if move["send_after_daa"] != after:
            problems.append(f"{where}: send_after_daa is {move['send_after_daa']}; by its rule it is {after}")
        before = move["lock_time"] - WINDOW_MARGIN if refusal == "finality" else None
        if move["send_before_daa"] != before:
            problems.append(f"{where}: send_before_daa is {move['send_before_daa']}; by its rule it is {before}")
        if name == B2:
            judged = by_name[B1]["send_after_daa"]
        elif move["lock_time"] != 0:
            judged = move["send_after_daa"]
        else:
            judged = None
        if judged is not None and move["judged_at_daa"] != judged:
            problems.append(f"{where}: judged_at_daa is {move['judged_at_daa']}; by its rule it is {judged}")
        if judged is None and move["judged_at_daa"] > start:
            problems.append(f"{where}: judged_at_daa {move['judged_at_daa']} is after the start of the schedule {start}")
        if kind != "probe" or move["refusal_expected"] is None:
            continue
        expected = move["refusal_expected"]
        if expected["kind"] != refusal:
            problems.append(f"{where}: refusal_expected.kind is {expected['kind']!r}; this probe is refused by {refusal!r}")
        # The words a node's refusal must contain are the file's own, whatever they are (FORMAT.md names
        # typical ones per kind, but a contract line may fail with other words); only an empty text is refused.
        if not expected["node_message_contains"].strip():
            problems.append(f"{where}: refusal_expected.node_message_contains is empty: any refusal would read as the expected one")
        gate_refusal = move["gate"]["refusal"]
        if gate_refusal is not None and expected["kind"] in REFUSAL_LAYER:
            layer = REFUSAL_LAYER[expected["kind"]]
            if gate_refusal["layer"] != layer:
                problems.append(f"{where}: the gate refused it at the layer {gate_refusal['layer']!r}; refusal_expected says {layer!r}")
            if gate_refusal["input"] != expected["input"]:
                problems.append(f"{where}: the gate refused input {gate_refusal['input']}; refusal_expected says input {expected['input']}")
            if move["gate"].get(layer) is not False:
                problems.append(f"{where}: the gate's answer for {layer!r} is not false, though it refused it there")
        if gate_refusal is not None and expected["node_message_contains"] not in gate_refusal["message"]:
            problems.append(
                f"{where}: the gate's message {gate_refusal['message']!r} does not contain {expected['node_message_contains']!r}, "
                "the words `send` takes for the expected refusal"
            )
        if expected["input"] >= len(move["inputs"]):
            problems.append(f"{where}: refusal_expected names input {expected['input']}, which does not exist")
        twin = move["twin"]
        if name == B2 and twin is not None and not twin["judged_at_daa"] > move["lock_time"]:
            problems.append(f"{where}: its twin was judged at {twin['judged_at_daa']}, not after its lock time {move['lock_time']}")
    return problems


def window_notes(data: dict) -> list:
    """Moves that `send` can never hand over: their sending window is empty. Notes, not refusals."""
    notes = []
    for move in data["moves"]:
        before = move["send_before_daa"]
        if before is not None and before <= move["send_after_daa"] + 1:
            notes.append(
                f"note: {move['name']!r} can never be sent: it may go only when the DAA score is above {move['send_after_daa']} "
                f"and below {before}. `send` records it as not sent (window empty) and goes on."
            )
    return notes


def check_parties(data: dict, addresses: dict) -> list:
    """c. The parties of the file are payer, alice and beneficiary, those named on the command line."""
    problems = []
    parties = data["parties"]
    if sorted(parties) != sorted(PARTY_NAMES):
        problems.append(f"parties: the file names {sorted(parties)}; this run has exactly {sorted(PARTY_NAMES)}")
    if set(parties) != set(addresses):
        problems.append(f"parties: the file names {sorted(parties)}, the command line names {sorted(addresses)}: they must be the same")
    for name, party in parties.items():
        if party["script_public_key"] != "20" + party["x_only_public_key"] + "ac":
            problems.append(f"party {name!r}: its script is not the pay-to-public-key script of its key")
        address = addresses.get(name)
        if address is None:
            continue
        if not address.startswith(ADDRESS_PREFIX):
            problems.append(f"party {name!r}: the address given does not start with {ADDRESS_PREFIX}")
            continue
        try:
            script = pay_to_address_script(Address(address))
        except Exception as error:
            problems.append(f"party {name!r}: the address given does not read as an address ({error})")
            continue
        if (script.version, script.script) != (0, party["script_public_key"]):
            problems.append(
                f"party {name!r}: the address given does not match the file: it pays to {script.script}, the file says {party['script_public_key']}"
            )
    keys = [party["x_only_public_key"] for party in parties.values()]
    if len(set(keys)) != len(keys):
        problems.append("parties: two parties have the same key")
    funding = data["funding"]
    if funding["owner"] != "payer":
        problems.append(f"funding: the owner is {funding['owner']!r}; the funding coin must be the payer's")
    owner = parties.get(funding["owner"])
    if owner is None:
        problems.append(f"funding: the owner {funding['owner']!r} is not a party")
    elif (funding["script_public_key_version"], funding["script_public_key"]) != (0, owner["script_public_key"]):
        problems.append(f"funding: the coin is not locked to the script of its owner {funding['owner']!r}")
    return problems


def check_rebuild(data: dict) -> list:
    """d. Every transaction, rebuilt by the SDK from the fields, has the id the file states; the fixed fields."""
    problems = []
    start = data["schedule"]["start"]
    for position, transaction in enumerate(data["moves"]):
        where = label(position, transaction)
        try:
            built = build_transaction(transaction)
        except Exception as error:
            problems.append(f"{where}: cannot be rebuilt from its fields ({error})")
            continue
        if built.id != transaction["transaction_id"]:
            problems.append(f"{where}: id mismatch: rebuilt from the fields it is {built.id}, the file says {transaction['transaction_id']}")
        for difference in differences_outside_the_id(built, transaction):
            problems.append(f"{where}: {difference}")
        fixed = (("version", 1), ("gas", 0), ("payload", ""), ("subnetwork_id", ZERO_SUBNETWORK))
        for field, only in fixed:
            if transaction[field] != only:
                problems.append(f"{where}: {field} is {transaction[field]!r}; a transaction of this run has only {only!r} there")
        lock = transaction["lock_time"]
        if lock != 0 and not start <= lock < MAX_LOCK_TIME:
            problems.append(f"{where}: the lock time {lock} is neither 0 nor between the start {start} and {MAX_LOCK_TIME}")
        for number, tx_input in enumerate(transaction["inputs"]):
            for field in ("sequence", "sig_op_count"):
                if tx_input[field] != 0:
                    problems.append(f"{where}, input {number}: {field} is {tx_input[field]}; a transaction of this run has only 0 there")
            if not tx_input["signature_script"]:
                problems.append(f"{where}, input {number}: empty signature script")
            if tx_input["compute_budget"] > MAX_COMPUTE_BUDGET:
                problems.append(f"{where}, input {number}: the compute budget {tx_input['compute_budget']} is above {MAX_COMPUTE_BUDGET}")
        if not 0 < transaction["mass"] <= MAX_MASS:
            problems.append(f"{where}: the mass field {transaction['mass']} is not between 1 and {MAX_MASS}")
    return problems


def compare_spent(where: str, tx_input: dict, coin: dict) -> list:
    """An input's outpoint and `spends` block against the coin it names."""
    problems = []
    claimed = spent_coin(tx_input)
    if claimed["key"] != coin["key"]:
        problems.append(f"{where}: the outpoint {outpoint_text(claimed['key'])} is not {coin['label']} ({outpoint_text(coin['key'])})")
    if claimed["value"] != coin["value"]:
        problems.append(f"{where}: spends says the value is {claimed['value']}, {coin['label']} holds {coin['value']}")
    if (claimed["version"], claimed["script"]) != (coin["version"], coin["script"]):
        problems.append(f"{where}: spends states another script than {coin['label']} has")
    if claimed["covenant_id"] != coin["covenant_id"]:
        problems.append(f"{where}: spends states another covenant id than {coin['label']} carries")
    return problems


def named_coin(data: dict, source, position: int):
    """The coin a `spends.from` names: (coin, None) or (None, what is wrong). Only steps before `position` count."""
    if source == "funding":
        return funding_coin(data), None
    if source == "token_coin":
        return token_coin(data), None
    names = [move["name"] for move in data["moves"]]
    if source["move"] not in names:
        return None, f"the move {source['move']!r} does not exist"
    at = names.index(source["move"])
    if at >= position:
        return None, f"the move {source['move']!r} is not an earlier move"
    if data["moves"][at]["kind"] != "step":
        return None, f"the move {source['move']!r} is a probe: nothing it makes is ever on chain"
    coins = output_coins(data["moves"][at])
    if source["output"] >= len(coins):
        return None, f"output {source['output']} of the move {source['move']!r} does not exist"
    return coins[source["output"]], None


def check_linkage(data: dict) -> list:
    """e. Steps spend the funding coin, the token coin or outputs of earlier steps, each once; probes spend what is unspent at their place."""
    problems = []
    spent_by: dict = {}
    unspent = starting_coins(data)
    for position, move in enumerate(data["moves"]):
        where = label(position, move)
        seen = set()
        for number, tx_input in enumerate(move["inputs"]):
            at = f"{where}, input {number}"
            key = (tx_input["transaction_id"], tx_input["index"])
            coin, wrong = named_coin(data, tx_input["spends"]["from"], position)
            if coin is None:
                problems.append(f"{at}: {wrong}")
            else:
                problems += compare_spent(at, tx_input, coin)
                if coin["key"] not in unspent:
                    problems.append(f"{at}: {coin['label']} is not unspent at this place of the list")
            if key in seen:
                problems.append(f"{at}: the outpoint {outpoint_text(key)} is spent twice by this move")
            seen.add(key)
            if move["kind"] == "step":
                if key in spent_by:
                    problems.append(f"{at}: the outpoint {outpoint_text(key)} was already spent by {spent_by[key]}")
                else:
                    spent_by[key] = at
        if move["kind"] == "step":
            for key in spent_keys(move):
                unspent.pop(key, None)
            for coin in output_coins(move):
                unspent[coin["key"]] = coin
    for coin in starting_coins(data).values():
        if coin["key"] not in spent_by:
            problems.append(f"{coin['label']}: no step spends it")
    # FORMAT.md: B1, B2, B3 and B4 spend the vesting coin of A1 and the vault of A2, the coins R1 spends as well.
    by_name = {move["name"]: (position, move) for position, move in enumerate(data["moves"])}
    if GENESIS in by_name and VAULT_MOVE in by_name:
        wanted = {coin["key"] for coin in output_coins(by_name[GENESIS][1]) if coin["role"] == "vesting"}
        wanted |= {
            coin["key"] for coin in output_coins(by_name[VAULT_MOVE][1])
            if coin["role"] == "token" and isinstance(coin["state"], dict) and coin["state"].get("owner_scheme") == OWNER_COVENANT
        }
        for name in (*CLAIMS, FIRST_RELEASE):
            if name in by_name and not wanted <= set(spent_keys(by_name[name][1])):
                position, move = by_name[name]
                problems.append(f"{label(position, move)}: does not spend both the vesting coin of {GENESIS!r} and the vault of {VAULT_MOVE!r}")
    return problems


def check_money(data: dict) -> list:
    """f. Fees: what each move burns, the floor, the cap, the hard limit."""
    problems = []
    for position, transaction in enumerate(data["moves"]):
        where = label(position, transaction)
        spent = sum(tx_input["spends"]["value"] for tx_input in transaction["inputs"])
        made = sum(output["value"] for output in transaction["outputs"])
        fee = transaction["fee"]
        if spent - made != fee:
            problems.append(f"{where}: spends {spent} and makes {made}, a difference of {spent - made}; the file says the fee is {fee}")
        if fee <= 0:
            problems.append(f"{where}: the fee is not above zero")
        if fee < transaction["fee_floor"]:
            problems.append(f"{where}: the fee {fee} is below its floor {transaction['fee_floor']}")
    steps = sum(move["fee"] for move in data["moves"] if move["kind"] == "step")
    probes = sum(move["fee"] for move in data["moves"] if move["kind"] == "probe")
    if steps != data["totals"]["fees_of_steps"]:
        problems.append(f"totals: the fees of the steps sum to {steps}, the file says {data['totals']['fees_of_steps']}")
    if probes != data["totals"]["fees_of_probes"]:
        problems.append(f"totals: the fees of the probes sum to {probes}, the file says {data['totals']['fees_of_probes']}")
    if steps + probes > data["fee_cap_total"]:
        problems.append(f"totals: the fees of all moves sum to {steps + probes}, above the cap of the file {data['fee_cap_total']}")
    if steps + probes > MAX_TOTAL_FEES:
        problems.append(f"totals: the fees of all moves sum to {kas(steps + probes)} KAS, above the hard limit of {kas(MAX_TOTAL_FEES)} KAS")
    return problems


def is_p2sh(script: str) -> bool:
    return len(script) == 70 and script.startswith("aa20") and script.endswith("87")


def check_outputs(data: dict) -> list:
    """g. Every output is a vesting coin, a token coin or change to the payer; every binding has its authorizing input."""
    problems = []
    payer = data["parties"].get("payer")
    covenant_of = {"vesting": data["vesting_covenant_id"], "token": data["token"]["covenant_id"]}
    for position, transaction in enumerate(data["moves"]):
        spent = [coin for coin in (spent_coin(i) for i in transaction["inputs"])]
        for number, output in enumerate(transaction["outputs"]):
            where = f"{label(position, transaction)}, output {number}"
            role, binding = output["role"], output["covenant_binding"]
            version, script = output["script_public_key_version"], output["script_public_key"]
            if role in covenant_of:
                if version != 0 or not is_p2sh(script):
                    problems.append(f"{where}: a {role} coin must be locked by a version 0 pay-to-script-hash script")
                if output["value"] != data["coin_value"]:
                    problems.append(f"{where}: a {role} coin must hold {data['coin_value']}, this one holds {output['value']}")
                if output["to"] is not None:
                    problems.append(f'{where}: a {role} coin names a party in "to"')
                if binding is None:
                    problems.append(f"{where}: a {role} coin without a covenant binding")
                    continue
                if binding["covenant_id"] != covenant_of[role]:
                    problems.append(f"{where}: a {role} coin bound to another covenant id ({binding['covenant_id']}) than {covenant_of[role]}")
                authorizing = binding["authorizing_input"]
                if authorizing >= len(transaction["inputs"]):
                    problems.append(f"{where}: its authorizing input {authorizing} does not exist")
                    continue
                # The node takes a binding whose authorizing input carries the same covenant id as a continuation, and
                # any other as a genesis, which must give the id it names. The only genesis of this run is A1's vesting coin.
                genesis = transaction["name"] == GENESIS and role == "vesting"
                carried = spent[authorizing]["covenant_id"]
                if not genesis and carried != binding["covenant_id"]:
                    problems.append(
                        f"{where}: its authorizing input {authorizing} carries the covenant id {carried}, not the one it is bound to"
                    )
            elif role == "change":
                if binding is not None:
                    problems.append(f"{where}: a change output with a covenant binding")
                if output["state"] is not None:
                    problems.append(f"{where}: a change output with a contract state")
                if output["to"] != "payer":
                    problems.append(f"{where}: the change goes to {output['to']!r}; change goes only to the payer")
                elif payer is None or (version, script) != (0, payer["script_public_key"]):
                    problems.append(f"{where}: the change is said to go to the payer, but its script is not the payer's")
            else:
                problems.append(f"{where}: the role {role!r} is not one of vesting, token, change")
    return problems


def check_covenant_id(data: dict) -> tuple:
    """h. The vesting covenant id, derived again by the SDK from A1 alone. Returns (problems, what was done)."""
    first = next((move for move in data["moves"] if move["name"] == GENESIS), None)
    if first is None:
        return [f"vesting covenant id: there is no {GENESIS!r} to derive it from"], "not checked"
    groups: dict = {}
    for number, output in enumerate(first["outputs"]):
        if output["role"] == "vesting" and output["covenant_binding"] is not None:
            groups.setdefault(output["covenant_binding"]["authorizing_input"], []).append(number)
    if not groups:
        return [f"vesting covenant id: {GENESIS!r} makes no vesting coin, so the id cannot be derived"], "not checked"
    try:
        bare = build_transaction(first, with_bindings=False)
        bare.populate_genesis_covenants(
            [GenesisCovenantGroup(authorizing_input=authorizing, outputs=numbers) for authorizing, numbers in sorted(groups.items())]
        )
        bare.finalize()
        derived = [output.to_dict().get("covenant") for output in bare.outputs]
    except Exception as error:
        return [f"vesting covenant id: NOT CHECKED: the SDK could not derive it ({error})"], "not checked"
    problems = []
    for numbers in groups.values():
        for number in numbers:
            got = (derived[number] or {}).get("covenantId")
            if got != data["vesting_covenant_id"]:
                problems.append(f"vesting covenant id: for output {number} of {GENESIS!r} the SDK derives {got}, the file says {data['vesting_covenant_id']}")
    if bare.id != first["transaction_id"]:
        problems.append(f"vesting covenant id: {GENESIS!r} with the binding the SDK derives is another transaction than the file's")
    count = sum(len(numbers) for numbers in groups.values())
    return problems, f"derived by the SDK from {GENESIS!r}, {count} output(s)"


def kcc20_amount(state) -> int:
    return state["amount"] if isinstance(state, dict) and state.get("contract") == "KCC20" else 0


def state_of_spent(data: dict, tx_input: dict):
    """The contract state of the coin an input spends, from the coin the file names; None for a plain coin."""
    source = tx_input["spends"]["from"]
    if source == "funding":
        return None
    if source == "token_coin":
        return data["token"]["state"]
    for move in data["moves"]:
        if move["name"] == source["move"] and source["output"] < len(move["outputs"]):
            return move["outputs"][source["output"]]["state"]
    return None


def output_shape(data: dict, position: int, move: dict, keys: dict) -> list:
    """The outputs of a move against OUTPUTS: role, contract, owner, amount and order."""
    want = OUTPUTS.get(move["name"])
    if want is None:
        return []
    where = label(position, move)
    have = move["outputs"]
    if len(have) != len(want):
        return [f"{where}: has {len(have)} outputs; this move has {len(want)}: {', '.join(item[0] for item in want)}"]
    problems = []
    vesting_id = data["vesting_covenant_id"]
    for number, (item, output) in enumerate(zip(want, have)):
        at = f"{where}, output {number}"
        state = output["state"] if isinstance(output["state"], dict) else {}
        kind = item[0]
        if kind == "change":
            if output["role"] != "change":
                problems.append(f"{at}: is a {output['role']} output; here the change to the payer comes")
        elif kind == "vesting":
            if output["role"] != "vesting" or state.get("contract") != "TokenVesting" or state.get("released") != item[1]:
                problems.append(f"{at}: here comes the vesting coin with {item[1]} released; it is a {output['role']} output with {state or 'no state'}")
        else:
            amount = item[1]
            owner, scheme = (keys.get(item[2]), OWNER_KEY) if kind == "claim" else (vesting_id, OWNER_COVENANT)
            fits = (output["role"] == "token" and state.get("contract") == "KCC20" and state.get("amount") == amount
                    and state.get("owner") == owner and state.get("owner_scheme") == scheme)
            if not fits:
                whose = f"the key of {item[2]!r}" if kind == "claim" else "the vesting covenant"
                problems.append(f"{at}: here comes a token coin of {amount} tokens owned by {whose} (scheme {scheme}); it is a {output['role']} output with {state or 'no state'}")
    return problems


def check_states(data: dict) -> list:
    """i. The states of the coins: owners, the vesting parameters, what is released, and the tokens adding up."""
    problems = []
    parties = data["parties"]
    keys = {party["x_only_public_key"]: name for name, party in parties.items()}
    key_of = {name: party["x_only_public_key"] for name, party in parties.items()}
    vesting_id = data["vesting_covenant_id"]
    schedule = data["schedule"]
    token = data["token"]

    def kcc20(where: str, state: dict) -> None:
        if state["amount"] == 0:
            problems.append(f"{where}: a token coin of 0 tokens")
        if state["owner_scheme"] == OWNER_KEY:
            if state["owner"] not in keys:
                problems.append(f"{where}: owner scheme 0, but the owner {state['owner']} is not the key of a party")
        elif state["owner_scheme"] == OWNER_COVENANT:
            if state["owner"] != vesting_id:
                problems.append(f"{where}: owner scheme 4, but the owner {state['owner']} is not the vesting covenant id")
        else:
            problems.append(f"{where}: the owner scheme is {state['owner_scheme']}; this run allows only 0 (a party's key) and 4 (the vesting covenant)")
        if state["borrow_scheme"] != 0:
            problems.append(f"{where}: the borrow scheme is {state['borrow_scheme']}; borrowing must be disabled (0)")

    def vesting(where: str, state: dict) -> None:
        if state["beneficiary"] != key_of.get("beneficiary"):
            problems.append(f"{where}: the beneficiary {state['beneficiary']} is not the key of the party 'beneficiary'")
        if state["token_covid"] != token["covenant_id"]:
            problems.append(f"{where}: token_covid {state['token_covid']} is not the token's covenant id {token['covenant_id']}")
        if state["token_template"] != token["template_hash"]:
            problems.append(f"{where}: token_template {state['token_template']} is not token.template_hash {token['template_hash']}")
        for field in ("start", "cliff", "duration", "total"):
            if state[field] != schedule[field]:
                problems.append(f"{where}: {field} is {state[field]}, the schedule of the file says {schedule[field]}")

    held = token["state"]
    if held.get("contract") != "KCC20":
        problems.append(f"token, state: the contract is {held.get('contract')!r}, not 'KCC20'")
    else:
        if held.get("owner") != key_of.get("alice") or held.get("owner_scheme") != OWNER_KEY:
            problems.append("token, state: the token coin is not owned by Alice's key with owner scheme 0")
        if held.get("borrow_scheme") != 0:
            problems.append("token, state: borrowing is not disabled (borrow_scheme 0)")
        if held.get("amount") != schedule["total"]:
            problems.append(f"token, state: the token coin holds {held.get('amount')} tokens; the vault takes it whole, so it must hold {schedule['total']}")

    for position, move in enumerate(data["moves"]):
        name = move["name"]
        for number, output in enumerate(move["outputs"]):
            where = f"{label(position, move)}, output {number}"
            state, role = output["state"], output["role"]
            want = {"vesting": "TokenVesting", "token": "KCC20"}.get(role)
            if want is None:
                continue
            if state is None:
                problems.append(f"{where}: a {role} coin without a state: nothing shows what it is")
                continue
            if state.get("contract") != want:
                problems.append(f"{where}: a {role} coin whose state names the contract {state.get('contract')!r}, not {want!r}")
                continue
            (kcc20 if want == "KCC20" else vesting)(where, state)
        if move["kind"] == "step":
            made = [o["state"] for o in move["outputs"] if o["role"] == "vesting" and isinstance(o["state"], dict) and o["state"].get("contract") == "TokenVesting"]
            if name in RELEASED:
                released = RELEASED[name]
                if released is None and made:
                    problems.append(f"{label(position, move)}: makes a vesting coin; at this step the vesting coin ends")
                if released is not None and name != VAULT_MOVE and len(made) != 1:
                    problems.append(f"{label(position, move)}: makes {len(made)} vesting coins; it must make exactly one")
                for state in made:
                    if released is not None and state["released"] != released:
                        problems.append(f"{label(position, move)}: the vesting coin says {state['released']} released; at this step it is {released}")
            spent_tokens = sum(kcc20_amount(state_of_spent(data, i)) for i in move["inputs"])
            made_tokens = sum(kcc20_amount(o["state"]) for o in move["outputs"] if o["role"] == "token")
            if spent_tokens != made_tokens:
                problems.append(f"{label(position, move)}: spends {spent_tokens} tokens and makes {made_tokens}; a step must keep them")
        if name == VAULT_MOVE:
            vaults = [o for o in move["outputs"] if o["role"] == "token" and isinstance(o["state"], dict)
                      and o["state"].get("contract") == "KCC20" and o["state"].get("owner_scheme") == OWNER_COVENANT]
            if len(vaults) != 1:
                problems.append(f"{label(position, move)}: makes {len(vaults)} coins owned by the vesting covenant; the vault is exactly one")
            elif vaults[0]["state"]["amount"] != schedule["total"]:
                problems.append(f"{label(position, move)}: the vault holds {vaults[0]['state']['amount']} tokens, not {schedule['total']}")
        if name in CLAIMS:
            amount, owner = CLAIMS[name]
            claimed = [o["state"] for o in move["outputs"] if o["role"] == "token" and isinstance(o["state"], dict)
                       and o["state"].get("contract") == "KCC20" and o["state"].get("owner_scheme") == OWNER_KEY]
            got = sum(state["amount"] for state in claimed)
            owners = {keys.get(state["owner"], state["owner"]) for state in claimed}
            if got != amount or owners != {owner}:
                problems.append(
                    f"{label(position, move)}: claims {got} tokens for {sorted(owners) or 'nobody'}; this probe claims {amount} for {owner!r}"
                )
    for position, move in enumerate(data["moves"]):
        problems += output_shape(data, position, move, key_of)
    left = unspent_after_each_step(data)[-1].values()
    covenant = [coin for coin in left if coin["role"] in ("vesting", "token")]
    claims = sorted(
        coin["state"]["amount"] for coin in covenant
        if coin["role"] == "token" and isinstance(coin["state"], dict) and coin["state"].get("contract") == "KCC20"
        and coin["state"].get("owner_scheme") == OWNER_KEY and coin["state"].get("owner") == key_of.get("beneficiary")
    )
    if len(covenant) != len(END_CLAIMS) or claims != END_CLAIMS:
        problems.append(
            f"end: the covenant coins left unspent after the last step are {len(covenant)}, the beneficiary's token coins among them "
            f"hold {claims}; there must be exactly the beneficiary's three token coins of {END_CLAIMS}"
        )
    if any(coin["role"] == "vesting" for coin in left):
        problems.append("end: a vesting coin is still unspent after the last step; it ends at the last release")
    beneficiary, locked = 0, 0
    for coin in left:
        state = coin["state"] if coin["role"] == "token" else None
        if isinstance(state, dict) and state.get("contract") == "KCC20":
            if state.get("owner_scheme") == OWNER_KEY and state.get("owner") == key_of.get("beneficiary"):
                beneficiary += state["amount"]
            elif state.get("owner_scheme") == OWNER_COVENANT:
                locked += state["amount"]
    if beneficiary != schedule["total"] or locked:
        problems.append(f"end: the beneficiary ends with {beneficiary} tokens and the vault with {locked}; it must be {schedule['total']} and 0")
    return problems


VERIFY_FORMAT = "vesting-verify-report/1"
REPORT_SHAPE = {"format": "text", "vesting_covenant_id": "hex32", "mismatches": "u64", "outputs": "list"}
REPORT_OUTPUT_SHAPE = {"move": "text", "output": "u32", "role": "text", "script_public_key": "hex", "ok": "bool"}


def check_verify_report(data: dict, path: str | None) -> tuple:
    """l. The TokenVesting locks, as the verifier's report confirms them. Returns (problems, what was done, whether checked)."""
    if path is None:
        return [], "NOT CHECKED: no --verify-report given; the TokenVesting locks are not recomputed by this program", False
    try:
        report, sha256 = read_json(path)
    except Stop as stop:
        return [f"verify report: {stop.message}"], "NOT CHECKED", False
    problems: list = []
    if not shape(problems, "verify report", report, REPORT_SHAPE):
        return problems, "NOT CHECKED", False
    if report["format"] != VERIFY_FORMAT:
        problems.append(f"verify report: the format is {report['format']!r}, not {VERIFY_FORMAT!r}")
    if report["mismatches"] != 0:
        problems.append(f"verify report: it counts {report['mismatches']} mismatches")
    if report["vesting_covenant_id"] != data["vesting_covenant_id"]:
        problems.append(f"verify report: it is about the vesting covenant {report['vesting_covenant_id']}, the chain's is {data['vesting_covenant_id']}")
    chain = {(move["name"], number): output for move in data["moves"] for number, output in enumerate(move["outputs"])}
    listed: dict = {}
    for entry, item in enumerate(report["outputs"]):
        where = f"verify report, entry {entry}"
        if not shape(problems, where, item, REPORT_OUTPUT_SHAPE):
            continue
        key = (item["move"], item["output"])
        if key in listed:
            problems.append(f"{where}: lists output {item['output']} of {item['move']!r} twice")
            continue
        listed[key] = item
        output = chain.get(key)
        if output is None:
            problems.append(f"{where}: output {item['output']} of {item['move']!r} is not in the chain")
            continue
        if item["role"] != output["role"]:
            problems.append(f"{where}: output {item['output']} of {item['move']!r} is a {output['role']} output in the chain, the report says {item['role']}")
        if item["script_public_key"] != output["script_public_key"]:
            problems.append(f"{where}: output {item['output']} of {item['move']!r} carries another script in the chain than the report confirms")
        if item["ok"] is not True:
            problems.append(f"{where}: output {item['output']} of {item['move']!r} is not ok in the report")
    vesting = [key for key, output in chain.items() if output["role"] == "vesting"]
    for name, number in vesting:
        if (name, number) not in listed:
            problems.append(f"verify report: output {number} of {name!r}, a vesting coin, is not in the report")
    return problems, f"the verifier's report (SHA-256 {sha256}) confirms the {len(vesting)} TokenVesting locks", True


HARNESSES: dict = {}  # folder -> the loaded module: one load per run, so nothing is compiled twice


def load_harness(folder: str):
    """The harness module from `folder`, loaded without writing anything there."""
    path = (Path(folder) / "kcc20_harness.py").resolve()
    if str(path) in HARNESSES:
        return HARNESSES[str(path)]
    spec = importlib.util.spec_from_file_location("kcc20_harness", path)
    module = importlib.util.module_from_spec(spec)
    keep = sys.dont_write_bytecode
    sys.dont_write_bytecode = True  # the folder is somebody else's: no bytecode file is left in it
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = keep
    HARNESSES[str(path)] = module
    return module


def file_sha256(path: str) -> str | None:
    try:
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()
    except OSError:
        return None


def harness_sha256(folder: str) -> str | None:
    try:
        return hashlib.sha256((Path(folder) / "kcc20_harness.py").read_bytes()).hexdigest()
    except OSError:
        return None


STATE_BYTES = ("owner", "borrow_guard", "extension_commitment")


def kcc20_lock(harness, state: dict) -> str:
    """The lock of a KCC20 coin in `state`, recomputed by the harness, as hex."""
    fields = {name: value for name, value in state.items() if name != "contract"}
    for name in STATE_BYTES:
        if name in fields:
            fields[name] = bytes.fromhex(fields[name])
    return bytes(harness.lock("KCC20", fields)).hex()


def check_locks(data: dict, harness_folder: str | None) -> tuple:
    """j. With a harness: every KCC20 lock recomputed from its state, and the template hash. TokenVesting locks: not here.

    Returns (problems, what was done, whether the KCC20 locks were recomputed).
    """
    problems = []
    stated = [("token coin", data["token"]["state"], data["token"]["coin"]["script_public_key"])]
    vesting = 0
    for position, move in enumerate(data["moves"]):
        for number, output in enumerate(move["outputs"]):
            state = output["state"]
            if output["role"] == "token" and isinstance(state, dict) and state.get("contract") == "KCC20":
                stated.append((f"{label(position, move)}, output {number}", state, output["script_public_key"]))
            elif output["role"] == "vesting":
                vesting += 1
    not_here = f"the {vesting} TokenVesting locks are NOT recomputed by this program (check l)"
    if harness_folder is None:
        return problems, f"NOT CHECKED: no --harness given, no KCC20 lock was recomputed ({len(stated)} KCC20 coins); {not_here}", False
    try:
        harness = load_harness(harness_folder)
    except Exception as error:
        reason = getattr(error, "strerror", None) or str(error)
        return problems + [f"locks: the harness named by --harness could not be loaded ({type(error).__name__}: {reason})"], "NOT CHECKED", False
    try:
        template = bytes(harness.template_hash("KCC20")).hex()
        if template != data["token"]["template_hash"]:
            problems.append(f"token: template_hash {data['token']['template_hash']} is not the harness's template hash of KCC20 {template}")
    except Exception as error:
        problems.append(f"token: the harness could not give the template hash of KCC20 ({type(error).__name__}: {error})")
    for where, state, script in stated:
        try:
            lock = kcc20_lock(harness, state)
        except Exception as error:
            problems.append(f"{where}: the lock could not be recomputed from the state ({type(error).__name__}: {error})")
            continue
        if lock != script:
            problems.append(f"{where}: the lock recomputed from the state is {lock}, the coin carries {script}")
    return problems, f"KCC20 recomputed for {len(stated)} coins, the template hash compared; {not_here}", True


def party_of_key(data: dict, key: str) -> str:
    for name, party in data["parties"].items():
        if party["x_only_public_key"] == key:
            return name
    return "the vesting covenant" if key == data["vesting_covenant_id"] else f"key {key}"


def summary_lines(data: dict) -> list:
    """k. What the chain does, for a reader."""
    left = unspent_after_each_step(data)[-1].values()
    tokens: dict = {}
    for coin in left:
        state = coin["state"] if coin["role"] == "token" else None
        if isinstance(state, dict) and state.get("contract") == "KCC20":
            holder = party_of_key(data, state["owner"])
            tokens[holder] = tokens.get(holder, 0) + state["amount"]
    change = sum(coin["value"] for coin in left if coin["role"] == "change")
    locked = sum(coin["value"] for coin in left if coin["role"] in ("vesting", "token"))
    schedule = data["schedule"]
    fees = {kind: sum(m["fee"] for m in data["moves"] if m["kind"] == kind) for kind in ("step", "probe")}
    lines = [
        f"vesting id    : {data['vesting_covenant_id']}",
        f"token id      : {data['token']['covenant_id']}",
        f"funding coin  : {kas(data['funding']['value'])} KAS of {data['funding']['owner']!r}, {outpoint_text(funding_coin(data)['key'])}",
        f"token coin    : {outpoint_text(token_coin(data)['key'])}, {data['token']['state'].get('amount')} tokens",
        f"schedule      : start {schedule['start']}, cliff +{schedule['cliff']}, duration {schedule['duration']}, total {schedule['total']}",
        f"moves         : {len(steps_of(data))} steps, {len(data['moves']) - len(steps_of(data))} probes",
        f"fees          : {kas(fees['step'])} KAS over the steps, {kas(fees['probe'])} KAS over the probes "
        f"(cap of the file {kas(data['fee_cap_total'])}, feerate {data['feerate']} sompi per gram)",
        f"left locked   : {kas(locked)} KAS in covenant coins after the last step",
        f"left change   : {kas(change)} KAS to the payer",
    ]
    for holder in sorted(tokens):
        lines.append(f"tokens        : {holder} {tokens[holder]}")
    for position, move in enumerate(data["moves"]):
        window = f", not at or after {move['send_before_daa']}" if move["send_before_daa"] is not None else ""
        expected = move["refusal_expected"]
        words = f"; refused with {expected['node_message_contains']!r}" if expected else ""
        lines.append(
            f"  {move['kind']:<5} {move['name']}: lock time {move['lock_time']}, sent above {move['send_after_daa']}{window} "
            f"[fee {kas(move['fee'])}]{words}"
        )
    return lines


def totals_notes(data: dict) -> list:
    """Totals of the file other than the fees, against what the coins say. Notes, not refusals."""
    left = unspent_after_each_step(data)[-1].values()
    key = data["parties"].get("beneficiary", {}).get("x_only_public_key")
    mine = {
        "tokens_to_beneficiary": sum(
            coin["state"]["amount"] for coin in left
            if coin["role"] == "token" and isinstance(coin["state"], dict) and coin["state"].get("owner") == key
        ),
        "kas_locked_at_the_end": sum(coin["value"] for coin in left if coin["role"] in ("vesting", "token")),
        "change_at_the_end": sum(coin["value"] for coin in left if coin["role"] == "change"),
    }
    return [
        f"note: totals.{name} is {data['totals'][name]} in the file; counted from the coins it is {value}"
        for name, value in mine.items()
        if name in data["totals"] and data["totals"][name] != value
    ]


def check_offline(data, addresses: dict, harness_folder: str | None, report: str | None = None) -> tuple:
    """All offline checks. Prints one line per check and every refusal.

    Returns (the refusals, whether the KCC20 locks were recomputed). A file
    that lacks a field, or has one of the wrong kind, is refused on that
    alone: the other checks are not run on it.
    """
    problems = check_shape(data)
    if problems:
        say("check    shape of the file                             : REFUSED, the other checks were not run")
        for problem in problems:
            say(f"REFUSED  {problem}")
        return problems, False, False
    locks, locks_done, locks_checked = check_locks(data, harness_folder)
    verified, verified_done, verified_checked = check_verify_report(data, report)
    sections = [
        ("a  format, network, limits, launch checks, gates", check_basics(data), None, True),
        ("b  the moves, lock times, windows, refusals", check_moves(data), None, True),
        ("c  parties and their addresses", check_parties(data, addresses), None, True),
        ("d  transaction ids rebuilt by the SDK", check_rebuild(data), None, True),
        ("e  linkage of the coins", check_linkage(data), None, True),
        ("f  fees", check_money(data), None, True),
        ("g  outputs go to the contracts and the payer", check_outputs(data), None, True),
        ("h  vesting covenant id", *check_covenant_id(data), True),
        ("i  states, releases, tokens", check_states(data), None, True),
        ("j  locks from states", locks, locks_done, locks_checked),
        ("l  TokenVesting locks, the verifier's report", verified, verified_done, verified_checked),
    ]
    for title, found, done, checked in sections:
        verdict = f"REFUSED ({len(found)})" if found else "ok" if checked else "NOT CHECKED"
        if done and done.startswith("NOT CHECKED"):
            done = done.removeprefix("NOT CHECKED").lstrip(": ") if verdict == "NOT CHECKED" else done
        say(f"check {title:<50}: {verdict}{' - ' + done if done else ''}")
        problems += found
    for problem in problems:
        say(f"REFUSED  {problem}")
    if not problems:
        say("check k  summary")
        for line in summary_lines(data):
            say(f"  {line}")
        for note in totals_notes(data) + window_notes(data):
            say(f"  {note}")
    return problems, locks_checked, verified_checked


def no_repeated_keys(pairs: list) -> dict:
    """For the JSON reader: an object that names a key twice is refused, not read with the last one winning."""
    found: dict = {}
    for key, value in pairs:
        if key in found:
            raise ValueError(f"the key {key!r} is there twice in one object")
        found[key] = value
    return found


def read_json(path: str) -> tuple:
    """(the file as data, the SHA-256 of the bytes it was read from)."""
    try:
        raw = Path(path).read_bytes()
        return json.loads(raw, object_pairs_hook=no_repeated_keys), hashlib.sha256(raw).hexdigest()
    except OSError as error:
        raise Stop(EXIT_CHECK, f"Cannot read {path}: {error.strerror or error}") from None
    except ValueError as error:
        raise Stop(EXIT_CHECK, f"{path} does not read as JSON: {error}") from None


def party_addresses(pairs: list) -> dict:
    addresses = {}
    for pair in pairs:
        name, mark, address = pair.partition("=")
        if not mark or not name or not address:
            raise Stop(EXIT_CHECK, f"--party wants NAME=ADDRESS, got {pair!r}")
        if name in addresses:
            raise Stop(EXIT_CHECK, f"--party names {name!r} twice")
        if not address.startswith(ADDRESS_PREFIX):
            raise Stop(EXIT_CHECK, f"Refusing: the address of {name!r} does not start with {ADDRESS_PREFIX}")
        addresses[name] = address
    return addresses


# --------------------------------------------------------------------------
# Network: reading
# --------------------------------------------------------------------------


async def quietly_disconnect(client) -> None:
    try:
        await client.disconnect()
    except Exception:
        pass


def daa_of(info: dict) -> int | None:
    """The node's virtual DAA score from its server info, or None when it does not give one."""
    value = info.get("virtualDaaScore")
    if isinstance(value, bool):
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


async def check_node(client) -> dict:
    """Refuse a node that is not a synced testnet-10 node of version 2.1.0.

    Asked after every connection - the first one and each reconnect, which may
    land on another node - and right before every submission.
    """
    info = await asyncio.wait_for(client.get_server_info(), timeout=Timing.answer_timeout)
    if info.get("networkId") != NETWORK_ID:
        await quietly_disconnect(client)
        raise WrongNode(f"Refusing: the node reports network {info.get('networkId')!r}, not {NETWORK_ID!r}.")
    if info.get("serverVersion") != NODE_VERSION:
        await quietly_disconnect(client)
        raise WrongNode(f"Refusing: the node reports version {info.get('serverVersion')!r}, not {NODE_VERSION!r}.")
    if not info.get("isSynced"):
        await quietly_disconnect(client)
        raise WrongNode("Refusing: the node is not synced.")
    return info


async def connect():
    """A checked connection: (client, what the node said about itself)."""
    client = RpcClient(resolver=Resolver(), network_id=NETWORK_ID)
    await asyncio.wait_for(client.connect(), timeout=Timing.connect_timeout)
    info = await check_node(client)
    return client, info


async def open_node():
    """`connect`, with a failure to connect told in one line."""
    try:
        return await connect()
    except Stop:
        raise
    except Exception as error:
        raise Stop(EXIT_CHECK, f"Cannot connect to a node ({str(error) or type(error).__name__}). Nothing was sent.") from None


async def reading(work):
    """Await a read of the network; a read that fails for good ends the program in one line."""
    try:
        return await work
    except Stop:
        raise
    except Exception as error:
        raise Stop(EXIT_CHECK, f"The node could not be read ({str(error) or type(error).__name__}). Nothing was sent.") from None


async def with_retries(client, question):
    """Ask `question(client)`; public nodes drop connections now and then: retry a few times.

    After a failed read the program reconnects, and reads again only once the
    node it reached has said what it is. A reconnect that fails, or a node
    that does not answer that question, counts as one more failed attempt.
    """
    last_error = None
    checked = True  # the connection handed in was checked when it was made
    for attempt in range(Timing.read_attempts):
        try:
            if not checked:
                await asyncio.wait_for(client.connect(), timeout=Timing.connect_timeout)
                info = await check_node(client)  # a wrong or unsynced node stops the program here
                checked = True
                say(f"via     : {getattr(client, 'url', '?')}, node version {info.get('serverVersion')} (after a reconnect)")
                if CURRENT_RECORD is not None:
                    CURRENT_RECORD.version = str(info.get("serverVersion"))
                    shown = {k: info.get(k) for k in ("networkId", "serverVersion", "isSynced", "virtualDaaScore")}
                    note(CURRENT_RECORD, "reconnected", server_info=shown)
            return await question(client)
        except WrongNode:
            raise
        except Exception as error:
            last_error = error
            checked = False
            if Timing.read_pause_seconds:
                await asyncio.sleep(Timing.read_pause_seconds + attempt)
    raise last_error


async def utxos_at(client, address: str) -> list:
    """UTXOs at `address`, with retries."""

    async def question(node):
        result = await asyncio.wait_for(node.get_utxos_by_addresses({"addresses": [address]}), timeout=Timing.answer_timeout)
        return list(result["entries"])

    return await with_retries(client, question)


async def node_info(client) -> dict:
    """What the node says about itself, checked, with retries."""
    return await with_retries(client, check_node)


def entry_key(entry: dict) -> tuple:
    return (entry["outpoint"]["transactionId"], entry["outpoint"]["index"])


def entry_script(entry: dict) -> tuple:
    """(version, script hex) of a node's entry. A node gives 4 hex characters of version and then the script."""
    script = entry["utxoEntry"]["scriptPublicKey"]
    if isinstance(script, str):
        return int(script[:4], 16), script[4:]
    if isinstance(script, dict):
        return script["version"], script["script"]
    return script.version, script.script


def entry_shown(entry: dict) -> dict:
    """An entry as it goes into the record."""
    version, script = entry_script(entry)
    return {
        "outpoint": outpoint_text(entry_key(entry)),
        "amount": entry["utxoEntry"]["amount"],
        "covenant_id": entry["utxoEntry"].get("covenantId"),
        "script_public_key_version": version,
        "script_public_key": script,
        "block_daa_score": entry["utxoEntry"].get("blockDaaScore"),
    }


def entry_differences(entry: dict, coin: dict) -> list:
    """Where a node's entry differs from the coin the file expects at that outpoint."""
    found = []
    if entry["utxoEntry"]["amount"] != coin["value"]:
        found.append(f"amount {entry['utxoEntry']['amount']} instead of {coin['value']}")
    if (entry["utxoEntry"].get("covenantId") or None) != coin["covenant_id"]:
        found.append(f"covenant id {entry['utxoEntry'].get('covenantId')} instead of {coin['covenant_id']}")
    if entry_script(entry) != (coin["version"], coin["script"]):
        found.append("another script")
    return found


async def find_coins(client, coins: list) -> tuple:
    """Look for `coins` on chain, each at the address of its script.

    Returns (entries, wrong): `entries[i]` is the node's entry of `coins[i]`
    or None when it is not there; `wrong` lists coins found at their outpoint
    with another amount, covenant id or script, and coins a node lists twice.
    """
    by_address: dict = {}
    repeated: set = set()
    entries, wrong = [], []
    for coin in coins:
        address = address_of_script(coin["version"], coin["script"])
        if address not in by_address:
            listed = await utxos_at(client, address)
            by_address[address] = {entry_key(entry): entry for entry in listed}
            keys = [entry_key(entry) for entry in listed]
            repeated |= {key for key in keys if keys.count(key) > 1}
        entry = by_address[address].get(coin["key"])
        if entry is not None:
            differences = entry_differences(entry, coin)
            if coin["key"] in repeated:
                differences.append("more than one entry for this outpoint in the node's answer")
            if differences:
                wrong.append(f"{coin['label']} ({outpoint_text(coin['key'])}) is on chain with {', '.join(differences)}")
        entries.append(entry)
    return entries, wrong


async def balances(client, addresses: dict) -> dict:
    """What sits at each party's address: {name: {"address", "sompi", "coins"}}."""
    result = {}
    for name, address in addresses.items():
        entries = await utxos_at(client, address)
        result[name] = {"address": address, "sompi": sum(entry["utxoEntry"]["amount"] for entry in entries), "coins": len(entries)}
    return result


def say_balances(title: str, found: dict) -> None:
    for name, at in found.items():
        say(f"{title}: {name:<11} {kas(at['sompi'])} KAS in {at['coins']} coin(s) at {at['address']}")


async def fee_estimate(client) -> dict:
    return (await asyncio.wait_for(client.get_fee_estimate(), timeout=Timing.answer_timeout))["estimate"]


def say_estimate(estimate: dict) -> None:
    say(f"estimate: priority bucket {estimate.get('priorityBucket')}")
    say(f"estimate: normal buckets  {estimate.get('normalBuckets')}")
    say(f"estimate: low buckets     {estimate.get('lowBuckets')}")


class Picture:
    """Which coins of the chain are on chain right now. Only steps change it; a probe is never meant to be on chain."""

    def __init__(self, data: dict, visible: dict, wrong: list):
        self.visible = visible  # outpoint -> the node's entry, coins of the chain only
        self.wrong = wrong
        steps = steps_of(data)
        self.after = unspent_after_each_step(data)
        made = {coin["key"] for step in steps for coin in output_coins(step)}
        seen = set(visible) & (made | set(starting_coins(data)))
        # A step is proven to be on chain when one of its outputs is there, or a proven later step spends one.
        self.proven = [False] * len(steps)
        for position in range(len(steps) - 1, -1, -1):
            own = {coin["key"] for coin in output_coins(steps[position])}
            spent_later = any(self.proven[later] and own & set(spent_keys(steps[later])) for later in range(position + 1, len(steps)))
            self.proven[position] = bool(own & seen) or spent_later
        # How many steps are on chain, if what is visible is exactly what is unspent after that many.
        self.prefix = next((count for count, coins in enumerate(self.after) if set(coins) == seen), None)
        self.probe_outputs = [
            coin for probe in data["moves"] if probe["kind"] == "probe" for coin in output_coins(probe)
            if coin["key"] in visible and coin["key"] not in made
        ]

    def proven_after(self, data: dict, position: int) -> bool:
        """Whether a step that comes after the move at `position` is on chain."""
        return any(self.proven[steps_before(data, position):])


async def read_picture(client, data: dict) -> Picture:
    coins = list(starting_coins(data).values())
    for transaction in data["moves"]:
        coins += output_coins(transaction)
    unique = list({coin["key"]: coin for coin in coins}.values())
    entries, wrong = await find_coins(client, unique)
    visible = {coin["key"]: entry for coin, entry in zip(unique, entries) if entry is not None}
    return Picture(data, visible, wrong)


def picture_lines(data: dict, picture: Picture) -> tuple:
    """(lines for a reader, problems) about what is on chain."""
    lines, problems = [], list(picture.wrong)
    steps = steps_of(data)
    for coin in picture.probe_outputs:
        problems.append(f"FORBIDDEN MOVE on chain: {coin['label']} ({outpoint_text(coin['key'])}) is visible")
    if not picture.visible:
        problems.append(
            "neither the funding coin, nor the token coin, nor any coin of this chain is on chain "
            f"(funding {outpoint_text(funding_coin(data)['key'])}, token coin {outpoint_text(token_coin(data)['key'])})"
        )
    elif picture.prefix is None:
        proven = [steps[i]["name"] for i in range(len(steps)) if picture.proven[i]]
        problems.append(
            "the coins visible on chain are not what is unspent after any number of steps of this file "
            f"(funding coin visible: {funding_coin(data)['key'] in picture.visible}; token coin visible: "
            f"{token_coin(data)['key'] in picture.visible}; steps with visible traces: {proven or 'none'}). "
            "A transaction that is still on its way gives the same picture: look again in a minute."
        )
    elif picture.prefix == 0:
        lines.append("on chain: the funding coin and the token coin are unspent, with the stated value, script and covenant id; no step is on chain")
    else:
        done = ", ".join(repr(step["name"]) for step in steps[: picture.prefix])
        lines.append(f"on chain: the first {picture.prefix} of {len(steps)} steps are on chain ({done})")
    return lines, problems


def daa_lines(data: dict, score: int | None) -> list:
    """The node's virtual DAA score against the schedule and the sending windows."""
    if score is None:
        return ["DAA     : the node did not give its virtual DAA score"]
    start = data["schedule"]["start"]
    lines = [f"DAA     : virtual DAA score {score}; schedule start {start} ({'passed' if score >= start else f'{start - score} to go'})"]
    for move in data["moves"]:
        before = move["send_before_daa"]
        if before is None:
            continue
        after = move["send_after_daa"]
        if before <= after + 1:
            state = "empty: it can never be sent"
        elif score >= before:
            state = "PASSED: it will not be sent"
        else:
            state = f"open from above {after} to below {before}, {before - score} left"
        lines.append(f"window  : {move['name']!r}: {state}")
    return lines


async def check_online(client, data: dict, info: dict) -> tuple:
    """The read-only network checks of `check --online`. Returns (problems, picture)."""
    estimate = await fee_estimate(client)
    say_estimate(estimate)
    suggested = max(MIN_FEERATE, math.ceil(estimate["priorityBucket"]["feerate"]))
    say(f"feerate : the file was built at {data['feerate']} sompi per gram; the node's priority estimate now gives {suggested}")
    if suggested > data["feerate"]:
        say("feerate : NOTE - the node now asks for more than the file pays; a move may wait or be refused for its fee")
    for line in daa_lines(data, daa_of(info)):
        say(line)
    picture = await read_picture(client, data)
    lines, problems = picture_lines(data, picture)
    if not problems and picture.prefix == 0:
        # Nothing is on chain yet. The first step is built exactly as `send` would build it, with the
        # node's own entry of the funding coin on its input, and not submitted.
        first = data["moves"][0]
        try:
            to_submit(first, [picture.visible[key] for key in spent_keys(first)])
            lines.append(f"built   : {first['name']!r} builds for sending from the node's own entries; not submitted")
        except Stop as stop:
            problems.append(stop.message)
        except Exception as error:
            problems.append(f"{first['name']!r} could not be built for sending from the node's own entries ({type(error).__name__}: {error})")
    for line in lines:
        say(line)
    for problem in problems:
        say(f"REFUSED  {problem}")
    return problems, picture


# --------------------------------------------------------------------------
# Network: waiting for a DAA score, submitting
# --------------------------------------------------------------------------


class DaaRate:
    """The DAA rate measured over a run: from its first reading to its last."""

    def __init__(self):
        self.first = None
        self.last = None

    def saw(self, score: int) -> None:
        now = Timing.clock()
        if self.first is None:
            self.first = (now, score)
        self.last = (now, score)

    def per_second(self) -> float:
        if self.first and self.last:
            seconds = self.last[0] - self.first[0]
            gained = self.last[1] - self.first[1]
            if seconds >= Timing.rate_min_seconds and gained > 0:
                return gained / seconds
        return Timing.nominal_daa_rate


async def daa_now(client, rate: DaaRate) -> int:
    info = await node_info(client)
    score = daa_of(info)
    if score is None:
        raise Stop(EXIT_NODE, "STOP: the node did not give its virtual DAA score. Nothing more was submitted.")
    rate.saw(score)
    return score


async def wait_for_daa(client, record, rate: DaaRate, title: str, move: dict) -> tuple:
    """Wait until the node's virtual DAA score is above the move's send_after_daa.

    Returns ("ready", score), or ("passed", score) when the score is at or
    above the move's send_before_daa. Gives up (exit 9) when the wait exceeds
    what the measured DAA rate predicts plus Timing.daa_slack_seconds.
    """
    after, before = move["send_after_daa"], move["send_before_daa"]
    score = await daa_now(client, rate)
    if before is not None and score >= before:
        return "passed", score
    if score > after:
        return "ready", score
    per_second = rate.per_second()
    predicted = (after - score) / per_second
    allowed = predicted + Timing.daa_slack_seconds
    started = last_told = Timing.clock()
    say(
        f"{title}: waiting for a DAA score above {after}; now {score}, about {predicted:.0f} s at {per_second:.2f} per second; "
        f"giving up after {allowed:.0f} s"
    )
    note(record, "waiting for DAA", move=move["name"], transaction_id=move["transaction_id"], daa_score=score,
         send_after_daa=after, send_before_daa=before, daa_per_second=round(per_second, 3), seconds_allowed=round(allowed, 1))
    while True:
        await asyncio.sleep(Timing.daa_poll_seconds)
        score = await daa_now(client, rate)
        if before is not None and score >= before:
            return "passed", score
        if score > after:
            note(record, "DAA reached", move=move["name"], transaction_id=move["transaction_id"], daa_score=score,
                 seconds_waited=round(Timing.clock() - started, 1))
            return "ready", score
        now = Timing.clock()
        if now - started > allowed:
            note(record, "DAA wait given up", move=move["name"], transaction_id=move["transaction_id"], daa_score=score,
                 send_after_daa=after, seconds_waited=round(now - started, 1))
            raise Stop(
                EXIT_TIMING,
                f"STOP at {title}: after {now - started:.0f} s the DAA score is {score}, not above {after}; the measured rate "
                f"predicted {predicted:.0f} s. Nothing more was submitted. Run `status`; `send` with the same --record folder goes on.",
            )
        if now - last_told >= Timing.progress_seconds:
            last_told = now
            say(f"{title}: DAA score {score}, {after - score + 1} to go ({now - started:.0f} s waited)")


async def ready_to_hand_over(client, move: dict) -> int:
    """Right before a hand-over: the node is what it must be, and its DAA score is inside the move's window. Returns the score."""
    try:
        info = await check_node(client)
    except WrongNode as wrong:
        raise Stop(EXIT_NODE, f"Not sent: {wrong.message}") from wrong
    except Exception as error:
        raise Stop(EXIT_NODE, f"Not sent: the node did not say what it is ({str(error) or type(error).__name__}).") from error
    score = daa_of(info)
    if score is None:
        raise Stop(EXIT_NODE, "Not sent: the node did not give its virtual DAA score.")
    before = move["send_before_daa"]
    if before is not None and score >= before:
        raise WindowPassed(score)
    if score <= move["send_after_daa"]:
        raise NotYet(score)
    return score


def node_words(answer: str, txid: str) -> str:
    """What a node says after "Rejected transaction <id>" about this very transaction."""
    return answer.split(f"{REFUSAL_MARK} {txid}", 1)[-1]


async def hand_over(client, built: Transaction, txid: str) -> tuple:
    """Return (accepted, the id the node returns or the node's refusal).

    Only the node's own answer counts as a refusal. Any other failure - a
    dropped connection, a timeout - raises `NoAnswer`: the transaction may
    have been accepted all the same.
    """
    try:
        handed = client.submit_transaction({"transaction": built, "allowOrphan": False})
        result = await asyncio.wait_for(handed, timeout=Timing.answer_timeout)
        return True, str(result["transactionId"])
    except asyncio.CancelledError:
        raise
    except Exception as error:  # the node's refusal arrives as an exception too
        reason = str(error) or type(error).__name__
        if f"{REFUSAL_MARK} {txid}" in reason:  # only the node's refusal of this very transaction is a refusal
            return False, reason
        raise NoAnswer(reason) from error


async def wait_for_coins(client, coins: list, polls: int) -> tuple:
    """Look for `coins` until all are there or `polls` looks are used up. Returns (entries, wrong, seconds)."""
    started = time.monotonic()
    limit = polls * Timing.poll_seconds  # a look takes time too: the clock ends the wait as well as the count
    entries, wrong = [None] * len(coins), []
    for look in range(polls):
        entries, wrong = await find_coins(client, coins)
        if wrong or all(entry is not None for entry in entries):
            break
        if look == polls - 1 or time.monotonic() - started >= limit:
            break
        await asyncio.sleep(Timing.poll_seconds)
    return entries, wrong, round(time.monotonic() - started, 1)


class Record:
    """The record of a run: one JSON object per line, written as things happen.

    Events go in through `note`: a line that cannot be written is printed
    instead and the record is marked broken; nothing more is submitted then.
    """

    def __init__(self, folder: Path, client, version: str):
        folder.mkdir(parents=True, exist_ok=True)
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        self.path = folder / f"run-{stamp}.jsonl"
        self.summary_path = folder / f"run-{stamp}-summary.json"
        self.client, self.version = client, version
        self.broken = False

    def write(self, what: str, **fields) -> None:
        event = {"time_utc": utc_now(), "node": getattr(self.client, "url", None), "node_version": self.version, "what": what, **fields}
        with open(self.path, "a", encoding="utf-8", newline="\n") as file:
            file.write(json.dumps(event, ensure_ascii=False) + "\n")
            file.flush()
            os.fsync(file.fileno())

    def must_be_whole(self, before: str) -> None:
        if self.broken:
            raise Stop(EXIT_RECORD, f"STOP before {before}: the record could not be written. Nothing more was submitted.")


def note(record: Record, what: str, **fields) -> None:
    """Write an event into the record. One that cannot be written goes on screen instead, and the record is marked broken."""
    try:
        record.write(what, **fields)
    except Exception as error:
        record.broken = True
        reason = getattr(error, "strerror", None) or str(error)
        shown = json.dumps({"what": what, **fields}, ensure_ascii=False, default=str)
        say(f"RECORD NOT WRITTEN ({type(error).__name__}: {reason}): {shown}")


def write_summary(record: Record, summary: dict) -> None:
    """The short summary of a run, next to its record. A summary that cannot be written is printed instead."""
    text = json.dumps(summary, indent=2, ensure_ascii=False, default=str) + "\n"
    try:
        with open(record.summary_path, "w", encoding="utf-8", newline="\n") as file:
            file.write(text)
    except Exception as error:
        reason = getattr(error, "strerror", None) or str(error)
        say(f"SUMMARY NOT WRITTEN ({type(error).__name__}: {reason}):")
        say(text)


def shown_path(path: Path) -> str:
    """A path for the screen: relative to the current folder when it lies under it, else its name alone."""
    try:
        return str(path.resolve().relative_to(Path.cwd().resolve()))
    except ValueError:
        return path.name


WATCHED = (
    "step accepted", "step refused", "step no answer",
    "probe accepted", "probe already with a node", "probe no answer", "probe refused", "probe refused for another reason",
)
STEP_UNKNOWN = "step handed over, outcome unknown"
PROBE_UNKNOWN = "probe handed over, outcome unknown"


def earlier_events(folder: Path) -> dict:
    """What earlier runs recorded in `folder`: for each watched event, the ids of the transactions it was written for.

    Two more sets hold the hand-overs whose outcome never reached the record
    (the program was interrupted or killed, or the record broke): the last
    line about them is "submitting" or "interrupted".
    """
    found = {what: set() for what in WATCHED + (STEP_UNKNOWN, PROBE_UNKNOWN)}
    if not folder.is_dir():
        return found
    for path in sorted(folder.glob("run-*.jsonl")):  # the names carry the time: oldest first
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if not isinstance(event, dict) or not isinstance(event.get("transaction_id"), str):
                continue
            what, txid = event.get("what"), event["transaction_id"]
            if what in WATCHED:
                found[what].add(txid)
            # A hand-over is open from "submitting" (or an interrupt) until its outcome is written.
            if what in ("step submitting", "step interrupted"):
                found[STEP_UNKNOWN].add(txid)
            elif what in ("step accepted", "step refused", "step no answer"):
                found[STEP_UNKNOWN].discard(txid)
            elif what in ("probe submitting", "probe interrupted"):
                found[PROBE_UNKNOWN].add(txid)
            elif isinstance(what, str) and what.startswith("probe refused"):
                found[PROBE_UNKNOWN].discard(txid)
    return found


def inputs_shown(transaction: dict) -> list:
    """The inputs as they go into the record: a signature script by its first 16 characters and its length."""
    return [
        {
            "outpoint": outpoint_text((tx_input["transaction_id"], tx_input["index"])),
            "compute_budget": tx_input["compute_budget"],
            "signature_script_starts": tx_input["signature_script"][:16],
            "signature_script_bytes": len(tx_input["signature_script"]) // 2,
        }
        for tx_input in transaction["inputs"]
    ]


def to_submit(transaction: dict, entries: list) -> Transaction:
    """The transaction with the node's own entries on its inputs, checked once more against the file."""
    built = build_transaction(transaction, entries)
    wrong = differences_outside_the_id(built, transaction)
    if built.id != transaction["transaction_id"]:
        wrong.append(f"its id is {built.id}, the file says {transaction['transaction_id']}")
    if wrong:
        raise Stop(EXIT_INCONSISTENT, f"STOP before {transaction['name']!r}: built for sending it differs from the file: {'; '.join(wrong)}")
    return built


class Run:
    """What a `send` knows while it goes: the record, the picture at its start, earlier records, the DAA rate."""

    def __init__(self, client, record: Record, data: dict, picture: Picture, earlier: dict, again: dict):
        self.client, self.record, self.data, self.picture = client, record, data, picture
        self.earlier, self.again = earlier, again
        self.rate = DaaRate()
        self.outcomes: list = []
        self.current = None  # the move under way

    def outcome(self, move: dict, outcome: str, **more) -> None:
        self.outcomes.append({"name": move["name"], "kind": move["kind"], "transaction_id": move["transaction_id"], "outcome": outcome, **more})

    def stopped_at(self, outcome: str) -> None:
        """The move the run ended at, for the summary, unless it has an outcome already."""
        if self.current is not None and all(item["name"] != self.current["name"] for item in self.outcomes):
            self.outcome(self.current, outcome)


async def wait_and_check(run: Run, move: dict, title: str, step: bool):
    """Wait for the move's time, then look up the coins it spends.

    Returns ("passed", None, score) when the move's window closed during the
    wait, else ("ready", found, score). For a step `found` is (entries, wrong)
    and the caller decides; for a probe it is the entries, and a coin that is
    not unspent on chain as the file says stops the run here.
    """
    state, score = await wait_for_daa(run.client, run.record, run.rate, title, move)
    if state == "passed":
        return "passed", None, score
    inputs = [spent_coin(tx_input) for tx_input in move["inputs"]]
    entries, wrong = await find_coins(run.client, inputs)
    if step:
        return "ready", (entries, wrong), score
    missing = [f"{outpoint_text(coin['key'])} ({coin['label']})" for coin, entry in zip(inputs, entries) if entry is None]
    if wrong or missing:
        note(run.record, "stopped", move=move["name"], transaction_id=move["transaction_id"],
             reason="inputs not on chain as expected", missing=missing, details=wrong)
        raise Stop(
            EXIT_INCONSISTENT,
            f"STOP at {title}: its inputs are not unspent on chain as expected (not found: {missing or 'none'}; "
            f"differing: {wrong or 'none'}). The chain is not what the file expects; it was not submitted.",
        )
    return "ready", entries, score


async def send_step(run: Run, position: int) -> str:
    """Send one step, or see that it is on chain already.

    Returns "sent" when it was handed over (now or in an earlier run) and its
    outputs were seen, "found" when it was on chain already; raises Stop
    otherwise.
    """
    data, record, client = run.data, run.record, run.client
    step = data["moves"][position]
    name, txid = step["name"], step["transaction_id"]
    title = f"step {name!r}"
    inputs = [spent_coin(tx_input) for tx_input in step["inputs"]]
    outputs = output_coins(step)
    proven = run.picture.proven[steps_before(data, position)]
    for _ in range(Timing.not_yet_rounds):
        state, seen, score = await wait_and_check(run, step, title, step=True)
        if state != "ready":
            raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: a step with a closing window")
        entries, wrong = seen
        if wrong:
            note(record, "stopped", step=name, transaction_id=txid, reason="inputs differ on chain", details=wrong)
            raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: {'; '.join(wrong)}. Nothing was submitted.")
        missing = [coin for coin, entry in zip(inputs, entries) if entry is None]
        if missing:
            own, own_wrong = await find_coins(client, outputs)
            if own_wrong:
                note(record, "stopped", step=name, transaction_id=txid, reason="outputs differ on chain", details=own_wrong)
                raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: {'; '.join(own_wrong)}. Nothing was submitted.")
            seen_own = [entry for entry in own if entry is not None]
            why = None
            if seen_own:
                why = f"{len(seen_own)} of its {len(outputs)} outputs are visible"
            elif proven:
                why = "a later step that spends its outputs is on chain"
            if why is None:
                found = [outpoint_text(coin["key"]) for coin, entry in zip(inputs, entries) if entry is not None]
                lacking = [f"{outpoint_text(coin['key'])} ({coin['label']})" for coin in missing]
                note(record, "stopped", step=name, transaction_id=txid, reason="inputs not on chain", found=found, missing=lacking)
                raise Stop(
                    EXIT_INCONSISTENT,
                    f"STOP at {title}: not all of its inputs are on chain, and nothing on chain shows that it was sent. "
                    f"Found: {found or 'none'}. Not found: {lacking}. None of its outputs is visible. Nothing was submitted.",
                )
            say(f"{title}: already done, not sent ({why})")
            note(record, "step already done", step=name, transaction_id=txid, reason=why, entries_seen=[entry_shown(e) for e in seen_own])
            return "found"
        if txid in run.earlier["step accepted"]:
            # Handed over and accepted in an earlier run, but its outputs were not seen then. Never sent twice.
            say(f"{title}: an earlier run recorded it as accepted; not sent again, looking for its outputs")
            note(record, "step accepted earlier, waiting", step=name, transaction_id=txid)
            break
        built = to_submit(step, entries)
        try:
            score = await ready_to_hand_over(client, step)
        except NotYet:
            continue
        except WindowPassed:  # a step has no window; kept for completeness
            raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: a step with a closing window") from None
        say(f"{title}: submitting {txid} at DAA score {score} (lock time {step['lock_time']}, fee {kas(step['fee'])} KAS)")
        if txid in run.again:
            say(f"{title}: HANDED OVER AGAIN by the operator's decision (--again-after-a-stop); an earlier run recorded {run.again[txid]!r}")
        record.must_be_whole(title)  # a record broken earlier: stop before a "submitting" line that nothing follows
        note(
            record, "step submitting", step=name, transaction_id=txid, daa_score=score, lock_time=step["lock_time"],
            fee=step["fee"], mass=step["mass"], inputs=inputs_shown(step), again_by_the_operator_after=run.again.get(txid),
        )
        record.must_be_whole(title)
        try:
            accepted, answer = await hand_over(client, built, txid)
        except asyncio.CancelledError:
            # An interrupt landed on the hand-over. Recorded, and never gone on from: the run ends here.
            INTERRUPTED.update(what=title, transaction_id=txid)
            note(record, "step interrupted", step=name, transaction_id=txid)
            raise
        except NoAnswer as lost:
            say(f"{title}: NO ANSWER ({lost}); looking for its outputs")
            note(record, "step answer lost", step=name, transaction_id=txid, answer=str(lost))
            try:
                found, _, seconds = await wait_for_coins(client, outputs, Timing.lost_polls)
            except asyncio.CancelledError:
                INTERRUPTED.update(what=title, transaction_id=txid)
                note(record, "step interrupted", step=name, transaction_id=txid, during="the look-up after a lost answer")
                raise
            except Stop:
                note(record, "step no answer", step=name, transaction_id=txid, answer=str(lost), seconds_looked=None)
                raise
            except Exception as error:
                reason = getattr(error, "message", None) or str(error) or type(error).__name__
                found, seconds = [None] * len(outputs), None
                say(f"{title}: the look-up then stopped ({reason})")
            if not any(entry is not None for entry in found):
                note(record, "step no answer", step=name, transaction_id=txid, answer=str(lost), seconds_looked=seconds)
                raise Stop(
                    EXIT_NO_ANSWER,
                    f"STOP at {title}: NO ANSWER from the node ({lost}), and its outputs did not show up. "
                    f"Transaction {txid} may have been accepted all the same. Run `status` later. A later `send` "
                    "with the same --record folder goes on by itself only if this step is then on chain; otherwise "
                    "it hands the step over again only with --again-after-a-stop.",
                ) from None
            say(f"{title}: its outputs are on chain, so the node had accepted it")
            note(record, "step accepted", step=name, transaction_id=txid, answer=None, how="outputs seen after a lost answer")
            break
        if not accepted:
            say(f"{title}: REFUSED by the node")
            say(f"reason  : {answer}")
            if "already" in node_words(answer, txid).lower():
                say("hint    : the node seems to have this transaction already; run `status`, and `send` again once its outputs are visible")
            note(record, "step refused", step=name, transaction_id=txid, answer=answer, daa_score=score)
            raise Stop(
                EXIT_REFUSED,
                f"STOP at {title}: the node refused it. Nothing after it was submitted. A later `send` with the "
                "same --record folder does not hand it over again unless --again-after-a-stop is given.",
            )
        if answer != txid:
            note(record, "stopped", step=name, transaction_id=txid, reason="the node returned another id", answer=answer)
            raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: the node accepted it under the id {answer}, the file expects {txid}.")
        say(f"{title}: ACCEPTED {answer}")
        note(record, "step accepted", step=name, transaction_id=txid, answer=answer, how="the node's answer", daa_score=score)
        break
    else:
        raise Stop(EXIT_TIMING, f"STOP at {title}: the node's DAA score fell back below the step's time {Timing.not_yet_rounds} times.")

    try:
        found, found_wrong, seconds = await wait_for_coins(client, outputs, Timing.seen_polls)
    except Exception as error:
        reason = getattr(error, "message", None) or str(error) or type(error).__name__
        note(record, "step not seen", step=name, transaction_id=txid, reason=f"the look-up stopped: {reason}")
        raise Stop(EXIT_NOT_SEEN, f"STOP at {title}: submitted, not seen - the look-up stopped ({reason}). Not submitted again. Run `status` later.") from None
    if found_wrong:
        note(record, "stopped", step=name, transaction_id=txid, reason="outputs differ on chain", details=found_wrong)
        raise Stop(EXIT_INCONSISTENT, f"STOP at {title}: {'; '.join(found_wrong)}")
    if not all(entry is not None for entry in found):
        lacking = [outpoint_text(coin["key"]) for coin, entry in zip(outputs, found) if entry is None]
        note(record, "step not seen", step=name, transaction_id=txid, seconds_looked=seconds, missing=lacking)
        raise Stop(
            EXIT_NOT_SEEN,
            f"STOP at {title}: submitted, not seen - after {seconds} s these outputs are not visible: {lacking}. "
            "Not submitted again. Run `status` later; `send` with the same --record folder waits for them without sending "
            "twice. Do not change the --record folder in the middle of a chain (README); if the node has dropped the "
            "transaction, stop and decide by hand.",
        )
    say(f"{title}: all {len(outputs)} outputs seen after {seconds} s with the expected value, script and covenant id")
    note(record, "step seen", step=name, transaction_id=txid, seconds_until_seen=seconds, entries_seen=[entry_shown(e) for e in found])
    return "sent"


async def send_probe(run: Run, position: int) -> str:
    """Send one forbidden move. Returns its outcome; raises Stop when a probe is taken or the chain is not as expected."""
    data, record, client = run.data, run.record, run.client
    probe = data["moves"][position]
    name, txid = probe["name"], probe["transaction_id"]
    title = f"probe {name!r}"

    def not_sent(reason: str, score: int | None, outcome: str) -> str:
        say(f"{title}: NOT SENT ({reason})")
        note(record, "probe not sent", probe=name, transaction_id=txid, reason=reason, daa_score=score)
        return outcome

    if run.picture.proven_after(data, position):
        return not_sent("a later step is on chain already: the coins it spends are gone", None, "not sent, the chain is past it")
    earlier = [what for what in ("probe refused", "probe refused for another reason") if txid in run.earlier[what]]
    if earlier:
        return not_sent(f"an earlier run with this record saw it refused ({earlier[0]}); a probe is not sent twice", None, "not sent, refused earlier")
    after, before = probe["send_after_daa"], probe["send_before_daa"]
    if before is not None and before <= after + 1:
        return not_sent(f"window empty: it may go only above {after} and below {before}", None, "not sent, window empty")
    for _ in range(Timing.not_yet_rounds):
        state, entries, score = await wait_and_check(run, probe, title, step=False)
        if state == "passed":
            return not_sent(f"window passed: the DAA score {score} is at or above {before}", score, "not sent, window passed")
        built = to_submit(probe, entries)
        try:
            score = await ready_to_hand_over(client, probe)
        except NotYet:
            continue
        except WindowPassed as passed:
            return not_sent(f"window passed: the DAA score {passed.args[0]} is at or above {before}", passed.args[0], "not sent, window passed")
        break
    else:
        raise Stop(EXIT_TIMING, f"STOP at {title}: the node's DAA score fell back below the probe's time {Timing.not_yet_rounds} times.")
    expected = probe["refusal_expected"]["node_message_contains"]
    say(f"{title}: submitting {txid} at DAA score {score} (lock time {probe['lock_time']}), expecting a refusal containing {expected!r}")
    record.must_be_whole(title)  # a record broken earlier: stop before a "submitting" line that nothing follows
    note(record, "probe submitting", probe=name, transaction_id=txid, daa_score=score, lock_time=probe["lock_time"], inputs=inputs_shown(probe))
    record.must_be_whole(title)
    outputs = output_coins(probe)
    try:
        accepted, answer = await hand_over(client, built, txid)
    except asyncio.CancelledError:
        INTERRUPTED.update(what=title, transaction_id=txid)
        note(record, "probe interrupted", probe=name, transaction_id=txid)
        raise
    except NoAnswer as lost:
        say(f"{title}: NO ANSWER ({lost}); looking for its outputs")
        note(record, "probe answer lost", probe=name, transaction_id=txid, answer=str(lost))
        try:
            found, _, seconds = await wait_for_coins(client, outputs, Timing.lost_polls)
        except asyncio.CancelledError:
            INTERRUPTED.update(what=title, transaction_id=txid)
            note(record, "probe interrupted", probe=name, transaction_id=txid, during="the look-up after a lost answer")
            raise
        except Stop:
            note(record, "probe no answer", probe=name, transaction_id=txid, answer=str(lost), seconds_looked=None)
            raise
        except Exception as error:
            reason = getattr(error, "message", None) or str(error) or type(error).__name__
            found, seconds = [None] * len(outputs), None
            say(f"{title}: the look-up then stopped ({reason})")
        if any(entry is not None for entry in found):
            say("FORBIDDEN MOVE ACCEPTED")
            note(record, "probe accepted", probe=name, transaction_id=txid, answer=None, how="outputs seen after a lost answer",
                 entries_seen=[entry_shown(e) for e in found if e is not None])
            raise Stop(EXIT_FORBIDDEN_ACCEPTED, f"STOP at {title}: FORBIDDEN MOVE ACCEPTED - its outputs are on chain. Nothing more was submitted in this run, and no later run with this record starts.") from None
        note(record, "probe no answer", probe=name, transaction_id=txid, answer=str(lost), seconds_looked=seconds)
        raise Stop(EXIT_NO_ANSWER, f"STOP at {title}: NO ANSWER from the node ({lost}), and its outputs did not show up. Run `status` later.") from None
    if accepted:
        say("FORBIDDEN MOVE ACCEPTED")
        note(record, "probe accepted", probe=name, transaction_id=txid, answer=answer, how="the node's answer", daa_score=score)
        raise Stop(EXIT_FORBIDDEN_ACCEPTED, f"STOP at {title}: FORBIDDEN MOVE ACCEPTED by the node as {answer}. Nothing more was submitted in this run, and no later run with this record starts.")
    words = node_words(answer, txid)
    if "already" in words.lower():
        # Not a refusal of the move: the node holds this transaction, or another one that spends the same coin.
        say("FORBIDDEN MOVE MAY BE WITH A NODE")
        say(f"reason  : {answer}")
        note(record, "probe already with a node", probe=name, transaction_id=txid, answer=answer)
        raise Stop(
            EXIT_FORBIDDEN_ACCEPTED,
            f"STOP at {title}: the node answers that it already has this transaction or one that spends the same coin: "
            "the probe is in a node's mempool or on chain. Nothing more was submitted. Look with `status`.",
        )
    if expected in words:
        say(f"{title}: REFUSED as expected")
        note(record, "probe refused", probe=name, transaction_id=txid, answer=answer, expected=expected, daa_score=score)
        outcome = "refused as expected"
    else:
        say(f"{title}: REFUSED FOR ANOTHER REASON - the answer does not contain {expected!r}")
        note(record, "probe refused for another reason", probe=name, transaction_id=txid, answer=answer, expected=expected, daa_score=score)
        outcome = "refused for another reason"
    say(f"reason  : {answer}")
    return outcome


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------


def outpoint_arg(text: str, option: str) -> tuple:
    txid, mark, index = text.partition(":")
    if not mark or not is_hex(txid, 32) or not index.isdigit() or not is_uint(int(index), 32):
        raise Stop(EXIT_CHECK, f"{option} wants TXID:INDEX (64 lowercase hex characters, a colon, a number), got {text!r}")
    return txid, int(index)


def read_token_state(path: str) -> tuple:
    """(covenant id, KCC20 state with "contract" first) from a token state file."""
    data, _ = read_json(path)
    problems: list = []
    if not shape(problems, "token state file", data, {"covenant_id": "hex32", "state": "object"}):
        raise Stop(EXIT_CHECK, "Refusing: " + "; ".join(problems))
    state = {"contract": "KCC20", **{k: v for k, v in data["state"].items() if k != "contract"}}
    if data["state"].get("contract", "KCC20") != "KCC20":
        problems.append(f"the state names the contract {data['state'].get('contract')!r}, not 'KCC20'")
    elif shape(problems, "token state", state, KCC20_SHAPE):
        if state["owner_scheme"] != OWNER_KEY:
            problems.append(f"the owner scheme is {state['owner_scheme']}; the token coin must be owned by a key (0)")
        if state["borrow_scheme"] != 0:
            problems.append("borrowing is not disabled (borrow_scheme 0)")
        if state["amount"] != TOTAL:
            problems.append(f"the coin holds {state['amount']} tokens; the vault takes it whole, so it must hold {TOTAL}")
    if problems:
        raise Stop(EXIT_CHECK, "Refusing the token state: " + "; ".join(problems))
    return data["covenant_id"], state


def coin_of_entry(entry: dict) -> dict:
    version, script = entry_script(entry)
    return {
        "transaction_id": entry["outpoint"]["transactionId"],
        "index": entry["outpoint"]["index"],
        "value": entry["utxoEntry"]["amount"],
        "script_public_key_version": version,
        "script_public_key": script,
        "block_daa_score": entry["utxoEntry"].get("blockDaaScore"),
    }


async def cmd_prepare(args) -> int:
    address = args.payer_address
    if not address.startswith(ADDRESS_PREFIX):
        raise Stop(EXIT_CHECK, f"Refusing: the payer's address does not start with {ADDRESS_PREFIX}")
    try:
        payer_script = pay_to_address_script(Address(address))
    except Exception as error:
        raise Stop(EXIT_CHECK, f"The payer's address does not read as an address ({error}).") from None
    plain = payer_script.script
    if payer_script.version != 0 or not (len(plain) == 68 and plain.startswith("20") and plain.endswith("ac")):
        raise Stop(EXIT_CHECK, "Refusing: the payer's address does not pay to an x-only public key (20 KEY ac).")
    funding_key = outpoint_arg(args.funding, "--funding")
    token_key = outpoint_arg(args.token_coin, "--token-coin")
    if args.feerate < MIN_FEERATE:
        raise Stop(EXIT_CHECK, f"Refusing: --feerate must be at least {MIN_FEERATE} sompi per gram.")
    if not 0 < args.lead <= 1_000_000:
        raise Stop(EXIT_CHECK, "Refusing: --lead must be a DAA count above 0 and at most 1000000.")
    fee_cap = round(args.fee_cap_kas * SOMPI_PER_KAS)
    if not 0 < fee_cap <= MAX_TOTAL_FEES:
        raise Stop(EXIT_CHECK, f"Refusing: --fee-cap-kas must be above 0 and at most {kas(MAX_TOTAL_FEES)}.")
    if not args.harness:
        raise Stop(EXIT_CHECK, "Refusing: `prepare` needs --harness (or KCC20_HARNESS_DIR) to compute the token coin's script from its state.")
    covenant_id, state = read_token_state(args.token_state)
    try:
        harness = load_harness(args.harness)
        token_script = kcc20_lock(harness, state)
    except Exception as error:
        raise Stop(EXIT_CHECK, f"Refusing: the harness could not compute the token coin's lock ({type(error).__name__}: {error}).") from None
    token_address = address_of_script(0, token_script)
    out = Path(args.out)
    if out.exists():
        raise Stop(EXIT_CHECK, f"Refusing to overwrite {out}.")
    client, info = await open_node()
    try:
        say(f"via     : {client.url}, node version {info.get('serverVersion')}")
        score = daa_of(info)
        if score is None:
            raise Stop(EXIT_CHECK, "Refusing: the node did not give its virtual DAA score.")
        estimate = await reading(fee_estimate(client))
        say_estimate(estimate)
        say(f"feerate : {args.feerate} sompi per gram; the node's priority estimate is {estimate.get('priorityBucket', {}).get('feerate')}")
        entries = await reading(utxos_at(client, address))
        funding = [e for e in entries if entry_key(e) == funding_key]
        if len(funding) != 1:
            say(f"coins at {address}:")
            for entry in entries:
                bound = " (carries a covenant id)" if entry["utxoEntry"].get("covenantId") else ""
                say(f"  {kas(entry['utxoEntry']['amount'])} KAS  {outpoint_text(entry_key(entry))}{bound}")
            raise Stop(EXIT_CHECK, f"Refusing: the funding coin {outpoint_text(funding_key)} is not among the unspent coins at the payer's address.")
        coin = funding[0]
        if coin["utxoEntry"].get("covenantId"):
            raise Stop(EXIT_CHECK, "Refusing: the funding coin carries a covenant id; it must be a plain coin.")
        if not 0 < coin["utxoEntry"]["amount"] <= MAX_FUNDING:
            raise Stop(EXIT_CHECK, f"Refusing: the funding coin holds {kas(coin['utxoEntry']['amount'])} KAS; the run allows at most {kas(MAX_FUNDING)}.")
        if entry_script(coin) != (payer_script.version, payer_script.script):
            raise Stop(EXIT_CHECK, "Refusing: the funding coin is not locked to the payer's address.")
        tokens = await reading(utxos_at(client, token_address))
        found = [e for e in tokens if entry_key(e) == token_key]
        if len(found) != 1:
            raise Stop(
                EXIT_CHECK,
                f"Refusing: the token coin {outpoint_text(token_key)} is not among the unspent coins at the address of its "
                f"state ({token_address}); {len(tokens)} other coin(s) there.",
            )
        held = found[0]
        if (held["utxoEntry"].get("covenantId") or None) != covenant_id:
            raise Stop(EXIT_CHECK, f"Refusing: the token coin carries the covenant id {held['utxoEntry'].get('covenantId')}, the state file says {covenant_id}.")
        if held["utxoEntry"]["amount"] != COIN_VALUE:
            raise Stop(EXIT_CHECK, f"Refusing: the token coin holds {kas(held['utxoEntry']['amount'])} KAS; the vault must be a coin of exactly {kas(COIN_VALUE)}.")
        if entry_script(held) != (0, token_script):
            raise Stop(EXIT_CHECK, "Refusing: the token coin's script is not the lock of the state given.")
        request = {
            "format": REQUEST_FORMAT,
            "network": NETWORK_ID,
            "read_at_utc": utc_now(),
            "node": client.url,
            "node_version": info.get("serverVersion"),
            "virtual_daa_score": score,
            "coin_value": COIN_VALUE,
            "feerate": args.feerate,
            "fee_cap_total": fee_cap,
            "schedule": {"start": score + args.lead, "cliff": CLIFF, "duration": DURATION, "total": TOTAL},
            "funding": coin_of_entry(coin),
            "token": {"covenant_id": covenant_id, "coin": coin_of_entry(held), "state": state},
        }
        try:
            with open(out, "x", encoding="utf-8", newline="\n") as file:
                file.write(json.dumps(request, indent=2, ensure_ascii=False) + "\n")
        except FileExistsError:
            raise Stop(EXIT_CHECK, f"Refusing to overwrite {out}.") from None
        say(f"DAA     : virtual DAA score {score}; schedule start {score + args.lead} (lead {args.lead})")
        say(f"funding : {kas(coin['utxoEntry']['amount'])} KAS, {outpoint_text(funding_key)}")
        say(f"token   : {outpoint_text(token_key)}, {state['amount']} tokens, covenant id {covenant_id}")
        say(f"written : {out}")
        return EXIT_OK
    finally:
        await quietly_disconnect(client)


async def cmd_check(args) -> int:
    addresses = party_addresses(args.party)
    data, chain_sha256 = read_json(args.chain)
    say(f"file    : {Path(args.chain).name}, SHA-256 {chain_sha256}")
    problems, locks_checked, verified = check_offline(data, addresses, args.harness, args.verify_report)
    if problems:
        say(f"REFUSED: {len(problems)} problem(s). Nothing was sent.")
        return EXIT_CHECK
    without = "" if locks_checked else " EXCEPT check j: the KCC20 locks were NOT recomputed (no --harness), and `send` does not run without it."
    vesting = "" if verified else " EXCEPT check l: the TokenVesting locks were NOT CHECKED (no --verify-report), and `send` does not run without it."
    if not args.online:
        say(f"check: passed (offline).{without}{vesting} The network was not read.")
        return EXIT_OK
    client, info = await open_node()
    try:
        say(f"via     : {client.url}, node version {info.get('serverVersion')}")
        online, _ = await reading(check_online(client, data, info))
    finally:
        await quietly_disconnect(client)
    if online:
        say(f"REFUSED: {len(online)} problem(s) on chain. Nothing was sent.")
        return EXIT_CHECK
    say(f"check: passed (offline and online).{without}{vesting}")
    return EXIT_OK


async def cmd_send(args) -> int:
    """`send` under the lock of its record folder: no two runs share a record folder at one time."""
    if not args.yes_send_to_testnet_10:
        raise Stop(EXIT_CHECK, "Refusing: `send` submits transactions. Say --yes-send-to-testnet-10 to mean it. Nothing was sent.")
    if not args.harness:
        raise Stop(EXIT_CHECK, "Refusing: `send` needs --harness (or KCC20_HARNESS_DIR). Nothing was sent.")
    if not args.verify_report:
        raise Stop(
            EXIT_CHECK,
            "Refusing: `send` needs --verify-report FILE, the verifier's report on the TokenVesting locks: this program "
            "does not recompute them. Nothing was sent.",
        )
    folder = Path(args.record)
    lock = folder / "send.lock"
    try:
        folder.mkdir(parents=True, exist_ok=True)
        with open(lock, "x", encoding="utf-8", newline="\n") as file:
            file.write(json.dumps({"pid": os.getpid(), "since_utc": utc_now()}) + "\n")
    except FileExistsError:
        raise Stop(
            EXIT_CHECK,
            f"Refusing: {folder.name}/send.lock exists: another `send` may be running with this record folder. If none is "
            "(an earlier run was killed), look with `status`, then remove the lock file by hand. Nothing was sent.",
        ) from None
    except OSError as error:
        raise Stop(EXIT_CHECK, f"The record folder {folder.name!r} cannot be used ({error.strerror}). Nothing was sent.") from None
    try:
        return await send_under_lock(args)
    finally:
        global CURRENT_RECORD
        CURRENT_RECORD = None
        try:
            lock.unlink()
        except OSError as error:
            say(f"NOTE    : the lock file {folder.name}/send.lock could not be removed ({error.strerror}); remove it by hand")


async def send_under_lock(args) -> int:
    if not args.yes_send_to_testnet_10:
        raise Stop(EXIT_CHECK, "Refusing: `send` submits transactions. Say --yes-send-to-testnet-10 to mean it. Nothing was sent.")
    if not args.harness:
        raise Stop(
            EXIT_CHECK,
            "Refusing: `send` needs --harness (or KCC20_HARNESS_DIR). Without it nothing shows that the token "
            "outputs are coins of the token's contract. Nothing was sent.",
        )
    addresses = party_addresses(args.party)
    data, chain_sha256 = read_json(args.chain)
    say(f"file    : {Path(args.chain).name}, SHA-256 {chain_sha256}")
    problems, locks_checked, verified = check_offline(data, addresses, args.harness, args.verify_report)
    if problems or not locks_checked or not verified:
        say(f"REFUSED: {len(problems)} problem(s). Nothing was sent.")
        return EXIT_CHECK
    folder = Path(args.record)
    earlier = earlier_events(folder)
    probes = [move for move in data["moves"] if move["kind"] == "probe"]
    # A probe that an earlier run saw accepted, or lost the answer to, may be with a node. No flag overrides this.
    blocking = (
        ("probe accepted", EXIT_FORBIDDEN_ACCEPTED),
        ("probe already with a node", EXIT_FORBIDDEN_ACCEPTED),
        ("probe no answer", EXIT_NO_ANSWER),
        (PROBE_UNKNOWN, EXIT_NO_ANSWER),
    )
    for what, code in blocking:
        names = [probe["name"] for probe in probes if probe["transaction_id"] in earlier[what]]
        if names:
            raise Stop(
                code,
                f"Refusing: the record folder holds {what!r} for {names}: a forbidden move may be on its way or on chain. "
                "Look with `status`. No flag overrides this. Nothing was sent.",
            )
    client, info = await open_node()
    try:
        say(f"via     : {client.url}, node version {info.get('serverVersion')}")
        online, picture = await reading(check_online(client, data, info))
        if online:
            say(f"REFUSED: {len(online)} problem(s) on chain. Nothing was sent.")
            return EXIT_INCONSISTENT
        # A step an earlier run stopped at is not handed over a second time by itself.
        again = {}
        for position, step in enumerate(steps_of(data)):
            txid = step["transaction_id"]
            for what in ("step refused", "step no answer", STEP_UNKNOWN):
                if txid in earlier[what] and not picture.proven[position] and txid not in earlier["step accepted"]:
                    again[txid] = what
        if again and not args.again_after_a_stop:
            stopped = [f"{step['name']!r} ({again[step['transaction_id']]})" for step in steps_of(data) if step["transaction_id"] in again]
            raise Stop(
                EXIT_CHECK,
                f"Refusing: an earlier run with this record folder stopped at {', '.join(stopped)}, and that step is not on chain. "
                "It is not handed over a second time by itself (where the outcome is unknown, the earlier hand-over "
                "may or may not have reached the node). Look with `status`; to hand it over again by your own "
                "decision, say --again-after-a-stop. Nothing was sent.",
            )
        try:
            record = Record(folder, client, str(info.get("serverVersion")))
        except OSError as error:
            raise Stop(EXIT_CHECK, f"The record folder {folder.name!r} cannot be made ({error.strerror}). Nothing was sent.") from None
        started_utc = utc_now()
        note(
            record,
            "run started",
            chain_file=Path(args.chain).name,
            chain_sha256=chain_sha256,
            program_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            harness_sha256=harness_sha256(args.harness),
            verify_report_sha256=file_sha256(args.verify_report),
            vesting_covenant_id=data["vesting_covenant_id"],
            token_covenant_id=data["token"]["covenant_id"],
            schedule=data["schedule"],
            moves=len(data["moves"]),
            steps_on_chain_at_start=picture.prefix,
            daa_score=daa_of(info),
            again_after_a_stop=sorted(again) if again else None,
        )
        if record.broken:
            raise Stop(EXIT_CHECK, "The record cannot be written. Nothing was sent.")
        global CURRENT_RECORD
        CURRENT_RECORD = record
        say(f"record  : {shown_path(record.path)}")
        run = Run(client, record, data, picture, earlier, again)
        if daa_of(info) is not None:
            run.rate.saw(daa_of(info))

        def summary(code: int, last_words: str) -> None:
            write_summary(record, {
                "chain_file": Path(args.chain).name,
                "chain_sha256": chain_sha256,
                "started_utc": started_utc,
                "ended_utc": utc_now(),
                "exit_code": code,
                "last_words": last_words,
                "moves": run.outcomes,
            })

        try:
            before = await balances(client, addresses)
            say_balances("before ", before)
            note(record, "balances before", balances=before)
            for position, move in enumerate(data["moves"]):
                run.current = move
                if move["kind"] == "step":
                    run.outcome(move, await send_step(run, position))
                else:
                    run.outcome(move, await send_probe(run, position))
            after = await balances(client, addresses)
            say_balances("after  ", after)
            note(record, "balances after", balances=after)
            outcomes = [item["outcome"] for item in run.outcomes]
            counts = {name: outcomes.count(name) for name in sorted(set(outcomes))}
            note(record, "run finished", outcomes=counts)
            line = "finished: " + "; ".join(f"{count} {name}" for name, count in counts.items())
            say(line)
            if record.broken:
                summary(EXIT_RECORD, line)
                raise Stop(EXIT_RECORD, "STOP: the run went through, but the record could not be written in full: the lines are above.")
            summary(EXIT_OK, line)
            return EXIT_OK
        except Stop as stop:
            # The last line goes on screen first: whatever happens to the record, the stop is told and keeps its code.
            code = EXIT_NODE if isinstance(stop, WrongNode) else stop.code
            message = f"STOP: {stop.message} Nothing more was submitted." if isinstance(stop, WrongNode) else stop.message
            if not stop.said:
                say(message)
            at_stop = None
            if code in (EXIT_REFUSED, EXIT_FORBIDDEN_ACCEPTED, EXIT_INCONSISTENT):
                try:
                    at_stop = await balances(client, addresses)
                    say_balances("at stop", at_stop)
                except Exception:
                    at_stop = None
            note(record, "run stopped", exit_code=code, last_words=message, balances=at_stop)
            run.stopped_at(f"stopped, exit code {code}")
            summary(code, message)
            final = Stop(code, message)
            final.said = True
            raise final from None
        except asyncio.CancelledError:
            words = f"interrupted while {INTERRUPTED['what']} was being handed over" if INTERRUPTED else "interrupted, nothing was being handed over"
            note(record, "run interrupted", exit_code=130, last_words=words)
            run.stopped_at("interrupted, exit code 130")
            summary(130, words)
            raise
        except Exception as error:
            # Reading failed for good (the retries ran out) before a submission of the move at hand.
            reason = f"{type(error).__name__}: {error}"
            message = f"STOP: the node could not be read ({reason}). Nothing more was submitted. Run `status`."
            say(message)
            note(record, "run stopped", exit_code=EXIT_NODE, last_words=message)
            run.stopped_at(f"stopped, exit code {EXIT_NODE}")
            summary(EXIT_NODE, message)
            final = Stop(EXIT_NODE, message)
            final.said = True
            raise final from None
    finally:
        await quietly_disconnect(client)


async def cmd_status(args) -> int:
    addresses = party_addresses(args.party)
    data, _ = read_json(args.chain)
    problems = check_shape(data)
    if not problems:
        problems = check_parties(data, addresses) + check_rebuild(data) + check_linkage(data)
    if problems:
        for problem in problems:
            say(f"REFUSED  {problem}")
        say("status: the file does not pass the checks `status` needs; run `check`.")
        return EXIT_CHECK
    client, info = await open_node()
    try:
        say(f"via     : {client.url}, node version {info.get('serverVersion')}")
        score = daa_of(info)
        for line in daa_lines(data, score):
            say(line)
        picture = await reading(read_picture(client, data))
        lines, wrong = picture_lines(data, picture)
        for position, move in enumerate(data["moves"]):
            coins = output_coins(move)
            due = "due now" if score is not None and score > move["send_after_daa"] else (
                f"due in {move['send_after_daa'] - score + 1} DAA" if score is not None else "")
            if move["kind"] == "step":
                visible = [coin for coin in coins if coin["key"] in picture.visible]
                proven = "on chain" if picture.proven[steps_before(data, position)] else "no trace"
                say(f"step  {move['name']!r}: {proven}; {len(visible)} of {len(coins)} outputs unspent; {due}")
            else:
                made = [coin for coin in coins if coin in picture.probe_outputs]
                say(f"probe {move['name']!r}: {'OUTPUTS VISIBLE - a forbidden move is on chain' if made else 'no trace'}; {due}")
        say("coins of the chain that are unspent now:")
        known = {**starting_coins(data), **{c["key"]: c for s in steps_of(data) for c in output_coins(s)}}
        for key, entry in picture.visible.items():
            coin = known.get(key)
            if coin is None:
                continue
            state = coin["state"] or {}
            what = coin["role"] + (f" to {coin['to']!r}" if coin["to"] else "") + (f" {state.get('contract')}" if state else "")
            if state.get("contract") == "KCC20":
                what += f" {state.get('amount')} tokens of {party_of_key(data, state.get('owner'))}"
            elif state.get("contract") == "TokenVesting":
                what += f" released {state.get('released')}"
            say(f"  {kas(entry['utxoEntry']['amount'])} KAS  {outpoint_text(key)}  {what}")
        for line in lines:
            say(line)
        for problem in wrong:
            say(f"NOTE     {problem}")
        say_balances("balance", await reading(balances(client, addresses)))
        return EXIT_OK if not wrong else EXIT_INCONSISTENT
    finally:
        await quietly_disconnect(client)


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise Stop(EXIT_CHECK, f"{self.format_usage().strip()}\n{self.prog}: {message}")


def parser() -> argparse.ArgumentParser:
    top = Parser(prog="vesting_send.py", description="Check a prepared chain of vesting transactions and send it to Kaspa testnet-10.")
    commands = top.add_subparsers(dest="command", required=True, parser_class=Parser)
    harness = os.environ.get("KCC20_HARNESS_DIR")

    prepare = commands.add_parser("prepare", help="read the coins, the DAA score and the fee estimate, write the request (read-only)")
    prepare.add_argument("--payer-address", required=True)
    prepare.add_argument("--funding", required=True, metavar="TXID:INDEX")
    prepare.add_argument("--token-coin", required=True, metavar="TXID:INDEX")
    prepare.add_argument("--token-state", required=True, metavar="FILE")
    prepare.add_argument("--harness", default=harness)
    prepare.add_argument("--feerate", type=int, default=DEFAULT_FEERATE)
    prepare.add_argument("--lead", type=int, default=DEFAULT_LEAD)
    prepare.add_argument("--fee-cap-kas", type=float, default=MAX_TOTAL_FEES / SOMPI_PER_KAS)
    prepare.add_argument("--out", required=True)

    def with_chain(command):
        command.add_argument("chain")
        command.add_argument("--party", action="append", required=True, metavar="NAME=ADDRESS")

    check = commands.add_parser("check", help="verify the chain file; with --online also read the network")
    with_chain(check)
    check.add_argument("--harness", default=harness)
    check.add_argument("--online", action="store_true")
    check.add_argument("--verify-report", metavar="FILE")

    send = commands.add_parser("send", help="check, then submit the moves in their order, each at its DAA score")
    with_chain(send)
    send.add_argument("--harness", default=harness)
    send.add_argument("--record", required=True, metavar="DIR")
    send.add_argument("--yes-send-to-testnet-10", action="store_true")
    send.add_argument("--again-after-a-stop", action="store_true")
    send.add_argument("--verify-report", metavar="FILE")

    status = commands.add_parser("status", help="what of the chain is on chain now (read-only)")
    with_chain(status)
    return top


COMMANDS = {"prepare": cmd_prepare, "check": cmd_check, "send": cmd_send, "status": cmd_status}


def main(argv: list | None = None) -> int:
    INTERRUPTED.clear()
    for stream in (sys.stdout, sys.stderr):
        # A console that cannot show a character of the file's texts must not end a run.
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="backslashreplace")
    try:
        args = parser().parse_args(argv)
        return asyncio.run(COMMANDS[args.command](args))
    except Stop as stop:
        if not stop.said:
            say(stop.message)
        return stop.code
    except (KeyboardInterrupt, asyncio.CancelledError):
        if INTERRUPTED:
            say(
                f"Interrupted while {INTERRUPTED['what']}, transaction {INTERRUPTED['transaction_id']}, was being handed "
                "over: it may or may not have reached the node. Nothing more was submitted. Run `status`."
            )
        else:
            say("Interrupted. Nothing was being handed over at that moment. Run `status` before anything else.")
        return 130


if __name__ == "__main__":
    sys.exit(main())
