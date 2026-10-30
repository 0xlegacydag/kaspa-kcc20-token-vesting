# Run record: the vesting contract on Kaspa testnet-10, 2026-10-15

A test network and test coins without value: **KAS below always means test KAS of
testnet-10.** This is the record of an independent study, not a token offering. The
contract of this repository, [`contracts/TokenVesting.sil`](../../../contracts/TokenVesting.sil),
was given 10 tokens of a test token and released them to one beneficiary on its schedule,
in three releases; four moves the contract or a node must refuse were sent in between, to
see whether a live network judges them the way the offline checks did.

The token is the test token that
[kaspa-kcc20-reference-study](https://github.com/0xlegacydag/kaspa-kcc20-reference-study)
launched on testnet-10 on 2026-10-11 from the KCC20 reference contracts
(`argent-lang/kcc20-reference` at `c8a0871`); its record is
`launch/results/2026-10-11-testnet-10/` in that repository. The 10 tokens are the coin that
run minted to Alice in its step 9. The study is not affiliated with the authors of the
reference.

## The run

- The chain was built and signed offline at 2026-10-15T08:42:22Z from a request read from
  the network 38 seconds earlier, checked by the sender offline and with the network, and
  submitted from 08:43:29Z to 09:08:47Z (UTC) through the public node
  `wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh` (version 2.1.0). About a minute
  after the last step it was read back from a second one,
  `wss://proton-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh` (version 2.1.0).
- 5 steps, each submitted only after every output of the step before it was visible on
  chain with the expected value, script and covenant id; a release only once the node's
  virtual DAA score was above its lock time plus 20. All 5 were accepted, each under the id
  the offline builder had calculated.
- 4 forbidden moves, sent between the steps while the coins they spend were unspent. All 4
  were refused by the node.
- Fee rate 200 sompi per gram, charged on the larger of the mass a node's fee floor uses and
  the storage mass: 0.62222400 KAS over the 5 steps. The node's own estimate at the time was
  100 in every bucket (`prepare.log`).

## The schedule

| | |
|---|---|
| Vesting covenant id | `82595cf62929f94559e1b93406adeeeaaa2b931f547292e4ac5401cbbc5a9b1b` |
| Token covenant id | `0d81e3e1d9f1c9568acd38e932f22eef1ea96dd6480d351e47cf61c692a4716a` |
| The 10 tokens | Alice's token coin `78e23990321b7decd966c14887b59806310b6faed043a3cbe44de2489c989da5:1` |
| Start | DAA score 597207388 (S): the score read by `prepare`, 597201388, plus 6000 |
| Cliff | S + 3000 = 597210388 |
| End | S + 10000 = 597217388 |
| Total | 10 tokens, released linearly between the start and the end, nothing before the cliff |
| Beneficiary | the key `beneficiary` below; it signs every release and receives the tokens |

Testnet-10 adds about 10 to the DAA score per second: the cliff came about 15 minutes after
`prepare`, the end about 27 minutes after it.

## Steps

The links go to the transaction's page on a public explorer of testnet-10, run by a third
party. They are a convenience, not the evidence: that is what the nodes answered
(`send.log`, `record/`) and the read-back (`status-after.log`). At 09:10:23Z the explorer's
API answered `"is_accepted": true` for each of the five (`explorer/`).

| | Step | What happens | Lock time | DAA score at hand-over | Transaction | Fee, KAS | Seconds until all outputs were seen |
|---|---|---|---|---|---|---|---|
| A1 | vesting genesis | the payer's coin creates the vesting coin: the schedule, the beneficiary, nothing released | 0 | 597202456 | [`bf9bd3a6ad32e0b2dfc056978c5c41ec434d253e26601e98973fe64f9f028e35`](https://tn10.kaspa.stream/transactions/bf9bd3a6ad32e0b2dfc056978c5c41ec434d253e26601e98973fe64f9f028e35) | 0.08008400 | 1.2 |
| A2 | fund the vault | Alice moves her 10-token coin whole to the vesting covenant (owner scheme 4) | 0 | 597202467 | [`51e5cbb6a9278a122848327ee3943ecac89c1d7ff3bbc98de4a5bd4e7693a947`](https://tn10.kaspa.stream/transactions/51e5cbb6a9278a122848327ee3943ecac89c1d7ff3bbc98de4a5bd4e7693a947) | 0.07040800 | 1.2 |
| R1 | first release | 3 tokens to the beneficiary at the cliff; 7 stay in the vault | S + 3000 | 597210465 | [`786260db9405b405687d73f0fc6ca22c032a52ce8aebd8c09f5e9acb55997e3e`](https://tn10.kaspa.stream/transactions/786260db9405b405687d73f0fc6ca22c032a52ce8aebd8c09f5e9acb55997e3e) | 0.21277200 | 1.5 |
| R2 | second release | 3 more at S + 6500, where 6 have vested; 4 stay | S + 6500 | 597213935 | [`3b5b025f4e6ee2e55fa56644ce2b47235df113a378033dbfd39b25ff97c9d8e0`](https://tn10.kaspa.stream/transactions/3b5b025f4e6ee2e55fa56644ce2b47235df113a378033dbfd39b25ff97c9d8e0) | 0.21077200 | 1.4 |
| R3 | the rest | the last 4 at the end of the schedule; the vault is empty and the vesting coin ends | S + 10000 | 597217462 | [`5ae7a4770144da75a497ae83e6d6e828b716eab42817f6cc79991995f2e78460`](https://tn10.kaspa.stream/transactions/5ae7a4770144da75a497ae83e6d6e828b716eab42817f6cc79991995f2e78460) | 0.04818800 | 1.2 |

Every release spends the vesting coin (input 0), the vault (input 1) and a coin of the
payer for the fee (input 2), as in the README of this repository.

## Forbidden moves

Each spends the same vesting coin and vault as R1 and differs from an honest move in one
thing. Offline, the builder judged each with the node's script engine and the node rules of
`node-gate` at the DAA score it was to be sent at: refused at the place named below, while
its twin, the same move with the one wrong thing put right, was accepted (`build.log`). A
live node does not name a contract line; its answer is given word for word, as the record
holds it.

- **B1 a release before the cliff.** 1 token, lock time S + 1000. Transaction
  `971a81e7802ac055cef34536d75780aebbc62948a1626f9af091a97537afcac6`, handed over at DAA
  score 597208452. Offline: refused at input 0 by `require(tx.daa >= start + cliff);`.
  The node: `RPC Server (remote error) -> Rejected transaction 971a81e7802ac055cef34536d75780aebbc62948a1626f9af091a97537afcac6: failed to verify the signature script: Unsatisfied lock time: locktime requirement not satisfied -- locktime is greater than the transaction locktime: 597210388 > 597208388`.
  The contract's time check compiles to `OpCheckLockTimeVerify`: the cliff, 597210388, is
  above the lock time the move commits to.
- **B2 everything, dated at the end of the schedule.** All 10 tokens, lock time S + 10000,
  handed over long before that, at DAA score 597208455. Transaction
  `4a64fd8d81a56cff02876136a89965829008b00f0ef518d7c2bd4b1e061775e3`. Offline: refused by
  the consensus rule of lock-time finality, before any script. The node: `RPC Server (remote error) -> Rejected transaction 4a64fd8d81a56cff02876136a89965829008b00f0ef518d7c2bd4b1e061775e3: transaction input #0 is not finalized`.
  The contract reads "now" from the lock time (`OpTxLockTime()`), and the node does not
  take a transaction whose lock time is still ahead of the chain: a spender cannot move
  "now" into the future. The sender would not have handed B2 over within 2000 of its lock
  time: past that, the same transaction becomes an honest release of everything.
- **B3 more than has vested.** 4 tokens at the cliff, where 3 have vested. Transaction
  `a99bc7dc2ec69a0c52c0403bb516c7f2607a819676f1d60a7f9914c77de83c31`, handed over at DAA
  score 597210462. Offline: refused at input 0 by `require(claim <= vested - released);`.
  The node: `RPC Server (remote error) -> Rejected transaction a99bc7dc2ec69a0c52c0403bb516c7f2607a819676f1d60a7f9914c77de83c31: failed to verify the signature script: script ran, but verification failed`.
- **B4 the claim to another key.** 3 tokens at the cliff, signed by the beneficiary, but the
  new token coin is owned by the payer's key. Transaction
  `92d4870386ed4624da5793576c6ce0f44ee9d66800a627e0524feb35945f9a3a`, handed over at DAA
  score 597210463. Offline: refused at input 0 by the check of the claim's output,
  `validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0), KCC20State { ... owner: beneficiary, owner_scheme: OWNER_P2PK_SCHNORR, ... } ... )`.
  The node: `RPC Server (remote error) -> Rejected transaction 92d4870386ed4624da5793576c6ce0f44ee9d66800a627e0524feb35945f9a3a: failed to verify the signature script: script ran, but verification failed`.

"Script ran, but verification failed" does not say which rule refused: for B3 and B4 the
rule comes from the offline engine only. For each of B1, B3 and B4 the scripts of the other
two inputs pass offline, so the node had no other input to refuse.

A refused transaction leaves nothing on chain: the explorer's API answered "Transaction not
found" for the four (`explorer/`), as for a transaction never sent.

## Balances of the three keys

| Party | Address | Before, KAS | After, KAS |
|---|---|---|---|
| payer | `kaspatest:qr9qu3z2cfv03506hxwct55hz00acg0wynz2c733cnhy0t9v0d4kg8gzyrlea` | 962.01791316 | 959.39568916 |
| alice | `kaspatest:qpqhsayjhjz8846pr3cxxc4gfnpdawhn3cvlht3xyyqt839u3xnzy3v7q4ckw` | 0.00000000 | 0.00000000 |
| beneficiary | `kaspatest:qp744ct30vpv0cwhzx3k484wqmxq6jzdq6e44suppnfx9y4zaldzynqsn2asf` | 0.00000000 | 0.00000000 |

The three are throwaway test keys; payer and alice are the issuer and Alice of the run of
2026-10-11, the beneficiary a key made for this run. The balance of an address counts its
plain coins only. A token coin or a vesting coin is a pay-to-script-hash coin of its
contract and holds 1 KAS of its own, so it is counted at no party's address, not even its
owner's. The payer paid every fee. Its coin of 16.61402000 KAS and the 1 KAS of Alice's
token coin paid for the whole run: 3.00000000 stay in the beneficiary's three token coins,
0.62222400 were fees, and 13.99179600 came back to the payer as change. So the payer's
balance went down by 2.62222400: the fees and 2 of the 3 KAS that stay in token coins.

## Coins of the run that are unspent after the last step

As the second node listed them (`status-after.log`):

| Transaction | Output | KAS | What |
|---|---|---|---|
| `786260db9405b405687d73f0fc6ca22c032a52ce8aebd8c09f5e9acb55997e3e` | 0 | 1.00000000 | token coin of the beneficiary: 3 tokens |
| `3b5b025f4e6ee2e55fa56644ce2b47235df113a378033dbfd39b25ff97c9d8e0` | 0 | 1.00000000 | token coin of the beneficiary: 3 tokens |
| `5ae7a4770144da75a497ae83e6d6e828b716eab42817f6cc79991995f2e78460` | 0 | 1.00000000 | token coin of the beneficiary: 4 tokens |
| `5ae7a4770144da75a497ae83e6d6e828b716eab42817f6cc79991995f2e78460` | 1 | 13.99179600 | change to the payer |

No coin of the vesting covenant is left: R3 ended the vesting coin with an empty vault.

## Files in this folder

The files below are kept as the programs and the shell wrote them, with the two exceptions
named. This page is a summary of them, written after the run. The last two came here later.

- `token-state.json` - the state of Alice's 10-token coin as the record of 2026-10-11 gives
  it; its field `source` names the place. `prepare` checked that the coin on chain has the
  lock of this state.
- `request.json`, `prepare.log` - what `prepare` read from the network before building: the
  DAA score, the payer's funding coin, the token coin, the node's fee estimate; and the fee
  rate of the run.
- `chain.json`, `chain.json.txt` - every transaction in full, as built and signed offline and
  judged by the builder, and a table of it for a reader; SHA-256 of `chain.json`:
  `660d6db1ee4a45862435033f7751bd3c21c2283670f5e1e6426df8423d61c3cd`. Both were written
  before sending: that is why they say that nothing was sent (the sender refuses a chain
  file that does not say so), and "accepted" and "refused" in them are the answers of the
  offline builder, not of the network.
- `build.log` - what the builder printed: each forbidden move with the place where the
  engine stopped and its twin, and the read-back of the file.
- `verify.json`, `verify.log` - the verifier's report and what it printed: the lock of each
  of the 19 vesting and token outputs computed again from the state the chain file gives
  for it, 36 checks, no mismatch.
- `check-offline.log`, `check-online.log` - the sender's check of the chain file, without
  and with the network, before anything was sent.
- `send.log`, `record/` - the run as it happened: the screen, and one JSON object per event
  with the time in UTC, the node, the DAA score of every hand-over and every answer word for
  word, then a summary. The record names the chain file, the sender, the harness and the
  verifier's report by their SHA-256.
- `status-after.log` - the chain read back from the second node after the run.
- `explorer/` - the answers of the explorer's API (`https://api-tn10.kaspa.org/transactions/<id>`)
  for the nine transactions, read at the time in `read_at_utc.txt`.
- `vesting_send.py` - the sender that sent this chain, byte for byte (below), copied here
  when the programs changed.
- `verify-format-2.json` - not written by the run: what the verifier writes today for the
  same `chain.json`. The outputs of `verify.json`, in a format that also names the chain
  file by its SHA-256.

The two exceptions: in `build.log` (one line) and `verify.log` (two lines) the folder of the
run on the computer that built it is cut from the paths of `chain.json` and `verify.json`;
nothing else is changed. In `send.log` the last line, `send exit 0`, was added by the shell
that captured the output. In `prepare.log` and `send.log`, `runs\2026-10-15\` is the folder
the run's files were in when the sender wrote them.

## The programs

[`vesting_send.py`](vesting_send.py) in this folder is byte for byte the program that sent
this chain: its SHA-256 is the record's `program_sha256`,
`afce3afbf5e596342d548820292d96dfafd7a08acb739813e926bb0a884a34f4`. The builder and the
verifier that built and verified it are `tests/testnet_chain.rs` and
`tests/testnet_verify.rs` as they are at revision `2e22382` of this repository, with two
changes made after the run that touch neither: the builder's self-test writes into
`target/tmp`, and the builder does not start while `VESTING_DUMP_DIR` is set. The programs
in [`testnet/`](../../README.md) and `tests/` have changed since that revision, after a
review of them; the commit after it says what changed. None of it touches the contract or
this record. `verify.json` is the report the run was sent with. The harness the record names,
`kcc20_harness.py`, is not in this repository. How to check this record again:
[testnet/README.md](../../README.md#checking-the-record-of-the-run).

## What this shows and what it does not

It shows that the vesting coin, the vault and three releases of this contract were
accepted by testnet-10 nodes of version 2.1.0 exactly as built offline, and that four moves
it must not allow were refused: two by the contract's scripts, one by the contract's time
check as a lock-time refusal, and one by the node before any script, because of its lock
time. It is one run on one day, with one fee rate and one schedule, and not an audit.

It does not show:

- which rule refused B3 and B4: a node does not say; the rule is the offline engine's;
- the twins of the forbidden moves on chain: they were judged offline only (the honest
  release R1 is the twin of B3 and B4);
- the launch hole of the README (`g2`, `g3`): a second coin with the vesting id, or a second
  vesting coin, was not tried on chain;
- tokens beyond the schedule, a later coin sent to the covenant, a vault that is open to
  borrowing: none of these was tried on chain;
- that the explorer's pages will stay: a test network can be reset, and by the reference's
  rules anyone may use the token's seed, so what is on chain today may change.
