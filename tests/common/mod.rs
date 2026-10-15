//! Shared helpers of the checks: the chain context, running a case and the expectations.

#![allow(dead_code)]

use kcc20_family::{Family, Scenario};
use node_gate::GateParams;
use node_gate::verdict::Stage;
use token_vesting::contract::{Compiled, VestingState};
use token_vesting::judge::{Judged, judge, write_record};
use token_vesting::release::{Release, initial_state};

/// A mutation run: record every answer, assert nothing (the runner compares the records).
pub fn mutant_mode() -> bool {
    std::env::var_os("VESTING_MUTANT").is_some()
}

/// The DAA score at which every schedule of the checks starts: the gate's "now" for
/// testnet-10, well after the covenant fork.
pub fn start() -> i64 {
    GateParams::testnet10().pov_daa_score as i64
}

/// The schedule of the checks.
pub fn schedule() -> VestingState {
    initial_state(start())
}

/// The node at the first DAA score at which a transaction with this lock time is final.
pub fn node_for(lock_time: u64) -> GateParams {
    GateParams::testnet10().with_daa_score(lock_time.saturating_add(1).max(1))
}

/// Lock time `offset` DAA units after the start of the schedule.
pub fn at(offset: i64) -> u64 {
    (start() + offset) as u64
}

/// Runs a release as described, judged at the first DAA score where it is final.
pub fn run(release: &Release) -> Judged {
    run_scenario(release, release.scenario(), node_for(release.lock_time))
}

/// Runs a changed scenario of a release, at a given node.
pub fn run_scenario(release: &Release, scenario: Scenario, node: GateParams) -> Judged {
    let assembled = scenario
        .assemble(Family::pinned())
        .expect("the scenario assembles");
    judge(&node, assembled, &[(0, release.compiled())])
}

/// Runs a scenario with explicit vesting inputs.
pub fn run_with(scenario: Scenario, node: GateParams, vesting: &[(usize, Compiled)]) -> Judged {
    let assembled = scenario
        .assemble(Family::pinned())
        .expect("the scenario assembles");
    judge(&node, assembled, vesting)
}

/// Prints the answer of every input and the refusals.
pub fn show(judged: &Judged) {
    println!("case: {}", judged.name);
    for input in &judged.inputs {
        println!(
            "  input {}: accepted={} budget={} units={} place={:?} error={:?}",
            input.index,
            input.accepted,
            input.budget,
            input.used_script_units,
            input.place,
            input.error
        );
    }
    println!("  submit: {:?}", judged.submit_refusal());
    println!("  block: {:?}", judged.block_refusal());
}

/// The case is accepted as a submission and inside a block, every input passing.
pub fn expect_accepted(file: &str, judged: &Judged) {
    show(judged);
    let ok = judged.accepted() && judged.inputs.iter().all(|input| input.accepted);
    write_record(
        file,
        &judged.record(
            "accepted",
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    if mutant_mode() {
        return;
    }
    assert!(ok, "the honest case `{}` was refused", judged.name);
}

/// The case is refused as a submission and inside a block, and the vesting input (input 0)
/// stops at the statement containing `statement`.
pub fn expect_vesting_refuses(file: &str, judged: &Judged, statement: &str) {
    show(judged);
    let line = judged.vesting_text().unwrap_or("");
    let ok = judged.refused()
        && judged.block_stage() == Some(Stage::Scripts)
        && line.contains(statement);
    let expectation = format!("refused; vesting input stops at `{statement}`");
    write_record(
        file,
        &judged.record(
            &expectation,
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    if mutant_mode() {
        return;
    }
    assert!(
        ok,
        "case `{}`: expected the vesting input to stop at `{statement}`, got {:?}; submit {:?}; block {:?}",
        judged.name,
        judged.inputs.first().map(|input| &input.place),
        judged.submit_refusal(),
        judged.block_refusal()
    );
}

/// The case is refused, and input `index` (a KCC20 coin) stops at a place whose description
/// contains `place`.
pub fn expect_token_refuses(file: &str, judged: &Judged, index: usize, place: &str) {
    show(judged);
    let found = judged.token_place(index).unwrap_or("");
    let ok = judged.refused() && found.contains(place);
    let expectation = format!("refused; token input {index} stops at `{place}`");
    write_record(
        file,
        &judged.record(
            &expectation,
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    if mutant_mode() {
        return;
    }
    assert!(
        ok,
        "case `{}`: expected token input {index} to stop at `{place}`, got {:?}",
        judged.name,
        judged.inputs.get(index).map(|input| &input.place)
    );
}

/// The case is refused by a consensus rule at `stage` (block mode), its message containing
/// `rule`.
pub fn expect_consensus_refuses(file: &str, judged: &Judged, stage: Stage, rule: &str) {
    show(judged);
    let refusal = judged.block_refusal().unwrap_or_default();
    let ok = judged.refused() && judged.block_stage() == Some(stage) && refusal.contains(rule);
    let expectation = format!("refused by consensus at {stage:?}, `{rule}`");
    write_record(
        file,
        &judged.record(
            &expectation,
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    if mutant_mode() {
        return;
    }
    assert!(
        ok,
        "case `{}`: expected a consensus refusal at {stage:?} with `{rule}`, got {:?} / {:?}",
        judged.name,
        judged.block_stage(),
        judged.block_refusal()
    );
}
