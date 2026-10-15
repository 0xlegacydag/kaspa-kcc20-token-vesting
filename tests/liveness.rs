//! Coins that end up locked for good: not a theft, but tokens or KAS nobody can move. Each
//! case says what the launch check (`token_vesting::launch`) does about it.

mod common;

use common::{at, expect_accepted, expect_vesting_refuses, run, schedule};
use kaspa_consensus_core::constants::LOCK_TIME_THRESHOLD;
use kaspa_consensus_core::tx::TransactionOutpoint;
use token_vesting::contract::{VestingState, int_constant};
use token_vesting::judge::write_record;
use token_vesting::launch::{check_parameters, check_vault};
use token_vesting::release::{DURATION, Release, TOTAL, token_coin, vault_state, vesting_covid};

const FILE: &str = "liveness";

/// Tokens beyond the schedule: from the end of the schedule on, the beneficiary may take the
/// vault coin whole, so the 5 extra tokens of an over-funded vault are not locked.
#[test]
fn l1_an_over_funded_vault_gives_its_extra_tokens_after_the_end() {
    // The vault holds 5 tokens more than the schedule: the schedule's tokens are released at
    // the end, the coin continues with released == total, and the 5 tokens follow.
    let state = schedule();
    let lock_time = at(DURATION);
    let last = Release::honest(
        "l1 1/2: over-funded vault, the schedule's tokens released at the end",
        state.clone(),
        TOTAL + 5,
        TOTAL,
        lock_time,
    );
    assert!(
        last.next_vesting.is_some(),
        "the coin continues: 5 tokens remain"
    );
    let judged = run(&last);
    expect_accepted(FILE, &judged);
    let mut after = Release::honest(
        "l1 2/2: the 5 extra tokens, after the schedule's tokens were released",
        state.with_released(TOTAL),
        5,
        5,
        at(DURATION + 100),
    );
    assert!(
        after.next_vesting.is_none(),
        "the coin ends: nothing is left"
    );
    after.vesting_outpoint = TransactionOutpoint::new(judged.assembled.tx.id(), 2);
    after.vault_outpoint = TransactionOutpoint::new(judged.assembled.tx.id(), 1);
    expect_accepted(FILE, &run(&after));
    // The launch check still refuses such a vault: it holds more than the schedule.
    assert!(
        check_vault(
            vesting_covid(),
            &schedule(),
            &token_coin(vault_state(TOTAL + 5))
        )
        .is_err()
    );
}

#[test]
fn l2_start_plus_cliff_outside_the_daa_domain_makes_a_dead_coin() {
    let mut state = schedule();
    state.start = 400_000_000_000;
    state.cliff = 100_000_000_000;
    state.duration = 1_000;
    let release = Release::honest(
        "l2: start 4e11 + cliff 1e11 = 5e11, the first timestamp lock time: every release is refused",
        state.clone(),
        TOTAL,
        1,
        499_999_999_999,
    );
    expect_vesting_refuses(FILE, &run(&release), "require(tx.daa >= start + cliff);");
    assert!(check_parameters(&state).is_err());
    assert!(check_parameters(&schedule()).is_ok());
}

/// `check_parameters` passes exactly the coins a release can open: one parameter at a time
/// just inside and just outside each bound of step 2 of the contract and of the lock-time
/// domain (step 3), each coin tried with the honest release of everything at the end of its
/// schedule. `released` is left at 0: a new coin with something released is refused by the
/// launch check on purpose, though the contract would open it.
#[test]
fn l3_check_parameters_passes_exactly_the_coins_a_release_can_open() {
    let max_daa = int_constant("MAX_DAA");
    let max_duration = int_constant("MAX_DURATION");
    let threshold = LOCK_TIME_THRESHOLD as i64;
    assert_eq!((max_daa, max_duration), (400_000_000_000, 3_000_000_000));
    let base = schedule();
    let with = |change: &dyn Fn(&mut VestingState)| {
        let mut state = base.clone();
        change(&mut state);
        state
    };
    let cases: Vec<(&str, VestingState)> = vec![
        ("the schedule of the checks", base.clone()),
        ("start -1", with(&|s| s.start = -1)),
        ("start 0", with(&|s| s.start = 0)),
        ("start MAX_DAA", with(&|s| s.start = max_daa)),
        ("start MAX_DAA + 1", with(&|s| s.start = max_daa + 1)),
        ("cliff -1", with(&|s| s.cliff = -1)),
        ("cliff 0", with(&|s| s.cliff = 0)),
        ("cliff MAX_DAA", with(&|s| s.cliff = max_daa)),
        ("cliff MAX_DAA + 1", with(&|s| s.cliff = max_daa + 1)),
        (
            "start MAX_DAA, start + cliff = 5e11 - 1",
            with(&|s| {
                s.start = max_daa;
                s.cliff = threshold - 1 - max_daa;
            }),
        ),
        (
            "start MAX_DAA, start + cliff = 5e11",
            with(&|s| {
                s.start = max_daa;
                s.cliff = threshold - max_daa;
            }),
        ),
        ("duration 0", with(&|s| s.duration = 0)),
        ("duration 1", with(&|s| s.duration = 1)),
        (
            "duration MAX_DURATION",
            with(&|s| s.duration = max_duration),
        ),
        (
            "duration MAX_DURATION + 1",
            with(&|s| s.duration = max_duration + 1),
        ),
        ("total 0", with(&|s| s.total = 0)),
        ("total 1", with(&|s| s.total = 1)),
        (
            "beneficiary not a key",
            with(&|s| s.beneficiary = [0xff; 32]),
        ),
    ];
    let mut differ = Vec::new();
    for (label, state) in cases {
        // The end of the schedule, or the first lock time after the cliff if later; never
        // before the gate's own DAA score (covenants are active there), never outside the
        // DAA-score domain.
        let end = state.start.saturating_add(state.cliff.max(state.duration));
        let lock_time = end.max(base.start).min(threshold - 1);
        let claim = state.vested_at(lock_time).max(1);
        let release = Release::honest(
            format!("l3: {label}: the release of everything at the end"),
            state.clone(),
            claim.max(state.total),
            claim,
            lock_time as u64,
        );
        let judged = run(&release);
        let opens = judged.accepted();
        let passes = check_parameters(&state).is_ok();
        println!(
            "{label}: check_parameters {}; the release {} ({:?})",
            if passes { "passes" } else { "refuses" },
            if opens { "is accepted" } else { "is refused" },
            judged.vesting_text()
        );
        if opens != passes {
            differ.push(label);
        }
    }
    write_record(
        FILE,
        &serde_json::json!({
            "case": "l3: check_parameters against the contract, one bound at a time",
            "expectation": "check_parameters passes a coin exactly when its release is accepted",
            "result": if differ.is_empty() { "as expected" } else { "NOT AS EXPECTED" },
            "differ": differ,
        }),
    );
    assert!(
        differ.is_empty(),
        "check_parameters and the contract differ: {differ:?}"
    );
}
