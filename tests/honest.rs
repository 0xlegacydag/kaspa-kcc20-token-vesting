//! Honest releases: each must be accepted by the node's rules, as a submission and inside a
//! block.

mod common;

use common::{at, expect_accepted, node_for, run, run_scenario, schedule, show};
use kaspa_consensus_core::tx::TransactionOutpoint;
use token_vesting::judge::write_record;
use token_vesting::release::{CLIFF, DURATION, Release, TOTAL};

const FILE: &str = "honest";

#[test]
fn first_release_after_the_cliff() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 150_000);
    let release = Release::honest(
        "first release after the cliff",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    expect_accepted(FILE, &run(&release));
}

#[test]
fn release_exactly_at_the_cliff() {
    let state = schedule();
    let lock_time = at(CLIFF);
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 100_000);
    let release = Release::honest(
        "release exactly at the cliff",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    expect_accepted(FILE, &run(&release));
}

#[test]
fn release_of_everything_exactly_at_the_end() {
    let state = schedule();
    let lock_time = at(DURATION);
    let release = Release::honest(
        "release of everything exactly at the end: no change, no next coin",
        state,
        TOTAL,
        TOTAL,
        lock_time,
    );
    assert!(release.next_vesting.is_none());
    assert_eq!(release.token_outputs.len(), 1);
    expect_accepted(FILE, &run(&release));
}

#[test]
fn partial_release_and_less_than_vested() {
    let state = schedule();
    let lock_time = at(4_000);
    let release = Release::honest(
        "partial release: 1 token of 400,000 vested",
        state,
        TOTAL,
        1,
        lock_time,
    );
    expect_accepted(FILE, &run(&release));
}

/// Three releases, each spending the real outputs of the one before: a partial release,
/// another one, and the last one, which empties the vault and ends the vesting coin.
#[test]
fn a_chain_of_three_releases_down_to_zero() {
    let mut state = schedule();
    let mut vault_amount = TOTAL;
    let mut vesting_outpoint = None;
    let mut vault_outpoint = None;
    let mut previous: Option<kcc20_family::Assembled> = None;
    let steps = [
        (CLIFF + 500, 60_000, "chain 1/3: part of what is vested"),
        (
            5_000,
            440_000,
            "chain 2/3: the rest of what is vested at 5,000",
        ),
        (
            DURATION + 100,
            500_000,
            "chain 3/3: the last tokens, vault emptied",
        ),
    ];
    for (index, (offset, claim, name)) in steps.into_iter().enumerate() {
        let lock_time = at(offset);
        assert!(claim <= state.vested_at(lock_time as i64) - state.released);
        let mut release = Release::honest(name, state.clone(), vault_amount, claim, lock_time);
        release.funding_outpoint = kcc20_family::shapes::outpoint(0x40 + index as u8, 0);
        if let (Some(vesting), Some(vault)) = (vesting_outpoint, vault_outpoint) {
            release.vesting_outpoint = vesting;
            release.vault_outpoint = vault;
        }
        let scenario = release.scenario();
        let judged = run_scenario(&release, scenario, node_for(lock_time));
        if let Some(previous) = &previous {
            // The coins this release spends are the outputs the previous one created.
            let tx = &judged.assembled.tx;
            for input in 0..2 {
                let outpoint = tx.inputs[input].previous_outpoint;
                assert_eq!(outpoint.transaction_id, previous.tx.id());
                let created = &previous.tx.outputs[outpoint.index as usize];
                let spent = &judged.assembled.utxos[input];
                assert_eq!(spent.amount, created.value);
                assert_eq!(spent.script_public_key, created.script_public_key);
                assert_eq!(
                    spent.covenant_id,
                    created.covenant.map(|binding| binding.covenant_id)
                );
            }
        }
        expect_accepted(FILE, &judged);
        let released = state.released + claim;
        state = state.with_released(released);
        vault_amount -= claim;
        let tx_id = judged.assembled.tx.id();
        vault_outpoint = Some(TransactionOutpoint::new(tx_id, 1));
        vesting_outpoint = Some(TransactionOutpoint::new(tx_id, 2));
        if index == 2 {
            assert_eq!(vault_amount, 0);
            assert_eq!(state.released, TOTAL);
            // Two outputs: the claim and the funder's change.
            assert_eq!(judged.assembled.tx.outputs.len(), 2);
        }
        previous = Some(judged.assembled);
    }
}

/// Numbers near the top of the range: no product of the formula passes 2^63.
#[test]
fn large_numbers_do_not_overflow() {
    let mut state = schedule();
    state.total = 9_000_000_000_000_000_000;
    state.duration = 3_000_000_000;
    state.cliff = 0;
    let lock_time = at(state.duration - 1);
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 8_999_999_997_000_000_000);
    let release = Release::honest(
        "large numbers: total 9e18, duration 3e9, one unit before the end",
        state.clone(),
        state.total,
        claim,
        lock_time,
    );
    expect_accepted(FILE, &run(&release));
}

/// The honest release at its smallest budgets and exactly the relay fee floor: what it
/// weighs and costs. One sompi less is refused by the mempool and accepted in a block.
#[test]
fn mass_budget_and_fee_of_an_honest_release() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest("measure: first release", state, TOTAL, claim, lock_time);
    let first = run(&release);
    let minimum_fee = first
        .submit
        .verdict
        .policy
        .as_ref()
        .and_then(|policy| policy.minimum_fee)
        .expect("the policy names a minimum fee");

    release.name = "measure: first release at the relay fee floor".to_string();
    release.fee = minimum_fee;
    let at_floor = run(&release);
    expect_accepted(FILE, &at_floor);
    let masses = at_floor.submit.verdict.masses.clone().expect("masses");
    println!(
        "size {} compute mass {} transient mass {} storage mass {:?} fee {} budgets {:?} units {:?}",
        masses.estimated_size,
        masses.compute_mass,
        masses.transient_mass,
        masses.calculated_storage_mass,
        minimum_fee,
        at_floor
            .inputs
            .iter()
            .map(|input| input.budget)
            .collect::<Vec<_>>(),
        at_floor
            .inputs
            .iter()
            .map(|input| input.used_script_units)
            .collect::<Vec<_>>()
    );

    release.name = "measure: one sompi under the fee floor".to_string();
    release.fee = minimum_fee - 1;
    let under = run(&release);
    show(&under);
    let ok = !under.submit.accepted()
        && under.block.accepted()
        && under.submit.verdict.consensus_accepted;
    let mut record = under.record(
        "mempool refuses the fee, a block accepts",
        if ok { "as expected" } else { "NOT AS EXPECTED" },
    );
    // The size of the contract's script, the same in all three transactions of this check.
    record["vesting_script_bytes"] = serde_json::json!(release.compiled().bytecode.len());
    write_record(FILE, &record);
    assert!(ok);
}

// ---------------------------------------------------------------------------------------
// Tokens beyond the schedule: from the end of the schedule on, the beneficiary may take a
// token coin of the covenant whole.

/// An over-funded vault (5 tokens beyond the schedule) taken whole at the end, in one
/// release: the released amount stops at total and the vesting coin ends.
#[test]
fn an_over_funded_vault_taken_whole_at_the_end() {
    let release = Release::honest(
        "beyond 1: the whole over-funded vault (total + 5) at the end",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    assert!(release.next_vesting.is_none());
    assert_eq!(release.token_outputs.len(), 1);
    expect_accepted(FILE, &run(&release));
}

/// Runs `release` spending the given outpoints, with its own funder's coin.
fn spending(
    mut release: Release,
    vesting: Option<TransactionOutpoint>,
    vault: Option<TransactionOutpoint>,
    funder: u8,
) -> kcc20_family::Assembled {
    if let Some(vesting) = vesting {
        release.vesting_outpoint = vesting;
    }
    if let Some(vault) = vault {
        release.vault_outpoint = vault;
    }
    release.funding_outpoint = kcc20_family::shapes::outpoint(funder, 0);
    let judged = run(&release);
    expect_accepted(FILE, &judged);
    judged.assembled
}

/// A coin of the token family sent to the covenant after the launch (7 tokens, beside the
/// vault) is taken whole after the end of the schedule; then the rest of the vault, whole,
/// and the released amount stops at total (150,007 + 850,000 is past it).
#[test]
fn a_coin_sent_to_the_covenant_later_is_taken_after_the_end() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let first = spending(
        Release::honest(
            "later coin 1/3: the first release, 150,000 of the vault",
            state.clone(),
            TOTAL,
            claim,
            lock_time,
        ),
        None,
        None,
        0x60,
    );
    let state = state.with_released(claim);
    let mut late = Release::honest(
        "later coin 2/3: after the end, a coin of 7 tokens sent to the covenant later, whole",
        state.clone(),
        7,
        7,
        at(DURATION + 100),
    )
    .with_other_coins_on_the_covenant();
    late.vault_outpoint = kcc20_family::shapes::outpoint(0x61, 0);
    assert_eq!(
        late.next_vesting.as_ref().map(|next| next.released),
        Some(claim + 7)
    );
    let second = spending(
        late,
        Some(TransactionOutpoint::new(first.tx.id(), 2)),
        None,
        0x62,
    );
    let state = state.with_released(claim + 7);
    let rest = TOTAL - claim;
    let last = Release::honest(
        "later coin 3/3: the rest of the vault, whole: released stops at total",
        state,
        rest,
        rest,
        at(DURATION + 200),
    );
    assert!(last.next_vesting.is_none(), "released reaches total");
    let third = spending(
        last,
        Some(TransactionOutpoint::new(second.tx.id(), 1)),
        Some(TransactionOutpoint::new(first.tx.id(), 1)),
        0x63,
    );
    // Two outputs: the claim and the funder's change.
    assert_eq!(third.tx.outputs.len(), 2);
}

/// From the end of the schedule on, with nothing left in the token coin, the beneficiary
/// may keep the vesting coin: a coin of the family sent to the covenant later is then taken
/// with it, and that release ends it.
#[test]
fn after_the_end_the_beneficiary_may_keep_the_vesting_coin() {
    let state = schedule();
    let mut keep = Release::honest(
        "kept coin 1/2: everything at the end, the vesting coin kept (released = total)",
        state.clone(),
        TOTAL,
        TOTAL,
        at(DURATION),
    );
    assert!(keep.next_vesting.is_none());
    keep.next_vesting = Some(state.clone().with_released(TOTAL));
    let first = spending(keep, None, None, 0x64);
    let mut late = Release::honest(
        "kept coin 2/2: a coin of 7 tokens sent to the covenant later, taken with the kept coin",
        state.with_released(TOTAL),
        7,
        7,
        at(DURATION + 100),
    );
    late.vault_outpoint = kcc20_family::shapes::outpoint(0x65, 0);
    assert!(late.next_vesting.is_none());
    spending(
        late,
        Some(TransactionOutpoint::new(first.tx.id(), 1)),
        None,
        0x66,
    );
}
