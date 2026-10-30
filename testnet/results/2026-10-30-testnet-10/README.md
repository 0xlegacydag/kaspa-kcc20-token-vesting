# Run record: the vesting contract on Kaspa testnet-10, 2026-10-30

A test network and test coins without value: **KAS below always means test KAS of
testnet-10.** This is the record of an independent study, not a token offering.

This is the second run of the contract of this repository. The plan is that of the
[run of 2026-10-15](../2026-10-15-testnet-10/README.md): nine moves, as new transactions. What
is new is the programs that built, checked and sent them: they were changed after a review,
in commit
[`010a8a9`](https://github.com/0xlegacydag/kaspa-kcc20-token-vesting/commit/010a8a932982d303693a6158ab31a85b5338d5af),
and until the day of this run the changed programs had been run only against a fake node. Two things
were done on the network besides the run, and both are recorded here: one transfer that
made the token coin the run needs, and one question put to a node, what it says about a
coin that another transaction holds.

The token is the test token of
[kaspa-kcc20-reference-study](https://github.com/0xlegacydag/kaspa-kcc20-reference-study),
as on 2026-10-15. The study is not affiliated with the authors of the reference.

## The token coin

The run takes one coin of exactly 10 tokens owned by Alice's key. The run of 2026-10-15 had
left its 10 tokens with the beneficiary in coins of 3, 3 and 4, and no more can be minted:
the token's whole allowance was minted on 2026-10-11. The KCC20 reference takes up to three
token inputs in one transfer, so one transaction turned the three coins into the one:

| | |
|---|---|
| Transaction | [`659ec3c0b6516d7aac76d1c178f4a9ec7f136576ee15c0db796de7f9c5f3de3c`](https://tn10.kaspa.stream/transactions/659ec3c0b6516d7aac76d1c178f4a9ec7f136576ee15c0db796de7f9c5f3de3c) |
| Spends | `786260db9405b405687d73f0fc6ca22c032a52ce8aebd8c09f5e9acb55997e3e:0` (3 tokens), `3b5b025f4e6ee2e55fa56644ce2b47235df113a378033dbfd39b25ff97c9d8e0:0` (3), `5ae7a4770144da75a497ae83e6d6e828b716eab42817f6cc79991995f2e78460:0` (4), all of the beneficiary, 1 KAS each, and nothing else |
| Makes | `659ec3c0b6516d7aac76d1c178f4a9ec7f136576ee15c0db796de7f9c5f3de3c:0`, 10 tokens owned by Alice's key, 1 KAS; `659ec3c0b6516d7aac76d1c178f4a9ec7f136576ee15c0db796de7f9c5f3de3c:1`, 1.94870800 KAS to the payer |
| Fee | 0.05129200 KAS, out of the 2 KAS the three coins freed |
| Masses | compute 17953, transient 51292, storage 0, fee mass 25646; compute budgets 18, 13 and 13 |
| Accepted | 2026-10-30T12:32:33Z by `wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/borsh`, version 2.1.0; the new coin seen 1.1 s later |

It was signed and judged offline by a one-off program ([`collect/collect_coins.rs`](collect/collect_coins.rs),
which holds the beneficiary's key only) and checked and sent by another
([`collect/collect_coins.py`](collect/collect_coins.py), which holds no key): the id rebuilt by
the SDK, the three coins unspent at a node, and the lock of the new coin computed a second
way with `kcc20_harness.py` before anything went out. This transfer is not one of the nine
moves, and the sender does not check it. It and the two transactions of the question below
are the only moves on this page that the programs of [`testnet/`](../../README.md) did not make.

## The run

- The chain was built and signed offline at 2026-10-30T12:34:03Z from a request read from
  the network at 2026-10-30T12:33:32Z, judged again from the file with no key, checked
  by the verifier and by the sender offline and with the network, and submitted from
  2026-10-30T12:34:38Z to 2026-10-30T12:59:45Z (UTC) through two public nodes:
  `wss://boson-10.kaspa.red/kaspa/testnet-10/wrpc/borsh` until a read failed after R1 and the sender reconnected (2026-10-30T12:49:41Z), then
  `wss://muon-10.kaspa.blue/kaspa/testnet-10/wrpc/borsh`, both version 2.1.0. After the last step it was read back from a
  third, `wss://vector-10.kaspa.green/kaspa/testnet-10/wrpc/borsh` (`status-after.log`).
- 5 steps, each submitted only after every output of the step before it was visible on
  chain with the expected value, script and covenant id; a release only once the node's
  virtual DAA score was above its lock time plus 20. All 5 were accepted, each under the id
  the offline builder had calculated.
- 4 forbidden moves, sent between the steps while the coins they spend were unspent. All 4
  were refused by the node, in the same words as on 2026-10-15.
- The sender ended with exit code 0 (`exit_code` in the record's summary): every step on chain, every probe refused as expected.
- Fee rate 200 sompi per gram, charged on the larger of the mass a node's fee floor uses and
  the storage mass: 0.60356000 KAS over the 5 steps. The node's own estimate at the
  time was 100 in every bucket (`prepare.log`).

## The schedule

| | |
|---|---|
| Vesting covenant id | `0ed8c871b904bd14ae0912a8e0e2bb299ffc05f27d8e120190fa8f29cf3dc3ce` |
| Token covenant id | `0d81e3e1d9f1c9568acd38e932f22eef1ea96dd6480d351e47cf61c692a4716a` |
| The 10 tokens | Alice's token coin `659ec3c0b6516d7aac76d1c178f4a9ec7f136576ee15c0db796de7f9c5f3de3c:0` |
| Start | DAA score 610293001 (S): the score read by `prepare`, 610287001, plus 6000 |
| Cliff | S + 3000 = 610296001 |
| End | S + 10000 = 610303001 |
| Total | 10 tokens, released linearly between the start and the end, nothing before the cliff |
| Beneficiary | the key `beneficiary` below; it signs every release and receives the tokens |

## Steps

The links go to the transaction's page on a public explorer of testnet-10, run by a third
party. They are a convenience, not the evidence: that is what the nodes answered
(`send.log`, `record/`) and the read-back (`status-after.log`). At 2026-10-30T13:18:52Z the explorer's
API answered `"is_accepted": true` for each of the five (`explorer/`).

| | Step | What happens | Lock time | DAA score at hand-over | Transaction | Fee, KAS | Seconds until all outputs were seen |
|---|---|---|---|---|---|---|---|
| A1 | vesting genesis | the payer's coin creates the vesting coin: the schedule, the beneficiary, nothing released | 0 | 610287616 | [`64e26d025cf28a39f9fdd7596dd0c654791fe2b34f8df397aebf2cd2d37be473`](https://tn10.kaspa.stream/transactions/64e26d025cf28a39f9fdd7596dd0c654791fe2b34f8df397aebf2cd2d37be473) | 0.08012000 | 1.2 |
| A2 | fund the vault | Alice moves her 10-token coin whole to the vesting covenant (owner scheme 4) | 0 | 610287629 | [`8207361c0102a0b5fd097c24f292b1beaa3db6c4ef642c628f657d57abc7f11a`](https://tn10.kaspa.stream/transactions/8207361c0102a0b5fd097c24f292b1beaa3db6c4ef642c628f657d57abc7f11a) | 0.06862000 | 1.2 |
| R1 | first release | 3 tokens to the beneficiary at the cliff; 7 stay in the vault | S + 3000 | 610296077 | [`f5d86e8bb89b8b721b49961b60d90578280cf4d38ba8e92e46bedeaaf759b277`](https://tn10.kaspa.stream/transactions/f5d86e8bb89b8b721b49961b60d90578280cf4d38ba8e92e46bedeaaf759b277) | 0.20803800 | 1.5 |
| R2 | second release | 3 more at S + 6500, where 6 have vested; 4 stay | S + 6500 | 610299558 | [`741d60a7f6b416452befc44c4b5af1fcd84a6c5baf58831f48f76182084bc5ee`](https://tn10.kaspa.stream/transactions/741d60a7f6b416452befc44c4b5af1fcd84a6c5baf58831f48f76182084bc5ee) | 0.20524600 | 2.0 |
| R3 | the rest | the last 4 at the end of the schedule; the vault is empty and the vesting coin ends | S + 10000 | 610303031 | [`14858a52790e599127888378a2cb13ead451db84a30b0f3e51fa68d9e7b08b1e`](https://tn10.kaspa.stream/transactions/14858a52790e599127888378a2cb13ead451db84a30b0f3e51fa68d9e7b08b1e) | 0.04153600 | 1.5 |

## Forbidden moves

The same four as on 2026-10-15, each differing from an honest move in one thing; what each
is, and why it must be refused, is on [that run's page](../2026-10-15-testnet-10/README.md#forbidden-moves).
Offline, the builder judged each at the DAA score it was to be sent at: refused at the
place named below, while its twin, the same move with the one wrong thing put right, was
accepted (`build.log`). A live node does not name a contract line; its answer is given word
for word, as the record holds it.

- **B1 a release before the cliff.** Transaction `9e44ee58ab48504bd6c7282e207aa384ed2632b8454b45383296439f23311597`, handed
  over at DAA score 610294044. Offline: refused at input 0 by
  `require(tx.daa >= start + cliff);`. The node: `RPC Server (remote error) -> Rejected transaction 9e44ee58ab48504bd6c7282e207aa384ed2632b8454b45383296439f23311597: failed to verify the signature script: Unsatisfied lock time: locktime requirement not satisfied -- locktime is greater than the transaction locktime: 610296001 > 610294001`.
- **B2 everything, dated at the end of the schedule.** Transaction
  `71e093e85443bc1def7e7437087f15b6a9f1fff67933ca3f8b4b1577c23fb9f1`, handed over at DAA score 610294047,
  long before its lock time. Offline: refused by the consensus rule of lock-time finality,
  before any script. The node: `RPC Server (remote error) -> Rejected transaction 71e093e85443bc1def7e7437087f15b6a9f1fff67933ca3f8b4b1577c23fb9f1: transaction input #0 is not finalized`.
- **B3 more than has vested.** Transaction `48b8af496a6d4bb909ea87f157e97c10dd86d404111bb37b106502398dbafac2`, handed over at
  DAA score 610296067. Offline: refused at input 0 by
  `require(claim <= vested - released);`. The node: `RPC Server (remote error) -> Rejected transaction 48b8af496a6d4bb909ea87f157e97c10dd86d404111bb37b106502398dbafac2: failed to verify the signature script: script ran, but verification failed`.
- **B4 the claim to another key.** Transaction `f70087c0da3b17dc47ee5836e6221ca60f2b51e10618ff364bca5a474177551a`, handed over
  at DAA score 610296071. Offline: refused at input 0 by the check of
  the claim's output. The node: `RPC Server (remote error) -> Rejected transaction f70087c0da3b17dc47ee5836e6221ca60f2b51e10618ff364bca5a474177551a: failed to verify the signature script: script ran, but verification failed`.

"Script ran, but verification failed" does not say which rule refused: for B3 and B4 the
rule comes from the offline engine only.

A refused transaction leaves nothing on chain: the explorer's API answered "Transaction not
found" for the four (`explorer/`), as for a transaction never sent.

## A coin that another transaction holds

A node's refusal that says "already" can mean two different things: that this transaction
is with the node, or that another transaction holds its coin. The sender tells them apart
by the transaction a refusal names, and its rule for the second case had only been read in
the node's source, rusty-kaspa v2.1.0, `mining/errors/src/mempool.rs`. So a node was asked.

Two plain transactions of the payer on one coin (`da5a7ffe0f388af7b4f56e2652aa2750784e46782df90a72abdef51ca4bde74e:0`, 1.00000000 KAS),
with different fees, were handed together over one connection to
`wss://vector-10.kaspa.green/kaspa/testnet-10/wrpc/borsh` at 2026-10-30T12:35:22.907038Z. It took one try
([`node-words/double_spend.py`](node-words/double_spend.py); the record:
[`node-words/double-spend.jsonl`](node-words/double-spend.jsonl)). The first, `662165866188d658b52afaf6366e627982e5901a8eed3af48ede35649d74b35b`, was
accepted. The second, `ccd32944605dc84f6236a95fdc57a1d1108d8552e69180e9f1ef1ec26155d4ac`, was refused:

`RPC Server (remote error) -> Rejected transaction ccd32944605dc84f6236a95fdc57a1d1108d8552e69180e9f1ef1ec26155d4ac: output (da5a7ffe0f388af7b4f56e2652aa2750784e46782df90a72abdef51ca4bde74e, 0) already spent by transaction 662165866188d658b52afaf6366e627982e5901a8eed3af48ede35649d74b35b in the mempool`

These are the words of the source and of the sender's tests. Then this answer, with the
ids of the fixture put in and nothing else changed, was handed to the sender's `send`
through its fake node ([`node-words/real_words_path.py`](node-words/real_words_path.py), run
from `testnet/`). Naming a transaction the chain file does not know, the sender stops with
exit code 6, says that the node names that transaction as the holder of a coin and does not
call it a forbidden move, and the next run goes on and ends with 0. Naming the probe itself,
it stops with exit code 5 (`node-words/real-words-path.log`). That later runs with such a
record send nothing is a test of the sender,
`test_already_about_a_probe_of_the_file_or_no_transaction_blocks_the_record`; this script does
not run it.

## Sending again

`send` was run a second time on the finished chain, with a copy of the record folder, so
that the record of the run stays as it was (`second-send/`). It handed nothing over: five
steps found on chain, four probes not sent because the chain is past them, exit code 0.

## Balances of the three keys

| Party | Address | Before the run, KAS | After, KAS |
|---|---|---|---|
| payer | `kaspatest:qr9qu3z2cfv03506hxwct55hz00acg0wynz2c733cnhy0t9v0d4kg8gzyrlea` | 961.34439716 | 958.73678916 |
| alice | `kaspatest:qpqhsayjhjz8846pr3cxxc4gfnpdawhn3cvlht3xyyqt839u3xnzy3v7q4ckw` | 0.00000000 | 0.00000000 |
| beneficiary | `kaspatest:qp744ct30vpv0cwhzx3k484wqmxq6jzdq6e44suppnfx9y4zaldzynqsn2asf` | 0.00000000 | 0.00000000 |

The same three throwaway test keys as on 2026-10-15. The balance of an address counts its
plain coins only; a token coin or a vesting coin holds 1 KAS of its own and is counted at
no party's address. The payer's coin of 13.99179600 KAS and the 1 KAS
of Alice's token coin paid for the whole run: 3.00000000 stay in the beneficiary's three
token coins, 0.60356000 were fees, and 11.38823600 came back as change. So the run
took 2.60356000 from the payer. The other 0.00404800 of the difference in the table is the
fee of the accepted transaction of the question above, which fell within the run.

## Coins of the run that are unspent after the last step

As the third node listed them (`status-after.log`):

| Transaction | Output | KAS | What |
|---|---|---|---|
| `f5d86e8bb89b8b721b49961b60d90578280cf4d38ba8e92e46bedeaaf759b277` | 0 | 1.00000000 | token coin of the beneficiary: 3 tokens |
| `741d60a7f6b416452befc44c4b5af1fcd84a6c5baf58831f48f76182084bc5ee` | 0 | 1.00000000 | token coin of the beneficiary: 3 tokens |
| `14858a52790e599127888378a2cb13ead451db84a30b0f3e51fa68d9e7b08b1e` | 0 | 1.00000000 | token coin of the beneficiary: 4 tokens |
| `14858a52790e599127888378a2cb13ead451db84a30b0f3e51fa68d9e7b08b1e` | 1 | 11.38823600 | change to the payer |

No coin of the vesting covenant is left: R3 ended the vesting coin with an empty vault.

## Files in this folder

The files below are kept as the programs and the shell wrote them, with the exceptions
named at the end. This page is a summary of them, written after the run.

- `token-state.json` - the state of Alice's 10-token coin, written when the transfer above
  was seen on chain; `prepare` checked that the coin on chain has the lock of this state.
- `collect/` - that transfer: the two one-off programs, what was read from a node
  (`collect-request.json`), the transaction (`collect-tx.json`), what the program printed
  (`build.log`, and `build-again.log` from a later rerun), and what the node answered
  (`collect-record.jsonl`, `send.log`).
- `request.json`, `prepare.log` - what `prepare` read from the network before building: the
  DAA score, the payer's funding coin, the token coin, the node's fee estimate; and the fee
  rate of the run.
- `chain.json`, `chain.json.txt` - every transaction in full, as built and signed offline and
  judged by the builder, and a table of it for a reader; SHA-256 of `chain.json`:
  `98d02dfabc8014416ca9f2734f8db7a16de017afba6090e589879f6d5a6ee87e`. Both were written before sending: that is why they say that
  nothing was sent, and "accepted" and "refused" in them are the answers of the offline
  builder, not of the network.
- `build.log` - what the builder printed: each forbidden move with the place where the
  engine stopped and its twin, and the read-back of the file.
- `verify.json`, `verify.log` - the verifier's report and what it printed: the lock of each
  of the 19 vesting and token outputs computed again from the state the chain file gives
  for it, 36 checks, no mismatch; the report names the chain file by its SHA-256.
- `judge.log` - the builder judging the chain file again from its fields alone, with no key.
- `check-offline.log`, `check-online.log` - the sender's check of the chain file, without
  and with the network, before anything of the run was sent.
- `send.log`, `record/` - the run as it happened: the screen, and one JSON object per event
  with the time in UTC, the node, the DAA score of every hand-over and every answer word for
  word, then a summary. The record names the chain file and the moves in it, the sender, the
  harness and the verifier's report by their SHA-256.
- `status-after.log` - the chain read back from the third node after the run.
- `second-send/` - the second `send`, with the two files it added to the copy of the record.
- `node-words/` - the question to a node and the sender's path on its answer.
- `explorer/` - the answers of the explorer's API (`https://api-tn10.kaspa.org/transactions/<id>`),
  read at the time in `read_at_utc.txt`: the nine moves by their names, `C1` the transfer of
  the token coin, `D1` and `D2` the two transactions of the question.

The exceptions: in `prepare.log`, `build.log`, `verify.log`, `judge.log`, `collect/build.log`
and `collect/send.log` the folders of the run and of the build on the computer that made
them are cut from the paths, and in `collect/build.log` the crate's folder reads `the clone`;
nothing else is changed. The record and `second-send/` name nothing of the kind.
`collect/collect_coins.rs`, `collect/collect_coins.py` and `node-words/double_spend.py` are the
programs that ran, with these changes made afterwards: new comments at their heads, `rustfmt`
on one statement of the first, and in the second the paths of the chain file and the harness
no longer written in. Copied to `tests/` and run again from `collect-request.json` with the
beneficiary's key, the first gave the same transaction id (`collect/build-again.log`). With no
key, the sender's `build_transaction` rebuilds that id from the fields of `collect-tx.json`.
The clock times in `collect/build.log` are the shell's, UTC+2.

## The programs

The builder, the verifier and the sender are `tests/testnet_chain.rs`, `tests/testnet_verify.rs`
and `testnet/vesting_send.py` as they are at revision `010a8a9` of this repository; the
SHA-256 of the sender is the record's `program_sha256`, `aa54d7864a0dff7cb8bf996cc12ba6027cf5b47eb3af495c3af21540a3031a0e`.
The harness the record names, `kcc20_harness.py` (`8e5381ea1ca2643881c09f8319d586be596bfade3ddc8ddce5c7295feea35dbb`), is not in
this repository. How to check this record again:
[testnet/README.md](../../README.md#checking-the-record-of-the-run).

## What this shows and what it does not

It shows that the programs as changed after their review carry the whole chain through a
live network: testnet-10 nodes of version 2.1.0 accepted the five steps exactly as built
offline and refused the four forbidden moves as the file says; the sender read those
refusals by the words its format names, and a report bound to the chain file by its SHA-256
was what it was sent with. It shows one transfer of three token coins into one, accepted. And
it shows one real answer to a second spend of a coin, in the words the sender expects. It is
one more run, with the same fee rate and the same plan of moves, and not an audit.

It does not show:

- the sender stopping on a live answer that a coin of a probe is held by another transaction:
  that did not happen in this run, and the window for it is short: from the sender's last look
  at the coin to the hand-over. Its path was run with a real answer through the fake node, and
  the answer itself came from a plain coin, one node, one try;
- what a node says in its other refusals with "already": they are read in the source only;
- which rule refused B3 and B4: a node does not say; the rule is the offline engine's;
- the twins of the forbidden moves on chain: they were judged offline only;
- a chain reorganisation, another fee rate, or a schedule other than this one;
- the launch hole of the README (`g2`, `g3`), tokens beyond the schedule, a later coin sent to
  the covenant, a vault that is open to borrowing: none of these was tried on chain;
- that the explorer's pages will stay: a test network can be reset, and by the reference's
  rules anyone may use the token's seed, so what is on chain today may change.
