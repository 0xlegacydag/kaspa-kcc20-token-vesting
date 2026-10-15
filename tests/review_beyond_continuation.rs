//! The release of tokens beyond the schedule ("from the end of the schedule on, the
//! beneficiary may take a token coin of the covenant whole"): the continuation choice of
//! step 8 and the accounting of `released`.
//!
//! - `s2_c1`, `s2_c2`: before the end the vesting coin must continue even when the token coin
//!   is emptied (the `new_released < total` term of step 8, which before the end always
//!   holds).
//! - `s2_c3`: after the end the beneficiary ends the vesting coin with `released` below
//!   total. Whatever else the covenant holds would then be locked for good, so step 8 refuses
//!   it, and so does a step 8 that counts `released` by addition.
//! - `s2_c4` to `s2_c6`: a kept coin after the end with changed fields or another program.
//! - `s2_c7`: after the end a later coin can bring `released` to total while the main vault
//!   still holds tokens. The helper keeps the vesting coin when told that the covenant owns
//!   another coin, and the kept coin then releases the vault; ended instead, which the
//!   contract allows, it would leave the vault locked for good.

mod common;

use common::{
    at, expect_accepted, expect_token_refuses, expect_vesting_refuses, node_for, run, run_with,
    schedule,
};
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutpoint};
use kaspa_hashes::Hash;
use kcc20_family::scenario::Call;
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::{
    Arg, ContractInput, ContractOutput, Entry, Kcc20State, LateBytes, PlainOutput, Scenario, keys,
};
use token_vesting::contract::SOURCE;
use token_vesting::release::{
    BENEFICIARY, CLIFF, COIN_VALUE, DEFAULT_FEE, DURATION, FUNDING_VALUE, Release, STRANGER,
    TOKEN_COVID, TOTAL, funding_input, vault_state,
};

const FILE: &str = "review_beyond_continuation";

const SIG: &str = "require(checkSig(beneficiary_sig, pubkey(beneficiary)));";
const ONE_NEXT: &str = "require(OpCovOutputCount(own_covid) == 1);";
const NEXT_STATE: &str = "validateOutputState( next_output,";

/// The contract with another step 8: released counted with an addition, the coin
/// continuing while `released + claim < total` or a rest stays, ended otherwise. The rest of
/// the contract is unchanged.
fn step_8_counting_by_addition() -> String {
    let written = "        int new_released = total;\n        if (claim < total - released) {\n            new_released = released + claim;\n        }\n        if (remaining > 0 || new_released < total || OpCovOutputCount(own_covid) > 0) {\n";
    let by_addition = "        int new_released = released + claim;\n        if (new_released < total || remaining > 0) {\n";
    let written_else =
        "        } else {\n            require(OpAuthOutputCount(this.activeInputIndex) == 0);\n";
    let by_addition_else = "        } else {\n            require(OpCovOutputCount(own_covid) == 0);\n            require(OpAuthOutputCount(this.activeInputIndex) == 0);\n";
    assert!(
        SOURCE.contains(written) && SOURCE.contains(written_else),
        "step 8 as expected"
    );
    SOURCE
        .replace(written, by_addition)
        .replace(written_else, by_addition_else)
}

/// The release spends a later coin of the family owned by the covenant (another outpoint)
/// in place of the main vault, which stays with the covenant.
fn later_coin(release: Release) -> Release {
    let mut release = release.with_other_coins_on_the_covenant();
    release.vault_outpoint = outpoint(0x61, 0);
    release
}

// ---------------------------------------------------------------------------------------
// Before the end the coin continues.

/// 500 units after the cliff (150,000 vested, nothing released), a later coin of 7 tokens
/// taken whole and the vesting coin ended.
#[test]
fn s2_c1_before_the_end_a_later_coin_taken_whole_and_the_vesting_coin_ended() {
    let mut release = later_coin(Release::honest(
        "s2-c1: before the end, a later coin of 7 taken whole, the vesting coin ended",
        schedule(),
        7,
        7,
        at(CLIFF + 500),
    ));
    assert!(release.next_vesting.is_some());
    release.next_vesting = None;
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

/// An under-funded vault (exactly the 150,000 vested at 1,500) emptied and the vesting coin
/// ended.
#[test]
fn s2_c2_before_the_end_an_under_funded_vault_emptied_and_the_vesting_coin_ended() {
    let mut release = Release::honest(
        "s2-c2: before the end, a vault of 150,000 emptied at 1,500, the vesting coin ended",
        schedule(),
        150_000,
        150_000,
        at(CLIFF + 500),
    );
    release.next_vesting = None;
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

// ---------------------------------------------------------------------------------------
// After the end the beneficiary may end the coin, released below total.

fn ended_with_released_below_total(name: &str) -> Release {
    let state = schedule().with_released(150_000);
    let mut release = later_coin(Release::honest(name, state, 7, 7, at(DURATION + 100)));
    assert_eq!(
        release.next_vesting.as_ref().map(|next| next.released),
        Some(150_007),
        "the helper would keep the coin"
    );
    release.next_vesting = None;
    release
}

/// Released 150,000, a later coin of 7 taken whole after the end and the vesting coin ended:
/// whatever else the covenant holds (in the checks' story the main vault, 850,000 tokens)
/// could no longer move. Refused: step 8 keeps the coin while released is below total.
#[test]
fn s2_c3a_after_the_end_released_below_total_coin_ended() {
    let release = ended_with_released_below_total(
        "s2-c3a: released 150,000, after the end a later coin of 7 taken whole, the vesting coin ended",
    );
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

/// The same transaction against the contract with a step 8 that counts released by
/// addition: refused as well, the coin has to continue while released stays below total.
#[test]
fn s2_c3b_the_same_against_a_step_8_counting_by_addition() {
    let mut release = ended_with_released_below_total(
        "s2-c3b: the same against a step 8 that counts released by addition",
    );
    release.source = step_8_counting_by_addition();
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

// ---------------------------------------------------------------------------------------
// A kept coin after the end.

fn kept_after_a_later_coin(name: &str) -> Release {
    let state = schedule().with_released(150_000);
    later_coin(Release::honest(name, state, 7, 7, at(DURATION + 100)))
}

#[test]
fn s2_c4_after_the_end_kept_coin_names_another_beneficiary() {
    let mut release = kept_after_a_later_coin(
        "s2-c4: after the end, the kept coin names a stranger as beneficiary",
    );
    release.next_vesting.as_mut().unwrap().beneficiary = keys::x_only(STRANGER);
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

#[test]
fn s2_c5_after_the_end_kept_coin_released_not_advanced() {
    let mut release = kept_after_a_later_coin(
        "s2-c5: after the end, the kept coin keeps released 150,000 after taking 7",
    );
    release.next_vesting.as_mut().unwrap().released = 150_000;
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

/// The kept coin runs the contract without the signature rule: had it been allowed, any
/// coin of the family sent to the covenant later could be taken by anybody, to any key.
#[test]
fn s2_c6_after_the_end_kept_coin_runs_a_program_without_the_signature_rule() {
    let mut release = kept_after_a_later_coin(
        "s2-c6: after the end, the kept coin runs the contract without the signature rule",
    );
    release.next_source = Some(SOURCE.replace(SIG, "require(true);"));
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

// ---------------------------------------------------------------------------------------
// The helper's choice after the end.

/// After the end, nothing released yet, a later coin of `total` tokens is taken first, while
/// the main vault of 1,000,000 stays with the covenant. Released reaches total; the helper,
/// told that the covenant owns another coin, keeps the vesting coin (1/4), and the kept coin
/// then releases the main vault whole and ends (2/4). Had the beneficiary ended the coin in
/// the first release, the contract would accept that (3/4), and the main vault could then
/// never move (4/4): the choice to end belongs to the beneficiary's wallet.
#[test]
fn s2_c7_a_later_coin_brings_released_to_total_while_the_vault_holds_tokens() {
    let first = later_coin(Release::honest(
        "s2-c7 1/4: after the end, a later coin of total taken whole first, the vesting coin kept",
        schedule(),
        TOTAL,
        TOTAL,
        at(DURATION + 100),
    ));
    assert_eq!(
        first.next_vesting.as_ref().map(|next| next.released),
        Some(TOTAL),
        "the helper keeps the coin while the covenant owns another coin"
    );
    let judged = run(&first);
    expect_accepted(FILE, &judged);

    // The kept coin (output 1: the claim, the kept coin, the change) releases the main vault.
    let mut second = Release::honest(
        "s2-c7 2/4: the main vault taken whole with the kept coin, which then ends",
        schedule().with_released(TOTAL),
        TOTAL,
        TOTAL,
        at(DURATION + 200),
    );
    assert!(
        second.next_vesting.is_none(),
        "nothing else on the covenant"
    );
    second.vesting_outpoint = TransactionOutpoint::new(judged.assembled.tx.id(), 1);
    second.funding_outpoint = outpoint(0x68, 0);
    expect_accepted(FILE, &run(&second));

    let mut ended = first.clone();
    ended.name =
        "s2-c7 3/4: after the end, a later coin of total taken whole first, the vesting coin ended"
            .into();
    ended.next_vesting = None;
    expect_accepted(FILE, &run(&ended));

    // The main vault, owned by the covenant, without any coin of the covenant beside it.
    let mut scenario =
        Scenario::new("s2-c7 4/4: the main vault cannot move once the vesting coin is gone");
    scenario.lock_time = at(DURATION + 200);
    let to_beneficiary = Kcc20State::new(keys::x_only(BENEFICIARY), TOTAL);
    scenario.push_contract_input(ContractInput::new(
        vault_state(TOTAL),
        COIN_VALUE,
        Some(Hash::from_bytes(TOKEN_COVID)),
        outpoint(0x32, 0),
        Call::typed(
            Entry::Transfer,
            vec![
                Arg::States(vec![to_beneficiary.clone()]),
                Arg::Bytes(LateBytes::fixed(vec![0x00])),
            ],
        ),
    ));
    scenario.push_ordinary_input(funding_input(outpoint(0x67, 0)));
    scenario.push_contract_output(ContractOutput::new(
        to_beneficiary,
        COIN_VALUE,
        Some(CovenantBinding::new(0, Hash::from_bytes(TOKEN_COVID))),
    ));
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        FUNDING_VALUE - DEFAULT_FEE,
    ));
    let judged = run_with(scenario, node_for(at(DURATION + 200)), &[]);
    expect_token_refuses(FILE, &judged, 0, "OpCovInputCount(gen__glob_owner) > 0");
}

/// A kept coin after the end (released = total) with nothing left on the covenant, spent
/// alone to end it and take back its KAS: refused, a release needs exactly one token coin
/// of the family. Not a theft: the coin's KAS waits until a coin of the family is sent to
/// the covenant (the beneficiary can send one token and take it back with the coin).
#[test]
fn s2_c8_after_the_end_a_kept_coin_cannot_end_without_a_token_coin() {
    let release = Release::honest(
        "s2-c8: after the end, a kept coin (released = total) ended alone, no token coin",
        schedule().with_released(TOTAL),
        1,
        1,
        at(DURATION + 100),
    );
    assert!(release.next_vesting.is_none());
    let mut scenario = release.scenario();
    scenario.inputs.remove(1);
    scenario
        .outputs
        .retain(|output| !matches!(output, kcc20_family::scenario::OutputSpec::Contract(_)));
    let judged = common::run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, "require(OpCovInputCount(token_covid) == 1);");
}
