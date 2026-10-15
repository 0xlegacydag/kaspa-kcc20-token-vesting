# File formats of the vesting run on testnet-10

Two JSON files pass between the two programs of the run. Both are UTF-8 with LF line
endings. All amounts are sompi (1 KAS = 100,000,000 sompi). All byte strings are lowercase
hex. Neither file ever holds a secret key. KAS on this page always means test KAS of
testnet-10. Time on chain is the DAA score: a lock time below 500,000,000,000 is a DAA
score, and a node takes a transaction only once its virtual DAA score is strictly above
the transaction's lock time (unless every input is final, which no input of this run is).

- The **request** is written by the sender's `prepare` command after it has read the
  network, and read by the builder.
- The **chain** is written by the builder, offline, and read by the sender.

The builder is a Rust test file of this repository, `tests/testnet_chain.rs` (started
with `cargo test --test testnet_chain`, see [README.md](README.md)). It builds and signs every transaction, runs each one
against the node rules of `node-gate` at the DAA score it is meant to be sent at, runs the
launch checks of `src/launch.rs` on the real coins, and writes the chain file. The sender
is `vesting_send.py` (Python, Kaspa SDK 2.1.0). It holds no keys: every signature is
already in the chain file. It rebuilds every transaction from its fields, checks the file
by its own rules, waits for the DAA score each move needs and only then submits.

## The run

One beneficiary, one schedule, one vault of 10 tokens of an existing KCC-20 token family
(the reference token launched on testnet-10 on 2026-10-11). Three parties:

- `payer`: owns the funding coin, pays every fee and the KAS of every new coin, gets the
  change;
- `alice`: owns the token coin that becomes the vault; signs once;
- `beneficiary`: signs every release and receives the tokens.

The moves, in the order they are sent (`S` is the schedule's start, all numbers DAA):

| name | kind | lock time | what |
|---|---|---|---|
| `A1 vesting genesis` | step | 0 | the payer's coin creates the vesting coin with a new covenant id |
| `A2 fund the vault` | step | 0 | Alice moves her 10-token coin whole to the vesting covenant (owner scheme 4) |
| `B1 before the cliff` | probe | S + 1000 | a release of 1 token dated before the cliff: the contract refuses |
| `B2 dated at the end` | probe | S + 10000 | all 10 tokens, dated at the end of the schedule, sent long before it: the node refuses, the lock time has not passed |
| `B3 more than vested` | probe | S + 3000 | 4 tokens at the cliff, where 3 have vested: the contract refuses |
| `B4 to another key` | probe | S + 3000 | 3 tokens at the cliff, signed by the beneficiary, but the claim is owned by the payer's key: the contract refuses |
| `R1 first release` | step | S + 3000 | 3 tokens at the cliff |
| `R2 second release` | step | S + 6500 | 3 more (6 vested) |
| `R3 the rest` | step | S + 10000 | the 4 left, at the end; the vesting coin ends |

Schedule: `cliff` 3000, `duration` 10000, `total` 10. B1, B2, B3 and B4 spend the vesting
coin of A1 and the vault of A2, the coins R1 spends as well.

## Request: `vesting-launch-request/1`

```
{
  "format": "vesting-launch-request/1",
  "network": "testnet-10",
  "read_at_utc": "2026-01-01T12:00:00Z",
  "node": "wss://...",
  "node_version": "2.1.0",
  "virtual_daa_score": 597148140,
  "coin_value": 100000000,
  "feerate": 200,
  "fee_cap_total": 800000000,
  "schedule": { "start": 597154140, "cliff": 3000, "duration": 10000, "total": 10 },
  "funding": {
    "transaction_id": "<64 hex>", "index": 2, "value": 1661402000,
    "script_public_key_version": 0, "script_public_key": "<hex>",
    "block_daa_score": 593874770
  },
  "token": {
    "covenant_id": "<64 hex>",
    "coin": {
      "transaction_id": "<64 hex>", "index": 1, "value": 100000000,
      "script_public_key_version": 0, "script_public_key": "<hex>",
      "block_daa_score": 593874700
    },
    "state": { "contract": "KCC20", "amount": 10, "owner": "<64 hex>", "owner_scheme": 0,
               "borrow_scheme": 0, "borrow_guard": "<64 hex>",
               "extension_commitment": "<64 hex>" }
  }
}
```

- `virtual_daa_score` is the node's virtual DAA score when `prepare` read it.
- `schedule.start` is `virtual_daa_score` plus the lead `prepare` was given (6000 unless
  told otherwise): the time to build, check and send A1 and A2 before B1 is due.
- `coin_value` is the value of every new covenant coin of the run: exactly 1 KAS.
- `feerate` is sompi per gram, a whole number, at least 100.
- `fee_cap_total` bounds the sum of the fees of every move, steps and probes.
- `funding` is the one plain coin the run starts from, as the node reported it, locked to
  the payer's key.
- `token.coin` is the token coin as the node reported it (it carries `token.covenant_id`);
  `token.state` is the state it is said to hold, taken from the record of its creation.
  The builder recomputes the coin's script from that state and refuses a mismatch; the
  owner of the state must be Alice's key, scheme 0, borrowing disabled (`borrow_scheme` 0).

Keys are not in the request. The builder reads them from three key files kept outside
every repository, named by the environment variables `VESTING_PAYER_KEY_FILE`,
`VESTING_ALICE_KEY_FILE` and `VESTING_BENEFICIARY_KEY_FILE`.

## Chain: `vesting-launch-chain/1`

```
{
  "format": "vesting-launch-chain/1",
  "network": "testnet-10",
  "built_at_utc": "2026-01-01T12:05:00Z",
  "nothing_was_sent": true,
  "coin_value": 100000000,
  "feerate": 200,
  "fee_rule": "<one sentence>",
  "fee_cap_total": 800000000,
  "schedule": { "start": 0, "cliff": 3000, "duration": 10000, "total": 10 },
  "funding": { ...as in the request..., "owner": "payer" },
  "token": { "covenant_id": "<64 hex>", "template_hash": "<64 hex>",
             "coin": { ...as in the request... }, "state": { ...as in the request... } },
  "vesting_covenant_id": "<64 hex>",
  "parties": {
    "payer":       { "x_only_public_key": "<64 hex>", "script_public_key": "<hex>" },
    "alice":       { ... },
    "beneficiary": { ... }
  },
  "launch_checks": { "check_vesting_genesis": "ok", "check_parameters": "ok", "check_vault": "ok" },
  "moves": [ <transaction>, ... ],
  "totals": {
    "fees_of_steps": 0,
    "fees_of_probes": 0,
    "tokens_to_beneficiary": 10,
    "kas_locked_at_the_end": 0,
    "change_at_the_end": 0
  }
}
```

- `parties`: a party's `script_public_key` is the pay-to-public-key script of its key,
  `20 <32-byte x-only key> ac`, version 0. The sender is told the parties' addresses on its
  command line and compares. No two parties share a key.
- `token.template_hash` is the template hash of the pinned KCC20 program that the vesting
  state names (`token_template`).
- `vesting_covenant_id` is the covenant id A1 creates; the builder computes it from A1 by
  the consensus rule, and the sender derives it again from A1.
- `launch_checks` are the results of `check_vesting_genesis` (on A1), `check_parameters`
  (on the state A1 creates) and `check_vault` (on the output of A2 that becomes the vault),
  each `"ok"`. The builder writes no chain file when one of them fails.
- `moves` are sent in the order of the list. A step is expected to be accepted; it spends
  the funding coin, the token coin or outputs of earlier steps. A probe is expected to be
  refused; it spends only coins that are unspent at its place in the list (outputs of
  earlier steps not spent by an earlier step). Two probes may spend the same coin.
- Names are unique. No JSON object repeats a key.
- `totals.fees_of_steps` and `totals.fees_of_probes` are checked; the rest is for a reader.

### Transaction

```
{
  "name": "R1 first release",
  "kind": "step",
  "what": "<one sentence for a reader>",
  "expect": "accepted",
  "transaction_id": "<64 hex>",
  "version": 1,
  "lock_time": 597157140,
  "send_after_daa": 597157160,
  "send_before_daa": null,
  "judged_at_daa": 597157160,
  "subnetwork_id": "<40 hex>",
  "gas": 0,
  "payload": "",
  "mass": 117651,
  "inputs": [
    {
      "transaction_id": "<64 hex>",
      "index": 0,
      "sequence": 0,
      "sig_op_count": 0,
      "compute_budget": 22,
      "signature_script": "<hex>",
      "spends": {
        "from": { "move": "A1 vesting genesis", "output": 0 },
        "value": 100000000,
        "script_public_key_version": 0,
        "script_public_key": "<hex>",
        "covenant_id": "<64 hex>",
        "what": "<a few words>"
      }
    }
  ],
  "outputs": [
    {
      "value": 100000000,
      "script_public_key_version": 0,
      "script_public_key": "<hex>",
      "covenant_binding": { "authorizing_input": 1, "covenant_id": "<64 hex>" },
      "role": "token",
      "to": null,
      "state": { "contract": "KCC20", "amount": 3, "owner": "<64 hex>", "owner_scheme": 0,
                 "borrow_scheme": 0, "borrow_guard": "<64 hex>",
                 "extension_commitment": "<64 hex>" },
      "what": "<a few words>"
    }
  ],
  "fee": 0,
  "fee_floor": 0,
  "masses": { "compute_mass": 0, "transient_mass": 0, "calculated_storage_mass": 0, "fee_mass": 0 },
  "gate": { "scripts": true, "consensus": true, "mempool": true, "block": true, "refusal": null },
  "refusal_expected": null,
  "twin": null
}
```

- `kind` is `"step"` or `"probe"`; `expect` is `"accepted"` for a step and `"refused"` for
  a probe.
- Fixed values in every transaction: `version` 1, `gas` 0, an empty `payload`, the
  all-zero `subnetwork_id`, and on every input `sequence` 0 and `sig_op_count` 0 (no input
  is final, so the lock time binds). A version-1 input commits to a compute budget instead
  of a count of signature operations.
- `lock_time` is 0 for A1 and A2 and a DAA score at or after `schedule.start` for every
  other move.
- `send_after_daa`: the sender submits the move only when the node's virtual DAA score is
  above it. It is 0 when `lock_time` is 0, otherwise `lock_time + 20`, except for B2,
  whose refusal is its own lock time not having passed: its `send_after_daa` is the
  `send_after_daa` of B1, so that it is sent long before its lock time.
- `send_before_daa`: null, except for a probe whose refusal is the lock time itself (B2):
  the sender never submits it when the virtual DAA score is at or above this value. It is
  `lock_time - 2000`. Past it the probe is not sent, and the record says so.
- `judged_at_daa` is the DAA score `node-gate` judged the transaction at: `send_after_daa`
  for a move with a lock time, for B2 the `send_after_daa` of B1 (a score B2 can be sent
  at), for A1 and A2 the request's `schedule.start` minus the lead (any score works for
  them).
- `mass` is the storage mass a node calculates for the transaction; it is the value of the
  transaction's mass field.
- `spends.from` is `"funding"`, `"token_coin"` or `{ "move": "<name of an earlier step>",
  "output": <index> }`. `spends.covenant_id` is the covenant id the spent coin carries, or
  null.
- `role` of an output:
  - `"vesting"`: a coin of the vesting contract, bound to `vesting_covenant_id`;
  - `"token"`: a KCC20 coin of the token family, bound to `token.covenant_id`;
  - `"change"`: plain, to the payer.
  `to` names the party for `"change"` and is null otherwise.
- `state` describes a covenant coin so that a second tool can rebuild its lock. Every
  covenant output has one, in steps and in probes; for a probe it is the state the
  forbidden move claims:
  - `{ "contract": "KCC20", "amount": 0, "owner": "<64 hex>", "owner_scheme": 0 or 4,
    "borrow_scheme": 0, "borrow_guard": "<64 hex>", "extension_commitment": "<64 hex>" }`:
    with `owner_scheme` 0 the owner is the x-only key of a party; with 4 it is
    `vesting_covenant_id`;
  - `{ "contract": "TokenVesting", "beneficiary": "<64 hex>", "token_covid": "<64 hex>",
    "token_template": "<64 hex>", "start": 0, "cliff": 0, "duration": 0, "total": 0,
    "released": 0 }`.
- `fee` is the sum of the spent values minus the sum of the output values. `fee_floor` is
  the least fee a default node relays the transaction for.
- `gate` is what `node-gate` answered at `judged_at_daa`. For a step all four are true and
  `refusal` is null.
- `refusal_expected`, for a probe:
  `{ "kind": "scripts" | "finality", "input": 0, "contract_line": "<the source line of the
  contract that refuses>" | null, "node_message_contains": "<text>" }`. For `"scripts"` the
  text is `verification failed` (a node says "script ran, but verification failed"), except
  where the refusing line is a lock-time check (B1, `require(tx.daa >= start + cliff)`):
  then it is `Unsatisfied lock time`; for `"finality"` it is `is not finalized` (a node says
  "transaction input #0 is not finalized"). The sender classifies a refusal only by the
  probe's own text. For a probe refused by the scripts, the scripts of its other inputs
  pass, so that a node, which checks the inputs in no fixed order, can report no other
  refusal. `gate.refusal` is then `{ "layer": "scripts" | "consensus", "input": 0,
  "rule": null, "message": "<text>" }` and agrees with it.
- `twin`, for a probe: `{ "accepted": true, "judged_at_daa": <n>, "what": "<the one thing
  put right>" }`: the builder built the same move with the one wrong thing put right (B1:
  lock time at the cliff; B2: judged after its lock time; B3: 3 tokens; B4: the claim owned
  by the beneficiary) and `node-gate` accepted it. A probe pays the fee of its twin.

### Limits the sender enforces

The sender refuses a chain outside these bounds, whatever the file says: the funding coin
holds at most 20 KAS; `coin_value` is exactly 1 KAS; the fees of all moves are at most
8 KAS; at most 10 steps and 10 probes; a mass field between 1 and 500,000; a compute
budget of at most 1,000 per input; no empty signature script; every lock time either 0 or
between `schedule.start` and 500,000,000,000; `schedule.total` 10 and the vault exactly
`schedule.total` tokens.

### What a transaction id does and does not cover

The id covers the version, the outpoints, the sequence numbers, the outputs with their
bindings, the lock time, the subnetwork, the gas and the payload. It does not cover the
signature scripts, the mass field or the compute budgets. Equal ids therefore prove
nothing about those three; they travel in the file as they are and a node judges them.
