# TokenVesting: a SilverScript contract that releases KCC-20 tokens on a schedule

[![checks](https://github.com/0xlegacydag/kaspa-kcc20-token-vesting/actions/workflows/checks.yml/badge.svg)](https://github.com/0xlegacydag/kaspa-kcc20-token-vesting/actions/workflows/checks.yml)

**educational / unaudited / not for real funds**

A small contract written in SilverScript that holds tokens of the KCC-20 reference token
program ([argent-lang/kcc20-reference](https://github.com/argent-lang/kcc20-reference) at
`c8a0871`: a KCC20 program written in Argent and compiled through SilverScript) and releases
them to one beneficiary on a linear schedule with a cliff; from the end of the schedule on,
the beneficiary may take a token coin of the contract whole, tokens beyond the schedule
included. It comes with checks: almost every case is a whole transaction, judged by the
script engine and the transaction rules of a Kaspa node, run offline.

The contract passed the checks listed below, and one run on the Kaspa test network
testnet-10 with test coins ("On testnet-10" below); that is all this page claims. It is
not safe or ready for anything but study.

On this page a "coin" is a UTXO and a "move" is a transaction.

## The question

Can a contract of our own hold tokens of the real KCC-20 reference and give them out to one
recipient on a schedule, so that a node accepts the honest release and refuses every
forbidden move?

## How it works

**Ownership.** The token coin (the "vault") is owned by the contract's covenant id through
KCC-2 owner scheme `0x04`. The token program then lets the vault move whenever a coin of that
covenant is spent in the same transaction, and checks nothing else about the owner. So the
contract has to check the token's outputs itself: presence is not consent. Owner scheme
`0x03` (a script hash) would not do: the contract's script holds its state, so its hash
changes with every release.

**State.** Eight fields:

| Field | Meaning |
|---|---|
| `beneficiary` | the x-only key that signs every release and receives the tokens |
| `token_covid` | covenant id of the token family |
| `token_template` | template hash of the token program; with the covenant id it names the token |
| `start`, `cliff`, `duration` | DAA score of the start; DAA units to the cliff and to the end |
| `total` | tokens the schedule releases |
| `released` | tokens released so far |

The template hash is the one field beyond the minimum: reading another coin's state needs the
program's template, and the covenant id alone does not name the program.

**One entry, `release(sig beneficiary_sig, int claim)`**, in this order:

1. the beneficiary's signature, of the default type `All` only;
2. the parameters are in range (nobody checks them when a coin is created), and the claim
   is positive;
3. time: `require(tx.daa >= start + cliff)`, then `now = OpTxLockTime()`;
4. before the end of the schedule (`now < start + duration`) the claim must be at most
   vested minus released, vested being `total * (now - start) / duration`, rounded down, in
   a form whose products cannot pass 2^63; from the end on the only limit is the token coin
   (step 6): the beneficiary may take all of it, tokens beyond `total` included;
5. exactly one coin of the token family is spent, at most 8 inputs in all (`MAX_INPUTS`),
   and no covenant coin other than the vault and this one: a coin of this covenant in a
   transaction lets the token program move every coin the covenant owns;
6. the vault's state is read with the token template, and it must be owned by this covenant
   through scheme `0x04` and hold at least the claim;
7. the token outputs: the claim to the beneficiary's key (scheme `0x00`), the rest back to
   this covenant (scheme `0x04`) when anything is left, both with borrowing disabled and the
   vault's extension commitment; the rest keeps at least the vault's KAS;
8. `released` becomes `released + claim`, but stops at `total`; the contract coin continues
   with it while the schedule has tokens left to release (always before its end) and while a
   rest of the vault stays with the covenant, keeps its KAS, and authorizes no other
   continuation; once `released` has reached `total` with nothing left in the token coin, the
   beneficiary chooses to keep it (to take a coin of the family sent to the covenant later)
   or to end it.

**Tokens beyond the schedule.** Tokens on the covenant beyond `total` (an over-funded vault,
a coin of the family sent later) are released only from the end of the schedule on, and only
to the beneficiary. `released` counts the schedule's tokens alone: it is set to `total` once
a claim reaches what is left of the schedule (`claim >= total - released`) and to
`released + claim` below that, so no sum can overflow, however large the claim, and no coin
ever carries `released > total`, which step 2 would refuse for ever. A coin sent to the
covenant after its contract coin has ended stays locked for ever: nothing with the covenant's
id is left to spend beside it, and no contract can change that. The contract cannot see
which other coins its covenant owns, so the choice to end the contract coin belongs to the
beneficiary's wallet: end it only when the covenant owns no other coin of the family. The
checks' own helper keeps it when a check says the covenant owns another coin;
`s2_c7` in `tests/review_beyond_continuation.rs` shows both ways: the kept coin then
releases the main vault, while an ended one leaves it locked for good.

**Why the lock time.** A node accepts a transaction only once the DAA score passes its lock
time (`check_tx_is_finalized`). `tx.daa >= x` compiles to a range check and
`OpCheckLockTimeVerify`, which makes that lock time binding for this input and of the
DAA-score kind, so `OpTxLockTime()` is a "now" the beneficiary cannot push into the future
(case 11b) nor replace by a millisecond timestamp (case 11a). `tx.time` would work the same
way in milliseconds; `this.ageDaa` counts from the coin's own creation and restarts with every
release, so it cannot measure an absolute schedule.

**A release transaction:**

| Input | Coin |
|---|---|
| 0 | the vesting coin, calling `release` |
| 1 | the vault: a KCC20 coin owned by the vesting covenant, calling `transfer` on the normal path with an empty owner witness |
| 2 | an ordinary pay-to-key coin that pays the fee and the KAS of the new token coin |

| Output | Coin |
|---|---|
| 0 | the claim, owned by the beneficiary's key |
| 1 | the rest of the vault, owned by the vesting covenant (absent when nothing is left) |
| next | the vesting coin with the new `released` (absent when the coin ends) |
| last | change |

## What a launch must check: the contract cannot

The vault opens to **any** coin that carries the vesting covenant's id, and consensus lets
one genesis give that id to several outputs (rusty-kaspa `crypto/txscript/src/covenants.rs`,
genesis groups). No script runs when coins are created. Checked here on the engine:

- a genesis that gives the id to the vesting coin and to a pay-to-key coin is accepted
  (`g1`); that key coin then takes the whole vault before the cliff, and the vesting script
  never runs (`g2`);
- a genesis with two vesting coins of one id lets the schedule be released twice (`g3`).

`g2` and `g3` are written as forbidden moves and **fail on purpose**: nothing on chain
refuses them. The defence is a check before any token is sent, in `src/launch.rs`:
`check_vesting_genesis` (the id recomputes from its genesis group, exactly one output carries
it, and that output is this contract with the intended state), `check_vault` and
`check_parameters`. Both whoever funds the vault and the beneficiary need to run them, as
for any token whose genesis is not checked by a contract.

- `check_parameters` refuses every coin the contract would refuse at every release: its
  bounds are the contract's own constants `MAX_DAA` and `MAX_DURATION`, read from the
  source, and the node's lock-time threshold for `start + cliff`; a new coin must also have
  nothing released. `tests/liveness.rs`, `l3`, tries each bound from both sides against the
  contract itself.
- `check_vault` takes the vault coin whole (its UTXO entry: covenant id, script, KAS) and
  the state it is said to carry: the covenant id must be the contract's `token_covid`, the
  contract's `token_template` must be the KCC20 program it reads, and the coin's script must
  be that program with that state; then the state: owned through scheme `0x04` by the vesting
  covenant, borrowing disabled, holding exactly what is left to release.

**The vault check is made once.** It looks at the vault at launch, but the contract releases
from any coin of the token family that its covenant owns. A coin of the family sent to the
covenant later is checked by nobody: if it is open to borrowing, a stranger can move it
through the borrow path. That is no theft (the tokens come back to the covenant), but the
coin's outpoint changes (`s_u2` in `tests/review_third_parties.rs`), so a release signed in
advance for the old outpoint can no longer be mined; `s_l8` in `tests/review_launch.rs`
shows that the contract releases from such a coin. Before a release the beneficiary looks
at which coin it spends.

## What was checked

`cargo test --frozen --no-fail-fast` runs the checks against the node's script engine and
mass calculator (rusty-kaspa `a41a333`) and the port of the rules around them in
`node-gate`, with the testnet-10 parameters of that revision at a DAA score after the
covenant fork; without `--no-fail-fast` cargo stops at the two red cases of
`tests/genesis.rs` and skips the files after it. `node tools/run-checks.mjs` runs that
command and passes only when every other check passed and `g2` and `g3` failed with their
own message.

| File | Cases | What |
|---|---|---|
| `tests/honest.rs` | 10 | first release after the cliff; exactly at the cliff; everything exactly at the end; a partial release; a chain of three releases down to zero, each spending the outputs of the one before; numbers near 2^63; the fee floor; tokens beyond the schedule: an over-funded vault taken whole at the end, a coin sent to the covenant later taken after the end, the contract coin kept after the end and used for a later coin |
| `tests/forbidden.rs` | 92 | forbidden moves, each expected to be refused at a named line of the contract, rule of the token program, or consensus rule |
| `tests/layout.rs` | 2 | the reference's coin state has seven fields: the compiler-owned `gen__kcc20_template` first; with the six fields of the KCC-20 text the honest release is refused |
| `tests/genesis.rs` | 7 | the launch hole above: 5 pass, `g2` and `g3` fail on purpose; `g6`: a release that also starts a new covenant anchored to the vesting input is accepted (such an output is not counted by `OpAuthOutputCount`); `g7`: `check_vault` takes the vault coin whole and refuses another family, another script, a state the script does not carry, another token program |
| `tests/liveness.rs` | 3 | an over-funded vault gives its extra tokens after the end; `start + cliff` outside the DAA-score domain makes a dead coin; `check_parameters` passes exactly the coins a release can open, each bound tried from both sides |
| `tests/compile.rs` | 1 | the contract compiles |
| `tests/review_launch.rs` | 9 | the launch check against forged launches; `check_parameters` and `check_vault` against coins no release can open (`s_l6`, `s_l7`); a later coin open to borrowing; a timestamp lock time past the threshold |
| `tests/review_mutants.rs` | 5 | for five rules, a transaction the contract refuses and the contract without that rule accepts (the same moves are in `tests/forbidden.rs`, group 13) |
| `tests/review_third_parties.rs` | 5 | a stranger borrowing into the vault, a relayer re-encoding or rewriting the call, a coin opened by the covenant's presence |
| `tests/review_layout.rs` | 3 | the seven-field layout against the reference's own `KCC20.sil`, and two swapped fields refused |
| `tests/review_beyond_time.rs` | 14 | the release of tokens beyond the schedule: early release through lock times, final sequences, a cliff longer than the duration, a schedule at the top of the DAA domain; claims near 2^63 after the end |
| `tests/review_beyond_continuation.rs` | 9 | the contract coin ended or kept before and after the end; a kept coin with changed fields or another program; a later coin that brings `released` to `total` while the vault still holds tokens |
| `tests/review_beyond_combo.rs` | 5 | two token coins at once, coins open to borrowing after the end, a relayer rewriting the claim after the end |

`tests/testnet_chain.rs` and `tests/testnet_verify.rs` are not checks of the contract: they
are the builder and the verifier of the testnet run (`testnet/README.md`). `cargo test`
leaves them out (`test = false` in `Cargo.toml`).

The `review_*` files hold further cases: attacks on the contract, on its launch and on the
release of tokens beyond the schedule. The names of the cases start with their group:
`f` forbidden moves (the number is the group), `g` genesis, `l` liveness, `s_l` launch,
`s_m` weakened rules, `s_u` third parties, `s_y` layout, `s2_t` and `s2_a` time and amounts
beyond the schedule, `s2_c` continuation, `s2_d` combinations.

Every transaction is judged as a submission to a mempool and inside a block; a refusal is
traced to the line of the contract or of the token program where the engine stopped. The
compile check, the comparison of sources in `s_y1` and the checks of the launch check alone
judge no transaction. Result lines go to `<target>/check-results/*.jsonl` (each run adds to
these files).

The forbidden moves, by group: release before the cliff, more than vested, a zero or negative
claim; the claim to another key or under another owner scheme or extension; the rest to
another covenant, another scheme, another key, short by one token, with less KAS, split in
two, an extra empty token coin in the last release; each field of the next state changed,
less KAS, no next coin, another program, two next coins; two vault coins, a vault of another
family, a coin of the family running another program, a PublicMint coin of the family, a
vault owned by a key or by another covenant; a second token owned by this covenant leaving in
the same transaction, and a vault leaving without the contract; no signature, a stranger's
signature, the five other signature types, a signature made for another transaction;
borrowing opened on either output, the borrow path on the vault; no token input, a missing
rest output, too many inputs, a claim of 2^63 - 1, parameters that would overflow or are out
of range; call arguments that disagree with the outputs; a millisecond lock time, a lock time
ahead of the chain, a final sequence, two vesting coins, a vault smaller than the claim;
tokens beyond the schedule before the end, to a stranger's key whole or as the rest, signed
by a stranger, the contract coin ended while a rest stays or while `released` is below
`total`, kept twice, kept with another `released` or with less KAS; five moves that tell a
weakened rule apart by the verdict alone.

**Checking the checks.** `node tools/mutation-run.mjs` takes each of the contract's 31 rules
out in turn (a `require` becomes `require(true)`, an output check is removed) and runs the
honest and forbidden cases against that weaker contract. A weakened contract is caught when at
least one check gives another result than it demands: a forbidden move accepted, or refused at
another place than the one the check names. A run whose `cargo test` fails (a build error, a
missing `KCC20_REFERENCE_DIR`) is counted as an error, neither caught nor survived, and the
program then exits with 1.

The last run caught 29 of 31 rules, with no errors. 23 rules are caught by the verdict alone
(a forbidden move is accepted without them), 6 only by the place of a refusal (another rule
refuses the same moves). Caught by place is the weaker of the two: for five rules that the
other forbidden moves catch only by place, one ordinary transaction each tells the weaker
contract apart; those moves are group 13 of `tests/forbidden.rs` (and
`tests/review_mutants.rs`).

The two survivors are `require(OpAuthOutputCount(this.activeInputIndex) == 1)` and `== 0`:
the node counts as an input's authorized outputs only the continuations of its own covenant,
so with one vesting input the first equals the `OpCovOutputCount` rule next to it, and the
second follows from the branch it stands in (no output of the covenant); no transaction tells
the contracts apart (`g6` shows a genesis output bound to the vesting input going uncounted).
They stay in the contract as a second guard. Per rule, with the moves each one alone stops:
`results/mutation-run.txt`; the rule numbers: `results/README.md`.

**rusty-kaspa 2.1.0.** The suite pins `a41a333`, the revision the KCC-20
reference pins. `engine-2.1.0/` replays every judged transaction (209 of them), byte for
byte, on the script engine, the covenant context and the mass calculator of the rusty-kaspa
2.1.0 packages from crates.io: all 209 are judged the same. Two of them (`s_l4`, `s_l5`)
are refused on both revisions before any script runs, by the same covenant rule ("do not
correspond to the expected genesis hashing"). The rules around the engine (lock-time
finality, fees, mempool policy) were not ported to 2.1.0 and are not part of that replay.

## Numbers of an honest release

The first release after the cliff, coins of 1 KAS, fee at the relay floor
(`tests/honest.rs`, `mass_budget_and_fee_of_an_honest_release`):

| | |
|---|---|
| Script of the contract | 2042 bytes (`vesting_script_bytes` in the record "measure: one sompi under the fee floor") |
| Transaction size (estimated) | 7014 bytes |
| Compute mass | 12,084 grams |
| Transient mass | 28,056 grams |
| Storage mass | 117,651 grams |
| Compute budgets | vesting input 22 (224,167 script units), vault 4 (44,342), payer 10 (100,001) |
| Smallest fee the mempool relays | 1,402,800 sompi (about 0.014 KAS); one sompi less is refused by the mempool, accepted in a block |

## On testnet-10

On 2026-10-15 the contract ran once on testnet-10, with test coins and throwaway keys: a
vesting coin, a vault of 10 tokens of a test token made from the KCC20 reference, three
releases (3 tokens at the cliff, 3 more halfway, the last 4 at the end of the schedule),
and four moves that must be refused, sent between them. Nodes of version 2.1.0 accepted
the five honest moves exactly as they were built and signed offline, and refused the four:

| Forbidden move | The node's answer |
|---|---|
| a release before the cliff | `Unsatisfied lock time`: the contract's `require(tx.daa >= start + cliff)` |
| everything, dated at the end of the schedule and sent long before it | `transaction input #0 is not finalized`, before any script |
| more than has vested | `script ran, but verification failed` |
| the claim owned by another key | `script ran, but verification failed` |

The record, with every transaction, the nodes' answers word for word and links to a public
explorer: [testnet/results/2026-10-15-testnet-10](testnet/results/2026-10-15-testnet-10/README.md).
The programs that built, verified and sent the chain, and how to check the record again:
[testnet/README.md](testnet/README.md).

## What this does not show

- A live network beyond one run: one schedule, one fee rate, one day on testnet-10 (above);
  the rules around the engine in the checks are a port of one revision.
- The genesis of the vesting coin and of the token: no script checks them (see above); the
  launch checks ran on one real genesis, that of the testnet run.
- Revocation, several tranches, several beneficiaries: not built.
- Any other token program: only the pinned KCC20 template, whose prefix and suffix lengths
  (1 and 3885 bytes) are constants of the contract.
- Coins that stay locked: coins of the family sent to the covenant after its contract coin
  ended, coins of other families sent to it, KAS of a vesting coin whose vault ran out before
  the schedule did (it ends only with a release from a token coin), a coin made with
  parameters the contract refuses (`tests/liveness.rs`, `check_parameters`).
- Any coin unlocked merely by the presence of the covenant's id that is not a covenant coin
  itself: the contract does not see such coins.
- A wallet: the choice to keep or end the contract coin after the end is the beneficiary's,
  and no wallet that makes it was built.
- An audit.

## Check it yourself

Rust through rustup (`rust-toolchain.toml` pins 1.94.1), a C compiler, Node.js, git, and a
clone of the reference. In bash, in the root of this repository:

```
git clone -c core.autocrlf=false https://github.com/argent-lang/kcc20-reference
git -C kcc20-reference checkout c8a087117735a1f87c5c6d115fcddeaf2562c784
KCC20_REFERENCE_DIR=$(realpath kcc20-reference)
export KCC20_REFERENCE_DIR
export VESTING_DUMP_DIR=target/vesting-dump
cargo fetch --locked
cargo fmt --check
cargo clippy --frozen --all-targets -- -D warnings
node tools/run-checks.mjs
```

The last command prints the output of `cargo test --frozen --no-fail-fast` and ends with
`run-checks: 165 checks in 13 test files; 163 passed; g2 and g3 failed with their own
message, as they must (the launch hole).` With `VESTING_DUMP_DIR` set the checks also write
every transaction they judged to `target/vesting-dump/cases.jsonl`; the replay on the 2.1.0
packages reads it (remove the file before running the checks again, they add to it):

```
cd engine-2.1.0
cargo fetch --locked
cargo fmt --check
cargo clippy --frozen --all-targets -- -D warnings
cargo run --frozen --release -- ../target/vesting-dump/cases.jsonl
```

It ends with `209 transactions: 209 judged the same on 2.1.0, 0 differently`. On Windows
with the Microsoft toolchain the replay crate does not build as published: the
`kaspa-hashes` 2.1.0 package on crates.io lacks `keccakf1600_x86-64-msvc.asm`, which the
rusty-kaspa repository has. The main suite takes rusty-kaspa from git and is not affected.

The programs of the testnet run, their tests and the check of its record need Python 3.12
as well, with the pinned Kaspa SDK (`python -m pip install --require-hashes -r
requirements.txt`): `testnet/README.md`, "Tests" and "Checking the record of the run".

Checking the checks, back in the root; it builds and runs the honest and forbidden cases 31
times, and the last line of `results/mutation-run.txt` gives the time of its last run:

```
node tools/mutation-run.mjs
```

The workflow [`.github/workflows/checks.yml`](.github/workflows/checks.yml) runs the first
block of commands on pushes to `main`, on pull requests and when started by hand, on
machines hosted by GitHub: `ubuntu-24.04`, `macos-26` and `windows-2025`, in bash; on
`ubuntu-24.04` it also runs the replay. On all three it runs the self-tests of the builder
and the verifier, both programs on the chain file of the testnet run, and, in a job of its
own, the tests of the sender, its check of that chain file and the checksums the record names. It does not
run the replay on Windows (see above) or on macOS (to keep the run short), nor the mutation
run, nor anything that asks the network. A green mark beside a commit says
that these commands passed on those machines for that commit; for a pull request they are
run on its merge with the base branch. A red mark can also have its cause outside this
repository: every run downloads the Rust toolchain, the dependencies and the reference again.

## Versions

| | |
|---|---|
| SilverScript | `3ed9733`, tag `v1.0.0` (package `silverscript-lang` 1.0.0; its compiler names itself 0.1.0, the version the contract's `pragma silverscript ^0.1.0` asks for) |
| rusty-kaspa | `a41a333` (pinned by the reference; its packages carry version 2.0.1); the replay on the 2.1.0 packages from crates.io |
| KCC-20 reference | `c8a0871` |
| Argent | `9a9f4b1` |
| `node-gate`, `kcc20-family` | [kaspa-kcc20-reference-study](https://github.com/0xlegacydag/kaspa-kcc20-reference-study) at `4d111ff` |
| Rust | 1.94.1 |
| Kaspa Python SDK | `kaspa` 2.1.0 from PyPI (the sender of the testnet run), Python 3.12 |

Every version is pinned by `Cargo.lock`, `engine-2.1.0/Cargo.lock` and `requirements.txt`.

## License

ISC, see `LICENSE`. `NOTICE` says what in this repository comes from the work of others.
