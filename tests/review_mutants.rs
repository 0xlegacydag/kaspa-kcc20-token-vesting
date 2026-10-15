//! Weakened rules that the cases written for them catch only by the place of a refusal:
//! another rule refuses the same moves. For each rule below there IS a transaction that the
//! weakened contract accepts and the real one refuses, so the weakening is not equivalent.
//! The same moves are forbidden moves of `tests/forbidden.rs`, group 13, so the mutation run
//! catches these rules by the verdict as well.
//!
//! Each check runs the same transaction twice: against the contract as written (refused at
//! the rule) and against the contract with that one rule replaced by `require(true);`, as
//! `tools/mutation-run.mjs` weakens it (accepted). Both halves must hold for a check to
//! pass.

mod common;

use common::{at, expect_accepted, expect_vesting_refuses, run, schedule};
use kcc20_family::Kcc20State;
use kcc20_family::keys;
use kcc20_family::states::OWNER_COVENANT_ID;
use token_vesting::release::{CLIFF, COIN_VALUE, Release, STRANGER, TOKEN_COVID, TOTAL};

const FILE: &str = "review_mutants";

/// The contract with the rule `rule` (a whole statement, as in the source) replaced by
/// `require(true);`.
fn weakened(source: &str, rule: &str) -> String {
    assert_eq!(
        source.matches(rule).count(),
        1,
        "the rule `{rule}` stands once in the source"
    );
    source.replace(rule, "require(true);")
}

/// The real contract refuses `release` at `rule`; the contract without `rule` accepts it.
fn tells_apart(release: Release, rule: &str) {
    expect_vesting_refuses(FILE, &run(&release), rule);
    let mut weak = release.clone();
    weak.name = format!("{} (contract without `{rule}`)", release.name);
    weak.source = weakened(&release.source, rule);
    expect_accepted(FILE, &run(&weak));
}

/// R04. A coin whose schedule starts at DAA score 4.5e11: above `MAX_DAA` (4e11), still in
/// the DAA domain of lock times (below 5e11). Without the rule nothing overflows and the
/// release goes through; with it the coin is dead.
#[test]
fn s_m1_r04_start_above_max_daa_but_inside_the_daa_domain() {
    let mut state = schedule();
    state.start = 450_000_000_000;
    let lock_time = (state.start + CLIFF + 500) as u64;
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 150_000);
    tells_apart(
        Release::honest(
            "s-m1 R04: a coin made with start 4.5e11, the honest first release",
            state,
            TOTAL,
            claim,
            lock_time,
        ),
        "require(start <= MAX_DAA);",
    );
}

/// R06. A coin with cliff 4.1e11: above `MAX_DAA`, start + cliff still below 5e11.
#[test]
fn s_m2_r06_cliff_above_max_daa_but_inside_the_daa_domain() {
    let mut state = schedule();
    state.cliff = 410_000_000_000;
    assert!(state.start + state.cliff < 500_000_000_000);
    let lock_time = (state.start + state.cliff + 500) as u64;
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, TOTAL);
    tells_apart(
        Release::honest(
            "s-m2 R06: a coin made with cliff 4.1e11, everything released after the cliff",
            state,
            TOTAL,
            claim,
            lock_time,
        ),
        "require(cliff <= MAX_DAA);",
    );
}

/// R08. A coin with duration 4e9 (above `MAX_DURATION`, 3e9) and a total that the duration
/// divides: no product of the formula comes near 2^63.
#[test]
fn s_m3_r08_duration_above_the_bound_without_any_overflow() {
    let mut state = schedule();
    state.duration = 4_000_000_000;
    state.total = 1_000_000_000_000;
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 375_000);
    tells_apart(
        Release::honest(
            "s-m3 R08: a coin made with duration 4e9 and total 1e12, the honest first release",
            state.clone(),
            state.total,
            claim,
            lock_time,
        ),
        "require(duration <= MAX_DURATION);",
    );
}

/// R19. The vault is a coin of the token family owned by the token family's own covenant id
/// (owner scheme 0x04, owner = TOKEN_COVID): the token program sees its owner present (the
/// coin itself), so only `vault.owner == own_covid` refuses. Such a coin moves for anybody
/// anyway: an owner of scheme 0x04 is present when an input carries its id, the coin's own
/// input included (vector `covenant-self` of KCC-2, kaspanet/kccs 3fbec52,
/// `kcc-0002/vectors/authority-schemes.json`). Here it shows that the rule is not backed by
/// the token program in every case.
#[test]
fn s_m4_r19_a_self_owned_token_coin_in_place_of_the_vault() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(
        "s-m4 R19: the vault is a token coin owned by the token's own covenant id",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    release.vault = release
        .vault
        .clone()
        .with_owner(OWNER_COVENANT_ID, TOKEN_COVID);
    tells_apart(release, "require(vault.owner == own_covid);");
}

/// R22. The honest first release with a third token output: an empty token coin for a
/// stranger. The token program allows it (three outputs at most, amounts add up); without
/// `OpCovOutputCount(token_covid) == 2` the contract checks only the first two.
#[test]
fn s_m5_r22_an_extra_empty_token_coin_beside_claim_and_rest() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(
        "s-m5 R22: claim, rest and an extra empty token coin for a stranger",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    release
        .token_outputs
        .push(Kcc20State::new(keys::x_only(STRANGER), 0));
    release.token_output_values.push(COIN_VALUE);
    tells_apart(release, "require(OpCovOutputCount(token_covid) == 2);");
}
