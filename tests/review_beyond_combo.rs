//! The release of tokens beyond the schedule ("from the end of the schedule on, the
//! beneficiary may take a token coin of the covenant whole") combined with a second token
//! coin, with coins open to borrowing, and with a relayer who rewrites the unsigned call.
//!
//! - `s2_d1`: two token coins of the covenant taken together after the end.
//! - `s2_d2` to `s2_d4`: a coin open to borrowing (scheme 0x01, threshold 0) on the covenant
//!   after the end: taken whole by the beneficiary (accepted, not a hole), its call switched
//!   to the borrow path inside a release, and a stranger taking tokens out of it on the
//!   borrow path without the vesting coin.
//! - `s2_d5`: after the end the amount rule no longer runs; a relayer who rewrites the claim
//!   of a signed release is stopped by the output and count rules instead.

mod common;

use common::{
    at, expect_accepted, expect_token_refuses, expect_vesting_refuses, node_for, run, run_scenario,
    run_with, schedule,
};
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::CovenantBinding;
use kcc20_family::raw::{push_number, split_pushes};
use kcc20_family::scenario::{Call, InputSpec};
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::states::PATH_BORROW;
use kcc20_family::{
    Arg, ContractInput, ContractOutput, Entry, EntryRole, Kcc20State, LateBytes, PlainOutput,
    Scenario, keys, witness,
};
use token_vesting::judge::Judged;
use token_vesting::release::{
    COIN_VALUE, DEFAULT_FEE, DURATION, FUNDING_VALUE, Release, STRANGER, TOTAL, funding_input,
    token_covid, vault_state,
};

const FILE: &str = "review_beyond_combo";

const ONE_TOKEN: &str = "require(OpCovInputCount(token_covid) == 1);";
const VAULT_HOLDS: &str = "require(vault.amount >= claim);";
const TWO_TOKEN_OUTPUTS: &str = "require(OpCovOutputCount(token_covid) == 2);";
const ONE_TOKEN_OUTPUT: &str = "require(OpCovOutputCount(token_covid) == 1);";
const RECIPIENT: &str = "validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0),";

// ---------------------------------------------------------------------------------------
// Two token coins.

/// After the end, the main vault (leader) and a later coin of 7 owned by the covenant
/// (delegate, empty witness) taken together, total + 7 to the beneficiary.
#[test]
fn s2_d1_after_the_end_two_token_coins_of_the_covenant_taken_together() {
    let release = Release::honest(
        "s2-d1: after the end, the vault and a later coin of 7 taken together",
        schedule(),
        TOTAL,
        TOTAL + 7,
        at(DURATION),
    );
    let mut scenario = release.scenario();
    scenario.push_contract_input(ContractInput::new(
        vault_state(7),
        COIN_VALUE,
        Some(token_covid()),
        outpoint(0x61, 0),
        Call::typed(
            Entry::TransferDelegator,
            vec![Arg::Bytes(LateBytes::empty())],
        ),
    ));
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, ONE_TOKEN);
}

// ---------------------------------------------------------------------------------------
// Coins open to borrowing.

/// After the end, released 150,000: a later coin of 7, open to borrowing by anybody, taken
/// whole; the beneficiary's coin is closed to borrowing and the vesting coin kept.
#[test]
fn s2_d2_after_the_end_a_later_coin_open_to_borrowing_taken_whole() {
    let mut release = Release::honest(
        "s2-d2: after the end, a later coin of 7 open to borrowing taken whole, the coin kept",
        schedule().with_released(150_000),
        7,
        7,
        at(DURATION + 100),
    )
    .with_other_coins_on_the_covenant();
    release.vault = release.vault.clone().with_threshold(0);
    release.vault_outpoint = outpoint(0x61, 0);
    expect_accepted(FILE, &run(&release));
}

/// After the end, an over-funded vault open to borrowing, the whole release, with the
/// token's (unsigned) call switched to the borrow path.
#[test]
fn s2_d3_after_the_end_the_token_call_switched_to_the_borrow_path() {
    let mut release = Release::honest(
        "s2-d3: after the end, a vault open to borrowing taken whole, its call on the borrow path",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    release.vault = release.vault.clone().with_threshold(0);
    let mut scenario = release.scenario();
    *scenario.contract_input_mut(1).witness_mut() = LateBytes::fixed(vec![PATH_BORROW]);
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_token_refuses(FILE, &judged, 1, "next_states[0].owner == owner");
}

/// After the end, without the vesting coin: a stranger leads a coin of the covenant open to
/// borrowing on the borrow path, the stranger's own coin of 100 as delegate, and takes 6 tokens out of
/// it (the covenant's coin shrinks to total - 1, the stranger's grows to 106).
#[test]
fn s2_d4_a_stranger_takes_tokens_out_of_a_borrow_open_coin_after_the_end() {
    let open = vault_state(TOTAL + 5).with_threshold(0);
    let next = vec![
        open.clone().with_amount(TOTAL - 1),
        Kcc20State::new(keys::x_only(STRANGER), 106),
    ];
    let mut scenario = Scenario::new(
        "s2-d4: after the end, a stranger takes 6 tokens out of a covenant coin open to borrowing",
    );
    scenario.lock_time = at(DURATION + 100);
    scenario.push_contract_input(ContractInput::new(
        open,
        COIN_VALUE,
        Some(token_covid()),
        outpoint(0x32, 0),
        Call::typed(
            Entry::Transfer,
            vec![
                Arg::States(next.clone()),
                Arg::Bytes(witness::borrow_threshold()),
            ],
        ),
    ));
    scenario.push_contract_input(ContractInput::new(
        Kcc20State::new(keys::x_only(STRANGER), 100),
        COIN_VALUE,
        Some(token_covid()),
        outpoint(0xb1, 0),
        Call::typed(
            Entry::TransferDelegator,
            vec![Arg::Bytes(witness::owner_key(
                STRANGER,
                EntryRole::Delegate,
                SIG_HASH_ALL,
            ))],
        ),
    ));
    scenario.push_ordinary_input(funding_input(outpoint(0xb2, 0)));
    for state in next {
        scenario.push_contract_output(ContractOutput::new(
            state,
            COIN_VALUE,
            Some(CovenantBinding::new(0, token_covid())),
        ));
    }
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        FUNDING_VALUE - DEFAULT_FEE,
    ));
    let judged = run_with(scenario, node_for(at(DURATION + 100)), &[]);
    expect_token_refuses(FILE, &judged, 0, "amount_increase > amount_threshold");
}

// ---------------------------------------------------------------------------------------
// A relayer rewrites the claim after the end.

/// The release with the claim push of input 0 replaced by `claim` after signing.
fn relayed(release: &Release, name: &str, claim: i64) -> Judged {
    let honest = release.claim;
    let mut scenario = release.scenario();
    scenario.name = name.to_string();
    let InputSpec::Ordinary(input) = &mut scenario.inputs[0] else {
        panic!("input 0 is the vesting coin");
    };
    let original = std::mem::replace(&mut input.signature_script, LateBytes::empty());
    input.signature_script = original.map(move |script| {
        let mut parts = split_pushes(&script).expect("a push-only script");
        assert_eq!(parts.len(), 4, "signature, claim, tag, redeem script");
        assert_eq!(parts[1], push_number(honest).expect("a number"));
        parts[1] = push_number(claim).expect("a number");
        parts.concat()
    });
    run_scenario(release, scenario, node_for(release.lock_time))
}

/// After the end, a signed partial release (500,000 of an over-funded vault, a rest of
/// 500,005 and the coin continuing) and the whole release (total + 5), each with its claim
/// rewritten by a relayer.
#[test]
fn s2_d5_after_the_end_a_relayer_rewrites_the_claim() {
    let partial = Release::honest(
        "s2-d5: after the end, 500,000 of total + 5, as signed",
        schedule(),
        TOTAL + 5,
        500_000,
        at(DURATION),
    );
    expect_accepted(FILE, &run(&partial));
    let whole = Release::honest(
        "s2-d5: after the end, the whole total + 5, as signed",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    expect_accepted(FILE, &run(&whole));
    for (release, claim, statement, name) in [
        (
            &partial,
            500_001,
            RECIPIENT,
            "s2-d5: partial release, claim raised by one",
        ),
        (
            &partial,
            499_999,
            RECIPIENT,
            "s2-d5: partial release, claim lowered by one",
        ),
        (
            &partial,
            TOTAL + 5,
            ONE_TOKEN_OUTPUT,
            "s2-d5: partial release, claim raised to the whole vault",
        ),
        (
            &whole,
            TOTAL + 6,
            VAULT_HOLDS,
            "s2-d5: whole release, claim raised by one",
        ),
        (
            &whole,
            TOTAL + 4,
            TWO_TOKEN_OUTPUTS,
            "s2-d5: whole release, claim lowered by one",
        ),
    ] {
        let judged = relayed(release, name, claim);
        expect_vesting_refuses(FILE, &judged, statement);
    }
}
