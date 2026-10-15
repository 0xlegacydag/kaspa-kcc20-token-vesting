//! The release of tokens beyond the schedule ("from the end of the schedule on, the
//! beneficiary may take a token coin of the covenant whole"): attempts to reach that branch
//! early, and claims it might let through in larger amounts than the token coin holds.
//!
//! - `s2_t*`: time. The branch trusts `now = OpTxLockTime()`. The lock time is tied to
//!   the chain only by the node's finality rule, which a transaction escapes when every
//!   input has a final sequence; step 3 (`tx.daa >= start + cliff`, compiled to
//!   `OpCheckLockTimeVerify`) is what refuses a final sequence on the vesting input.
//! - `s2_a*`: amounts after the end, at the top of the range.

mod common;

use common::{
    at, expect_accepted, expect_consensus_refuses, expect_vesting_refuses, node_for, run,
    run_scenario, schedule,
};
use kcc20_family::scenario::InputSpec;
use node_gate::GateParams;
use node_gate::verdict::Stage;
use token_vesting::contract::VestingState;
use token_vesting::release::{CLIFF, DURATION, Release, TOTAL};

const FILE: &str = "review_beyond_time";

const TIME: &str = "require(tx.daa >= start + cliff);";
const AMOUNT: &str = "require(claim <= vested - released);";
const VAULT_HOLDS: &str = "require(vault.amount >= claim);";
const NEXT_STATE: &str = "validateOutputState( next_output,";

/// The schedule of the checks with another cliff and duration.
fn shaped(cliff: i64, duration: i64) -> VestingState {
    let mut state = schedule();
    state.cliff = cliff;
    state.duration = duration;
    state
}

// ---------------------------------------------------------------------------------------
// Time.

/// Cliff 20,000 longer than duration 10,000: at 15,000 the schedule is past its end but
/// before its cliff. The whole over-funded vault is asked for there.
#[test]
fn s2_t1_cliff_longer_than_duration_whole_vault_past_the_end_before_the_cliff() {
    let release = Release::honest(
        "s2-t1: cliff 20,000 > duration 10,000, the whole over-funded vault at 15,000",
        shaped(20_000, DURATION),
        TOTAL + 5,
        TOTAL + 5,
        at(15_000),
    );
    expect_vesting_refuses(FILE, &run(&release), TIME);
}

/// The same coin at its cliff: everything is vested and the whole vault goes to the
/// beneficiary. Not early: nothing is released before the cliff.
#[test]
fn s2_t2_cliff_longer_than_duration_whole_vault_at_the_cliff() {
    let release = Release::honest(
        "s2-t2: cliff 20,000 > duration 10,000, the whole over-funded vault at the cliff",
        shaped(20_000, DURATION),
        TOTAL + 5,
        TOTAL + 5,
        at(20_000),
    );
    expect_accepted(FILE, &run(&release));
}

/// Duration 1 and cliff 0: at the start nothing is vested (elapsed 0 < 1), so even one
/// token is refused by the amount rule, not let through by the branch for the end.
#[test]
fn s2_t3_duration_one_cliff_zero_one_token_at_the_start() {
    let release = Release::honest(
        "s2-t3: duration 1, cliff 0, one token at the start",
        shaped(0, 1),
        TOTAL,
        1,
        at(0),
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

/// Duration 1: one DAA unit after the start the schedule is over and the whole vault goes.
#[test]
fn s2_t4_duration_one_whole_vault_one_unit_after_the_start() {
    let release = Release::honest(
        "s2-t4: duration 1, cliff 0, the whole over-funded vault one unit after the start",
        shaped(0, 1),
        TOTAL + 5,
        TOTAL + 5,
        at(1),
    );
    expect_accepted(FILE, &run(&release));
}

/// Lock time exactly at the end, judged by a node whose DAA score equals it: the node does
/// not take the transaction yet (lock time must be below the DAA score), though the scripts
/// would.
#[test]
fn s2_t5_lock_time_at_the_end_judged_at_the_same_daa_score() {
    let release = Release::honest(
        "s2-t5: the whole over-funded vault, lock time at the end, node at that DAA score",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    let node = GateParams::testnet10().with_daa_score(at(DURATION));
    let judged = run_scenario(&release, release.scenario(), node);
    expect_consensus_refuses(FILE, &judged, Stage::HeaderContext, "check_tx_is_finalized");
    assert!(
        judged.inputs.iter().all(|input| input.accepted),
        "the scripts alone accept it"
    );
}

/// The way around the node's finality rule: every input with a final sequence makes the
/// transaction final at any DAA score, whatever its lock time. Lock time at the end, node
/// 500 units after the cliff, the whole over-funded vault asked for. Only the
/// `OpCheckLockTimeVerify` of step 3 (input sequence must not be final) stands in the way.
#[test]
fn s2_t6_every_input_final_lock_time_at_the_end_judged_after_the_cliff() {
    let mut release = Release::honest(
        "s2-t6: every input final, lock time at the end, node at 1,500, the whole vault",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    release.vesting_sequence = u64::MAX;
    let mut scenario = release.scenario();
    for input in &mut scenario.inputs {
        match input {
            InputSpec::Ordinary(input) => input.sequence = u64::MAX,
            InputSpec::Contract(input) => input.sequence = u64::MAX,
        }
    }
    let judged = run_scenario(&release, scenario, node_for(at(CLIFF + 500)));
    // The node itself lets the transaction through: final sequences everywhere.
    assert!(
        judged
            .submit
            .verdict
            .first_refusal
            .as_ref()
            .map(|r| r.stage)
            != Some(Stage::HeaderContext),
        "the finality rule is bypassed by final sequences"
    );
    expect_vesting_refuses(FILE, &judged, TIME);
}

/// A schedule at the top of the DAA domain of lock times: start = MAX_DAA (4e11), cliff 0,
/// duration = MAX_DURATION (3e9), so the end is 4.03e11, below the threshold 5e11. One unit
/// before the end the whole vault is refused by the amount rule.
#[test]
fn s2_t7a_top_of_the_daa_domain_whole_vault_one_unit_before_the_end() {
    let mut state = shaped(0, 3_000_000_000);
    state.start = 400_000_000_000;
    let end = (state.start + state.duration) as u64;
    let release = Release::honest(
        "s2-t7a: start 4e11, duration 3e9, the whole vault one unit before the end",
        state,
        TOTAL + 5,
        TOTAL + 5,
        end - 1,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

/// The same coin at its end: the whole vault goes to the beneficiary.
#[test]
fn s2_t7b_top_of_the_daa_domain_whole_vault_at_the_end() {
    let mut state = shaped(0, 3_000_000_000);
    state.start = 400_000_000_000;
    let end = (state.start + state.duration) as u64;
    let release = Release::honest(
        "s2-t7b: start 4e11, duration 3e9, the whole vault at the end",
        state,
        TOTAL + 5,
        TOTAL + 5,
        end,
    );
    expect_accepted(FILE, &run(&release));
}

// ---------------------------------------------------------------------------------------
// Amounts after the end.

/// After the end the only limit is the token coin: one token more than it holds.
#[test]
fn s2_a1_after_the_end_one_token_more_than_the_vault() {
    let release = Release::honest(
        "s2-a1: after the end, a claim of total + 6 from a vault of total + 5",
        schedule(),
        TOTAL + 5,
        TOTAL + 6,
        at(DURATION),
    );
    assert_eq!(release.token_outputs.len(), 1);
    expect_vesting_refuses(FILE, &run(&release), VAULT_HOLDS);
}

/// After the end, a claim of 2^63 - 1 from a vault of total + 5.
#[test]
fn s2_a2_after_the_end_a_claim_at_the_top_of_the_range() {
    let release = Release::honest(
        "s2-a2: after the end, a claim of 2^63 - 1 from a vault of total + 5",
        schedule(),
        TOTAL + 5,
        i64::MAX,
        at(DURATION),
    );
    expect_vesting_refuses(FILE, &run(&release), VAULT_HOLDS);
}

/// After the end, a vault of 2^63 - 1 tokens taken whole: the coin ends.
#[test]
fn s2_a3a_after_the_end_a_vault_of_2_63_minus_1_taken_whole() {
    let release = Release::honest(
        "s2-a3a: after the end, a vault of 2^63 - 1 taken whole, the vesting coin ends",
        schedule(),
        i64::MAX,
        i64::MAX,
        at(DURATION),
    );
    assert!(release.next_vesting.is_none());
    expect_accepted(FILE, &run(&release));
}

/// released one below total, a vault of 2^63 - 1 taken whole after the end, the vesting coin
/// kept: released + claim would overflow, the contract stops released at total without
/// adding, and the kept coin is the one with released = total.
#[test]
fn s2_a3b_released_one_below_total_a_claim_of_2_63_minus_1_coin_kept() {
    let state = schedule().with_released(TOTAL - 1);
    let mut release = Release::honest(
        "s2-a3b: released total - 1, after the end a vault of 2^63 - 1 taken whole, the coin kept with released = total",
        state.clone(),
        i64::MAX,
        i64::MAX,
        at(DURATION),
    );
    release.next_vesting = Some(state.with_released(TOTAL));
    expect_accepted(FILE, &run(&release));
}

/// The same release with the kept coin's released left at total - 1: refused.
#[test]
fn s2_a3c_released_one_below_total_a_claim_of_2_63_minus_1_released_not_advanced() {
    let state = schedule().with_released(TOTAL - 1);
    let mut release = Release::honest(
        "s2-a3c: released total - 1, after the end a vault of 2^63 - 1 taken whole, the coin kept with released = total - 1",
        state.clone(),
        i64::MAX,
        i64::MAX,
        at(DURATION),
    );
    release.next_vesting = Some(state.with_released(TOTAL - 1));
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

/// After the end, total near the top of the range (9e18) and released one below it: a
/// partial claim of 2^63 - 1 - 9e18 with a rest, the coin continuing with released = total.
#[test]
fn s2_a4_large_total_released_near_it_partial_claim_after_the_end() {
    let mut state = schedule();
    state.total = 9_000_000_000_000_000_000;
    state.released = state.total - 1;
    let vault = i64::MAX;
    let claim = i64::MAX - state.total;
    let release = Release::honest(
        "s2-a4: total 9e18, released total - 1, after the end a claim of 2^63 - 1 - 9e18 with a rest",
        state.clone(),
        vault,
        claim,
        at(DURATION),
    );
    assert_eq!(
        release.next_vesting.as_ref().map(|next| next.released),
        Some(state.total)
    );
    expect_accepted(FILE, &run(&release));
}
