# Results of the last runs

Run on 2026-10-14 (UTC) on Linux x86_64 (4 cores), Rust 1.94.1 from `rust-toolchain.toml`,
with the reference clone at `c8a0871`, from a copy of this tree made with `git archive`.
Nothing here was sent to a network.

| File | What |
|---|---|
| `cargo-test-summary.txt` | the summary lines of `node tools/run-checks.mjs` (`cargo test --frozen --no-fail-fast`), 20:43:33 to 20:47:23 UTC, the build of the checks included: 165 checks in 13 test files, 163 pass, `g2` and `g3` of `tests/genesis.rs` fail on purpose with their own message (see the main README, "What a launch must check") |
| `*.jsonl` | the files `<target>/check-results/*.jsonl` of that run, one per test file except `tests/compile.rs`, in the order the checks wrote them: one line per judged transaction (or per check of the launch check alone) with the expectation, the result, the submit and block verdicts, the place where every input stopped, budgets, masses, fee |
| `replay-2.1.0.txt` | the replay of the 209 transactions those checks judged on the rusty-kaspa 2.1.0 packages (`engine-2.1.0/`), 20:48:47 to 20:51:21 UTC, the build of the replay crate included: same transaction id, same verdict and script units of every input or the same refusal of the covenant context, same masses, for all 209 |
| `mutation-run.txt`, `mutation-summary.json` | the output of `node tools/mutation-run.mjs` and the `<target>/mutation/summary.json` it writes (92 forbidden moves and the 16 transactions of the 10 honest cases): each of the 31 rules taken out in turn, 20:51:43 to 21:20:25 UTC (29 minutes); caught 29, survived 2 (R28, R31), no run failed; 23 let at least one forbidden move through, 6 are caught only by the place of a refusal (R15, R16, R17, R20, R21, R27) |

A result line says `"result":"as expected"` when the case met its expectation. The two
records of `g2` and `g3` in `genesis.jsonl` say `NOT AS EXPECTED`: that is the launch hole
(main README, "What a launch must check"). In `mutation-run.txt`, the lines `accepted: ...`
under a rule (`now_accepted` in `mutation-summary.json`) list the forbidden moves a node
accepts once the rule is gone; the lines `written for it, now refused at ...`
(`cases_written_for_this_rule`) list the cases written for that rule which another rule
still refuses.

## The rules, by number

The numbers the mutation runner gives the rules of `contracts/TokenVesting.sil`, in order:
every single-line `require` and the three output-state checks.

| Rule | Line | Text |
|---|---|---|
| R01 | 63 | `require(byte[](beneficiary_sig).slice(64, 65) == byte[](0x01));` |
| R02 | 64 | `require(checkSig(beneficiary_sig, pubkey(beneficiary)));` |
| R03 | 68 | `require(start >= 0);` |
| R04 | 69 | `require(start <= MAX_DAA);` |
| R05 | 70 | `require(cliff >= 0);` |
| R06 | 71 | `require(cliff <= MAX_DAA);` |
| R07 | 72 | `require(duration > 0);` |
| R08 | 73 | `require(duration <= MAX_DURATION);` |
| R09 | 74 | `require(total > 0);` |
| R10 | 75 | `require(released >= 0);` |
| R11 | 76 | `require(released <= total);` |
| R12 | 77 | `require(claim > 0);` |
| R13 | 80 | `require(tx.daa >= start + cliff);` |
| R14 | 92 | `require(claim <= vested - released);` |
| R15 | 99 | `require(OpCovInputCount(own_covid) == 1);` |
| R16 | 100 | `require(OpCovInputCount(token_covid) == 1);` |
| R17 | 102 | `require(tx.inputs.length <= MAX_INPUTS);` |
| R18 | 106 | `require(OpInputCovenantId(i) == byte[32](0x0000000000000000000000000000000000000000000000000000000000000000));` |
| R19 | 118 | `require(vault.owner == own_covid);` |
| R20 | 119 | `require(vault.owner_scheme == OWNER_COVENANT_ID);` |
| R21 | 120 | `require(vault.amount >= claim);` |
| R22 | 126 | `require(OpCovOutputCount(token_covid) == 2);` |
| R23 | 128 | `require(OpCovOutputCount(token_covid) == 1);` |
| R24 | 130 | `validateOutputStateWithInputTemplate(OpCovOutputIdx(token_covid, 0), ...)`: the claim |
| R25 | 148 | `validateOutputStateWithInputTemplate(change_output, ...)`: the rest of the vault |
| R26 | 164 | `require(tx.outputs[change_output].value >= tx.inputs[token_input].value);` |
| R27 | 179 | `require(OpCovOutputCount(own_covid) == 1);` |
| R28 | 180 | `require(OpAuthOutputCount(this.activeInputIndex) == 1);` |
| R29 | 182 | `validateOutputState(next_output, ...)`: the next vesting coin |
| R30 | 195 | `require(tx.outputs[next_output].value >= tx.inputs[this.activeInputIndex].value);` |
| R31 | 197 | `require(OpAuthOutputCount(this.activeInputIndex) == 0);` |
