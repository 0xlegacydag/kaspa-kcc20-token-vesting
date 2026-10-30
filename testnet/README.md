# The testnet-10 run: builder, verifier, sender

Three programs take the vesting contract of this repository to Kaspa **testnet-10**. Test
network only. KAS on this page always means test KAS of testnet-10. The record of the one
run made with them, on 2026-10-15, as they were then, with links to its transactions:
[results/2026-10-15-testnet-10](results/2026-10-15-testnet-10/README.md).

- The **builder** (Rust, [`../tests/testnet_chain.rs`](../tests/testnet_chain.rs)) works
  offline. It holds the keys, builds and signs the nine moves of the run (two steps that
  set the vesting coin and the vault up, three releases, four forbidden moves), judges each
  one with the node's script engine and the node rules of `node-gate` at the DAA score it is
  meant to be sent at, runs the launch checks of `src/launch.rs` on the real coins, and
  writes the chain file. It then reads the file back and judges every transaction again
  from the file's fields alone. The same judging runs on a chain file that exists, with no
  key and no request (`VESTING_JUDGE_CHAIN`).
- The **verifier** (Rust, [`../tests/testnet_verify.rs`](../tests/testnet_verify.rs)) reads
  the chain file alone. It computes the lock of every vesting and token output again from
  the state the file gives for it (a TokenVesting lock by compiling the contract for that
  state, a KCC20 lock with the pinned program of `kcc20-family`), derives the vesting
  covenant id again from the first move, and writes a report for the sender that names the
  SHA-256 of the file it read. It judges locks, not transactions: it runs no script and
  does not read a signature or a compute budget.
- The **sender** ([`vesting_send.py`](vesting_send.py), Python 3.12 with the Kaspa SDK,
  package `kaspa` 2.1.0, pinned with its hashes in [`../requirements.txt`](../requirements.txt))
  holds no keys and has no option that takes one: every signature is already in the chain
  file. It refuses any network but testnet-10, a node that is not synced, and a node whose
  version is not 2.1.0. It reads the network, checks the chain file by its own rules, waits
  for the DAA score each move needs, submits, and records what the node answers.

The two files that pass between the builder and the sender are described in
[FORMAT.md](FORMAT.md). The builder and the verifier are test files of the crate with
`harness = false` and `test = false` (`Cargo.toml`): `cargo test` leaves them out, and
`tools/run-checks.mjs` does not count them among the checks of the contract.

## Order of a run

From the root of the repository, with `KCC20_REFERENCE_DIR` set as in the
[README](../README.md#check-it-yourself) and `VESTING_DUMP_DIR` not set (the builder does
not start while it is: its transactions are not part of the replay in `engine-2.1.0/`):

```
cd testnet
python vesting_send.py prepare --payer-address kaspatest:... --funding TXID:INDEX \
    --token-coin TXID:INDEX --token-state token-state.json --harness DIR --out request.json
cd ..
cargo test --frozen --test testnet_chain --no-run
VESTING_LAUNCH_INPUT=testnet/request.json VESTING_LAUNCH_OUTPUT=testnet/chain.json \
VESTING_PAYER_KEY_FILE=... VESTING_ALICE_KEY_FILE=... VESTING_BENEFICIARY_KEY_FILE=... \
    cargo test --frozen --test testnet_chain
VESTING_VERIFY_CHAIN=testnet/chain.json VESTING_VERIFY_REPORT=testnet/verify.json \
    cargo test --frozen --test testnet_verify
VESTING_JUDGE_CHAIN=testnet/chain.json cargo test --frozen --test testnet_chain
cd testnet
python vesting_send.py check  chain.json --party payer=kaspatest:... --party alice=... --party beneficiary=... --harness DIR --verify-report verify.json
python vesting_send.py check  chain.json --party ... --harness DIR --verify-report verify.json --online
python vesting_send.py send   chain.json --party ... --harness DIR --verify-report verify.json --record DIR --yes-send-to-testnet-10
python vesting_send.py status chain.json --party ...
```

1. `prepare` (read-only) reads the node's virtual DAA score and fee estimate, the funding
   coin named by `--funding` (plain, at most 20 KAS, at the payer's address, which must pay
   to an x-only key) and the token
   coin named by `--token-coin` (looked up at the address of the lock the harness computes
   from `--token-state`; it must carry the state file's covenant id and hold exactly 1 KAS).
   The schedule starts at the DAA score plus `--lead` (default 6000). `--feerate` defaults
   to 200 (at least 100), `--fee-cap-kas` to 8 (at most 8). It never overwrites `--out`.
2. The builder is built first, with no key file named in the environment: a first
   `cargo test` compiles every dependency and runs their build scripts, and the paths of
   the key files should not be there for them to read. The next `cargo test` only runs it.
   It makes the chain file from the request. Each key file holds one secret key as 64 hex
   characters; the builder refuses a key file inside the repository or at a path the run
   writes, and a chain file that exists already. Nothing is sent.
3. The verifier checks the chain file and, when every check passed, writes its report.
4. The builder judges the file again from its fields alone, with no key: every step must
   be accepted and every probe refused as the file says. A file changed after it was built
   (a compute budget, a signature) stops here. The verifier does not look at those, and
   `check` only at their bounds and whether the file agrees with itself. `send` cannot see
   whether this was done.
5. `check` verifies the chain file offline; `--online` also reads the network.
6. `send` runs the whole check with the network first, then sends the moves in the order
   of the file, each at its time (below). Nothing without `--yes-send-to-testnet-10`, and
   it does not start without `--harness` and `--verify-report`.
7. `status` (read-only) shows which steps are on chain, the unspent coins of the chain,
   the balances of the three parties and the DAA score against the schedule.

`--harness DIR` (or the environment variable `KCC20_HARNESS_DIR`) names the folder of
`kcc20_harness.py` (not in this repository), a second tool that compiles the reference's
contracts with the SDK, independently of the builder: with it the lock of every KCC20 coin
is recomputed from its state and the template hash of KCC20 is compared. **The locks of the
TokenVesting coins are not recomputed by the sender.** The verifier recomputes them and
writes a report (`vesting-verify-report/2`, [FORMAT.md](FORMAT.md)); `--verify-report FILE`
hands it to `check` and `send`. Without it `check` prints the TokenVesting locks as NOT
CHECKED, and `send` does not start.

## What `check` checks

A file that lacks a field, has one of the wrong kind, or names a key twice is refused on
that alone. Otherwise every refusal of every check is printed.

| | |
|---|---|
| a | format, network, `nothing_was_sent`, coin value 1 KAS, funding at most 20 KAS, feerate at least 100, schedule (cliff 3000, duration 10000, total 10), the three `launch_checks` "ok", at most 10 steps and 10 probes, the builder's gates (steps passed everywhere; probes refused, with a refusal and a twin that was accepted) |
| b | the nine moves of FORMAT.md in their order; lock times from `schedule.start` (B1 +1000, B2 +10000, B3, B4, R1 +3000, R2 +6500, R3 +10000, A1 and A2 0); `send_after_daa` (0, or lock time + 20; for B2 the `send_after_daa` of B1), `send_before_daa` (only B2: lock time - 2000), `judged_at_daa`; `refusal_expected` (kind scripts for B1, B3, B4, finality for B2; its `node_message_contains` one of the words FORMAT.md names for that kind, which `check` prints) agrees with `gate.refusal` by layer and input, and the gate's message contains that text; the twin was accepted (B2's judged after its lock time) |
| c | the parties are payer, alice and beneficiary, those named on the command line by their addresses, no two with one key; the funding coin is the payer's |
| d | every transaction, rebuilt by the SDK from its fields with its lock time, has the id the file states, and the same mass field, signature scripts, compute budgets and sequences; version 1, no gas, no payload, the plain subnetwork, sequence 0 and no signature operation count; mass field, compute budgets and lock times within bounds; no empty signature script |
| e | steps spend the funding coin, the token coin or outputs of earlier steps, each once; a probe spends only coins unspent at its place in the list; B1 to B4 and R1 spend the vesting coin of A1 and the vault of A2 |
| f | every fee is what the coins say, at least its floor; the sums equal `totals`, and all fees together are within the file's cap and 8 KAS |
| g | every output is a vesting coin (bound to the vesting covenant id), a token coin (bound to the token's covenant id), each of exactly 1 KAS under a pay-to-script-hash lock, or change to the payer; every binding's authorizing input carries the same covenant id, except the vesting genesis in A1 |
| h | the vesting covenant id is the one the SDK derives from A1 |
| i | the outputs of every move by role, owner and order (A1: vesting, change; A2: vault of 10, change; R1, R2 and the probes B1, B3, B4: the claim, the rest of the vault, the vesting coin with what is then released, change; R3 and B2: the claim, change; B4's claim owned by the payer's key); no token coin of 0 tokens; at the end the only covenant coins left unspent are the beneficiary's three token coins of 3, 3 and 4. The states: KCC20 owners are a party's key (scheme 0) or the vesting covenant (scheme 4), borrowing disabled; every TokenVesting state names the beneficiary, the token's covenant id, the template hash and the schedule; released 0 at A1, 3 at R1, 6 at R2, the vesting coin ends at R3; the token coin is Alice's 10 tokens and the vault holds exactly 10; steps keep the number of tokens; each probe claims what FORMAT.md says (B1 1, B2 10, B3 4 for the beneficiary, B4 3 for the payer's key); the beneficiary ends with 10 |
| j | with `--harness`: every KCC20 lock (the token coin's included) recomputed from its state, and the template hash |
| l | with `--verify-report`: the report's format and the SHA-256 of this chain file (a report of the earlier format `vesting-verify-report/1` names no file: `check` prints NOT BOUND, and `send` refuses it), no mismatches, the chain's vesting covenant id, every vesting output of the chain listed with its move, output number and identical script and "ok", and every output the report lists (vesting or token) the same in the chain |
| k | a summary, and notes: totals for a reader that differ, and moves that can never be sent |

## Time in `send`

Before each move `send` waits until the node's virtual DAA score is above the move's
`send_after_daa`. It reads the score every 5 seconds and says how far it is about every
30 seconds. It gives up (exit 9) when the wait takes 10 minutes longer than the DAA rate
measured during the run predicts (10 per second until a rate has been measured). Right
before every hand-over the node is asked again what it is and where its score stands.

B2 goes from B1's time on (its `send_after_daa` is B1's), long before its lock time, so
the node refuses it as not finalized. A move with `send_before_daa` (only B2) is never
handed over when the score is at or above that value (its lock time - 2000): the record
says "not sent, window passed", and the run goes on.

A node's answer counts as a refusal only when it contains "Rejected transaction" followed
by the move's own id; any other failure is a lost answer. A probe is handed over only when
every coin it spends is unspent on chain; otherwise the run stops (exit 6). A probe
refused with a message whose words after that prefix contain its own
`refusal_expected.node_message_contains` ("Unsatisfied lock time" for a contract line that
checks the lock time), and that name no other input than its `refusal_expected.input`, is
recorded as refused as expected. A node names an input only in some refusals ("transaction
input #0 is not finalized"), not in a refusal by a script: there the words are all it gives.
Refused with another message, a probe is recorded as refused for another reason, word for
word; the run goes on, and ends with exit 10.

A refusal that says "already" is not a refusal of the move. When the node names another
transaction that spends the probe's coin ("already spent by transaction ..."), the number is
compared with the file's: with a transaction the file does not know, or a step of the file,
this node did not take the probe and names that transaction as the holder of the coin.
Nothing is known about other nodes. The record says so and the run stops (exit 6). This
probe's own number, another probe of the file, or "already" with no transaction named means
a forbidden move may be with a node (exit 5). A step
must be accepted under the file's id, and each of its outputs must then show up among the
unspent coins at its address with the expected value, script and covenant id within 120
looks a second apart.

## When `send` stops

Nothing is retried and nothing is rebuilt within a run. The record (`DIR/run-<UTC
time>.jsonl`, one JSON object per event: each hand-over with the DAA score it was made
at, each node answer word for word, the time in UTC) and the screen tell the same story;
at the end `DIR/run-<UTC time>-summary.json` gives the outcome of every move the run
reached (the one it stopped at included) and the exit code. Every line names the node the
client is talking to at that moment; a reconnect is an event of its own, with what the new
node says about itself. The record names the chain file by its name and SHA-256, its moves
by a SHA-256 of all their fields (`moves_sha256`), the harness by the SHA-256 of
`kcc20_harness.py`, and the verifier's report by its SHA-256.

While a `send` runs, `DIR/send.lock` exists; a second `send` with the same folder refuses
to start. The lock goes at the end of the run, whatever the end. If a run was killed and
the lock stayed, look with `status`, then remove it by hand.

**Never change the record folder in the middle of a chain.** What a later run does after a
stop is decided by the records in that folder only; a new folder knows nothing of the
earlier hand-overs.

| Exit code | Meaning |
|---|---|
| 0 | every step on chain, every probe seen refused as expected, by this run or an earlier one with this record |
| 1 | wrong usage, a check failed, or an earlier stop stands in the way; nothing was sent |
| 2 | the node refused a step; its answer is recorded word for word |
| 3 | a submission got no answer, and its outputs did not show up; for a probe, a later `send` with that record ends with this code too, and sends nothing |
| 4 | a step was accepted, but not all of its outputs showed up |
| 5 | `FORBIDDEN MOVE ACCEPTED`: a node took a probe; or `FORBIDDEN MOVE MAY BE WITH A NODE`: its answer says "already" about this probe or another probe of the file, or names no other transaction; a later `send` with that record ends with this code too, and sends nothing |
| 6 | the coins on chain are not what the file expects (a probe's coin already spent, on chain or by another transaction in a node's mempool; a missing input; another id returned) |
| 7 | the node could not be read, or was no longer a synced testnet-10 node of version 2.1.0, before a submission |
| 8 | the record could not be written; the lines are on screen and nothing more was submitted |
| 9 | the DAA score did not reach a move's time within the allowed wait |
| 10 | the steps went through, but a probe was refused for another reason or not sent, and no run with this record saw it refused as expected; the last line names each |
| 130 | interrupted from the keyboard; if a hand-over was under way, the record and the last line name the transaction |

What a later `send` with the same `--record` folder does:

- A record folder with a run of another chain stops it before the network is asked. What a run
  takes over from a record goes by transaction ids, and an id does not cover the signature
  scripts, the mass field or the compute budgets: so every earlier run must have had these
  very moves (`moves_sha256`; a record older than that field, this very file). A new build
  of one request differs only in `built_at_utc` and goes on. No flag overrides this.
- A step that is on chain (one of its outputs visible, or a later step that spends them on
  chain) is not sent again.
- A step accepted while its outputs were not seen is never handed over a second time: the
  program waits for its outputs again.
- A step refused, or with a lost answer, or whose hand-over has no outcome in the record,
  is handed over again only with `--again-after-a-stop`.
- A probe refused in an earlier run is not sent again, and neither is a probe whose coins a
  later step on chain has spent. A probe refused for another reason therefore stays so:
  every later run with that record ends with exit 10.
- A probe whose coin another transaction holds (exit 6 above) blocks nothing: the next run
  starts by the check with the network, and hands the probe over again if its coins are
  there.
- After a probe accepted, said to be with a node already, with a lost answer, or with no
  recorded outcome, no run with that record starts again. No flag overrides this.

## What the sender does not show

- It does not run scripts and does not verify signatures; a node judges them (and, before
  that, the builder judged them offline). An id does not cover the signature scripts, the
  mass field or the compute budgets.
- The TokenVesting locks are not recomputed by the sender; the verifier recomputes them.
- "Seen" means that one node lists the output among the unspent coins of its address.
- A node is asked which network it is on, its version and whether it is synced, and is
  believed. The DAA score is the one the node reports.
- The fee estimate is printed, not enforced.
- It does not know whether the file was judged after it was built (step 4 of the order).
  Compute budgets and signatures are for the builder's judging and for a node to say.

The sender here is not the program that sent the run of 2026-10-15: that one is kept byte
for byte next to the record, as `results/2026-10-15-testnet-10/vesting_send.py`.

## Tests

The sender, offline, against a fake node; in `testnet/`:

```
python -m unittest discover -s tests -v
```

`tests/fixture-chain.json` is made by `tests/make_fixture.py` (its head says what is real
and what is made up): true transaction ids, true KCC20 locks, placeholder signature
scripts, made-up TokenVesting locks. The tests of `send` use a stand-in for the harness
that knows the KCC20 locks of the fixture. Two tests recompute the locks for real: they
need `KCC20_HARNESS_DIR` and are skipped without it. Two more run the sender over the
builder's self-test chains (real signatures, made-up keys) and are skipped when those files
are not there: run the builder's self-test first.

The builder's self-test, from the root, with `KCC20_REFERENCE_DIR` set and
`VESTING_DUMP_DIR`, `VESTING_LAUNCH_INPUT` and `VESTING_JUDGE_CHAIN` not set:

```
cargo test --frozen --test testnet_chain
```

It makes up three keys, a funding coin and a token coin, builds the whole chain at fee
rates 100 and 1000, and demands that every step is accepted and every forbidden move
refused at its layer while its twin is accepted; it writes each chain file to
`tmp/testnet-selftest` in the build's target folder (or to `VESTING_SELFTEST_DIR`) and reads
it back. The sender's tests look for those files in `target/tmp/testnet-selftest` or in
`VESTING_SELFTEST_DIR`. Then it
shows that the builder refuses a key file inside the repository, requests outside its
bounds, coins that are not the parties', a chain file that exists, a chain file with a
signature byte, a compute budget or a lock time changed after writing, and the judging of
a file with a compute budget of 1 or a raised one. The made-up key files go to a new folder in the system's
temporary folder and are removed at the end, also when the self-test fails. Its last line is
`refusals: as intended`.

The verifier's self-test, from the root, with `KCC20_REFERENCE_DIR` set and
`VESTING_VERIFY_CHAIN` not set:

```
cargo test --frozen --test testnet_verify
```

The chain file of the run passes, and the same file changed in memory, one thing at a time,
does not: its format, the template hash, the covenant ids, a party's key, the state of a
vesting coin or a claim, the script of the change, a role, the outputs of a move. Its last
line is `self-test: as intended`.

## Checking the record of the run

Offline, from the root of the repository (the first commands with `KCC20_REFERENCE_DIR`
set). The verifier computes every lock of the chain file of the run again, and its report
must be `verify-format-2.json`, next to the record, byte for byte. That is `verify.json`,
the report the run was sent with, in today's format: the same outputs, and the SHA-256 of
the chain file. Then the builder judges the file from its fields alone:

```
rm -f target/verify-of-the-run.json
VESTING_VERIFY_CHAIN=testnet/results/2026-10-15-testnet-10/chain.json \
VESTING_VERIFY_REPORT=target/verify-of-the-run.json cargo test --frozen --test testnet_verify
cmp target/verify-of-the-run.json testnet/results/2026-10-15-testnet-10/verify-format-2.json
VESTING_JUDGE_CHAIN=testnet/results/2026-10-15-testnet-10/chain.json cargo test --frozen --test testnet_chain
```

The verifier ends with `36 checks, 0 mismatches`, the builder with
`judged from its fields alone, 9 moves as the file says`. Then the sender's check of the same file;
without `--harness` its check j prints NOT CHECKED for the KCC20 locks (the verifier has
just computed them), and it reads no network:

```
cd testnet
python vesting_send.py check results/2026-10-15-testnet-10/chain.json --party payer=kaspatest:qr9qu3z2cfv03506hxwct55hz00acg0wynz2c733cnhy0t9v0d4kg8gzyrlea --party alice=kaspatest:qpqhsayjhjz8846pr3cxxc4gfnpdawhn3cvlht3xyyqt839u3xnzy3v7q4ckw --party beneficiary=kaspatest:qp744ct30vpv0cwhzx3k484wqmxq6jzdq6e44suppnfx9y4zaldzynqsn2asf --verify-report results/2026-10-15-testnet-10/verify-format-2.json
```

With `verify.json` instead the check passes as well and prints that the report is NOT BOUND
to the file: that format names no chain file.

The SHA-256 of `chain.json`, `verify.json` and `vesting_send.py` in
`results/2026-10-15-testnet-10/` are the `chain_sha256`, `verify_report_sha256` and
`program_sha256` of the first line of the record in its `record/`. With the network, `status` on the same file and
the same `--party` arguments shows what is on chain now.

The workflow of this repository runs these commands, the sender's tests and the self-tests
of the builder and the verifier on every push to `main`
([README](../README.md#check-it-yourself)).
