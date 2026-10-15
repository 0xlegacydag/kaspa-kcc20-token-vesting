//! Forbidden moves: each must be refused, and the refusal must come from the rule meant for
//! it. The number in a name is the group of the move (the headings below): time and amount
//! (1), the beneficiary's output (2), the rest of the vault (3), the next vesting state (4),
//! the token input (5), other coins of the covenant (6), signatures (7), borrowing (8),
//! positions and overflow (9), call arguments (10), lock time, sequences and the coins spent
//! (11), tokens beyond the schedule (12), and moves that tell five weakened rules apart by
//! the verdict alone (13).

mod common;

use common::{
    at, expect_consensus_refuses, expect_token_refuses, expect_vesting_refuses, node_for, run,
    run_scenario, run_with, schedule, start as start_of_schedule,
};
use kaspa_consensus_core::hashing::sighash_type::{
    SIG_HASH_ALL, SIG_HASH_NONE, SIG_HASH_SINGLE, SigHashType,
};
use kaspa_consensus_core::tx::CovenantBinding;
use kaspa_hashes::Hash;
use kcc20_family::scenario::{Call, InputSpec, OutputSpec};
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::states::{OWNER_COVENANT_ID, OWNER_P2PKH_SCHNORR, OWNER_P2SH, PATH_BORROW};
use kcc20_family::{
    Arg, ContractInput, ContractOutput, Entry, EntryRole, Family, Kcc20State, LateBytes,
    OrdinaryInput, PlainOutput, PublicMintState, keys, witness,
};
use node_gate::verdict::Stage;
use token_vesting::contract::SOURCE;
use token_vesting::release::{
    BENEFICIARY, CLIFF, COIN_VALUE, DURATION, OTHER_TOKEN_COVID, Release, STRANGER, Signer,
    TOKEN_COVID, TOTAL, VESTING_COVID, funding_input, vault_state, vesting_covid,
};

const FILE: &str = "forbidden";

// The statements of the contract that a refusal is expected at.
const SIG_TYPE: &str = "require(byte[](beneficiary_sig).slice(64, 65) == byte[](0x01));";
const SIG: &str = "require(checkSig(beneficiary_sig, pubkey(beneficiary)));";
const START_LOW: &str = "require(start >= 0);";
const START_BOUND: &str = "require(start <= MAX_DAA);";
const CLIFF_LOW: &str = "require(cliff >= 0);";
const CLIFF_BOUND: &str = "require(cliff <= MAX_DAA);";
const DURATION_POSITIVE: &str = "require(duration > 0);";
const TOTAL_POSITIVE: &str = "require(total > 0);";
const DURATION_BOUND: &str = "require(duration <= MAX_DURATION);";
const RELEASED_LOW: &str = "require(released >= 0);";
const RELEASED_HIGH: &str = "require(released <= total);";
const CLAIM_POSITIVE: &str = "require(claim > 0);";
const TIME: &str = "require(tx.daa >= start + cliff);";
const AMOUNT: &str = "require(claim <= vested - released);";
const ONE_OWN: &str = "require(OpCovInputCount(own_covid) == 1);";
const ONE_TOKEN: &str = "require(OpCovInputCount(token_covid) == 1);";
const MAX_INPUTS: &str = "require(tx.inputs.length <= MAX_INPUTS);";
const NO_OTHER_COVENANT: &str = "require(OpInputCovenantId(i) ==";
const READ_VAULT: &str = "KCC20State vault = readInputStateWithTemplate(";
const VAULT_OWNER: &str = "require(vault.owner == own_covid);";
const VAULT_SCHEME: &str = "require(vault.owner_scheme == OWNER_COVENANT_ID);";
const TWO_TOKEN_OUTPUTS: &str = "require(OpCovOutputCount(token_covid) == 2);";
const ONE_TOKEN_OUTPUT: &str = "require(OpCovOutputCount(token_covid) == 1);";
const RECIPIENT: &str = "validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0),";
const CHANGE: &str = "validateOutputStateWithInputTemplate( change_output,";
const CHANGE_KAS: &str =
    "require(tx.outputs[change_output].value >= tx.inputs[token_input].value);";
const ONE_NEXT: &str = "require(OpCovOutputCount(own_covid) == 1);";
const NEXT_STATE: &str = "validateOutputState( next_output,";
const NEXT_KAS: &str =
    "require(tx.outputs[next_output].value >= tx.inputs[this.activeInputIndex].value);";

/// The honest first release: 150,000 of 1,000,000 tokens, 500 DAA units after the cliff.
fn first() -> Release {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    Release::honest("first release", state, TOTAL, claim, lock_time)
}

/// The honest second release: 100,000 already released, 200,000 more at 3,000.
fn later() -> Release {
    let state = schedule().with_released(100_000);
    let lock_time = at(3_000);
    let claim = state.vested_at(lock_time as i64) - state.released;
    Release::honest("later release", state, TOTAL - 100_000, claim, lock_time)
}

fn named(mut release: Release, name: &str) -> Release {
    release.name = name.to_string();
    release
}

/// A token coin input of a family, on the normal path with an empty witness (owner scheme
/// 0x04), and its output bound to it.
fn token_input(
    state: Kcc20State,
    covid: [u8; 32],
    seed: u8,
    next: Vec<Kcc20State>,
    entry: Entry,
) -> ContractInput {
    let args = match entry {
        Entry::Transfer => vec![Arg::States(next), Arg::Bytes(LateBytes::fixed(vec![0x00]))],
        _ => vec![Arg::Bytes(LateBytes::empty())],
    };
    ContractInput::new(
        state,
        COIN_VALUE,
        Some(Hash::from_bytes(covid)),
        outpoint(seed, 0),
        Call::typed(entry, args),
    )
}

// ---------------------------------------------------------------------------------------
// 1. Time and amount.

#[test]
fn f1a_release_one_unit_before_the_cliff() {
    let state = schedule();
    let lock_time = at(CLIFF - 1);
    let release = Release::honest(
        "1a: one DAA unit before the cliff, 1 token",
        state,
        TOTAL,
        1,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), TIME);
}

#[test]
fn f1b_one_token_more_than_vested() {
    let mut release = named(first(), "1b: one token more than vested (150,001 at 1,500)");
    let claim = release.claim + 1;
    release = Release::honest(
        release.name,
        release.vesting,
        TOTAL,
        claim,
        release.lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f1c_one_token_more_than_vested_after_an_earlier_release() {
    let base = later();
    let release = Release::honest(
        "1c: after 100,000 released, one token more than the 200,000 due",
        base.vesting,
        TOTAL - 100_000,
        base.claim + 1,
        base.lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f1d_the_released_amount_is_not_vested_twice() {
    // 100,000 released at the cliff; asking for the cliff's 100,000 again at the cliff.
    let state = schedule().with_released(100_000);
    let release = Release::honest(
        "1d: at the cliff, after the cliff's tokens were released, 1 more token",
        state,
        TOTAL - 100_000,
        1,
        at(CLIFF),
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f1e_claim_zero() {
    let base = first();
    let release = Release::honest("1e: a claim of 0", base.vesting, TOTAL, 0, base.lock_time);
    expect_vesting_refuses(FILE, &run(&release), CLAIM_POSITIVE);
}

#[test]
fn f1f_negative_claim() {
    let base = first();
    let release = Release::honest("1f: a claim of -5", base.vesting, TOTAL, -5, base.lock_time);
    expect_vesting_refuses(FILE, &run(&release), CLAIM_POSITIVE);
}

// ---------------------------------------------------------------------------------------
// 2. The beneficiary's output.

#[test]
fn f2a_tokens_go_to_a_stranger() {
    let mut release = named(first(), "2a: the claim goes to a stranger's key");
    release.token_outputs[0] = Kcc20State::new(keys::x_only(STRANGER), release.claim);
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f2b_beneficiary_output_with_owner_scheme_0x01() {
    let mut release = named(
        first(),
        "2b: the claim's owner scheme is 0x01 (key hash) with the key's bytes",
    );
    release.token_outputs[0] = release.token_outputs[0]
        .clone()
        .with_owner(OWNER_P2PKH_SCHNORR, keys::x_only(BENEFICIARY));
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f2c_beneficiary_output_with_owner_scheme_0x04() {
    let mut release = named(
        first(),
        "2c: the claim's owner scheme is 0x04 with the key's bytes",
    );
    release.token_outputs[0] = release.token_outputs[0]
        .clone()
        .with_owner(OWNER_COVENANT_ID, keys::x_only(BENEFICIARY));
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f2d_beneficiary_output_with_owner_scheme_0x03() {
    let mut release = named(
        first(),
        "2d: the claim's owner scheme is 0x03 with the key's bytes",
    );
    release.token_outputs[0] = release.token_outputs[0]
        .clone()
        .with_owner(OWNER_P2SH, keys::x_only(BENEFICIARY));
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f2e_beneficiary_output_with_another_extension_commitment() {
    let mut release = named(
        first(),
        "2e: the claim carries another extension commitment",
    );
    release.token_outputs[0] = release.token_outputs[0].clone().with_extension([0x5a; 32]);
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

// ---------------------------------------------------------------------------------------
// 3. The rest of the vault.

#[test]
fn f3a_rest_goes_to_another_covenant() {
    let mut release = named(first(), "3a: the rest goes to another covenant id");
    release.token_outputs[1] = release.token_outputs[1]
        .clone()
        .with_owner(OWNER_COVENANT_ID, OTHER_TOKEN_COVID);
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f3b_rest_with_owner_scheme_0x03_and_our_bytes() {
    let mut release = named(
        first(),
        "3b: the rest has owner scheme 0x03 with the covenant id's bytes",
    );
    release.token_outputs[1] = release.token_outputs[1]
        .clone()
        .with_owner(OWNER_P2SH, VESTING_COVID);
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f3c_rest_goes_to_the_beneficiary_key() {
    let mut release = named(first(), "3c: the rest goes to the beneficiary's key");
    release.token_outputs[1] = Kcc20State::new(keys::x_only(BENEFICIARY), TOTAL - release.claim);
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f3d_rest_short_by_one_token() {
    let mut release = named(
        first(),
        "3d: the rest is one token short (the token's sum breaks too)",
    );
    release.token_outputs[1].amount -= 1;
    let judged = run(&release);
    expect_vesting_refuses(FILE, &judged, CHANGE);
    assert!(
        !judged.inputs[1].accepted,
        "the token's own sum rule refuses as well"
    );
}

#[test]
fn f3e_rest_loses_its_kas() {
    let mut release = named(
        first(),
        "3e: the rest's coin holds one sompi less KAS than the vault",
    );
    release.token_output_values[1] = COIN_VALUE - 1;
    expect_vesting_refuses(FILE, &run(&release), CHANGE_KAS);
}

#[test]
fn f3f_rest_split_into_two_coins() {
    let mut release = named(
        first(),
        "3f: the rest is split into two coins of the covenant",
    );
    let rest = release.token_outputs[1].amount;
    release.token_outputs[1].amount = rest - 1;
    release.token_outputs.push(vault_state(1));
    release.token_output_values.push(COIN_VALUE);
    expect_vesting_refuses(FILE, &run(&release), TWO_TOKEN_OUTPUTS);
}

#[test]
fn f3g_last_release_with_an_extra_empty_token_coin() {
    let state = schedule();
    let mut release = Release::honest(
        "3g: the last release adds a token coin of 0 tokens for a stranger",
        state,
        TOTAL,
        TOTAL,
        at(DURATION),
    );
    release
        .token_outputs
        .push(Kcc20State::new(keys::x_only(STRANGER), 0));
    release.token_output_values.push(COIN_VALUE);
    expect_vesting_refuses(FILE, &run(&release), ONE_TOKEN_OUTPUT);
}

// ---------------------------------------------------------------------------------------
// 4. Every field of the next vesting state.

fn next_field_case(name: &str, change: impl Fn(&mut token_vesting::contract::VestingState)) {
    let mut release = named(first(), name);
    let next = release.next_vesting.as_mut().expect("the coin continues");
    change(next);
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

#[test]
fn f4a_next_state_beneficiary_changed() {
    next_field_case("4a: next state, another beneficiary", |s| {
        s.beneficiary = keys::x_only(STRANGER)
    });
}

#[test]
fn f4b_next_state_token_covid_changed() {
    next_field_case("4b: next state, another token family", |s| {
        s.token_covid = OTHER_TOKEN_COVID
    });
}

#[test]
fn f4c_next_state_token_template_changed() {
    next_field_case("4c: next state, another token template", |s| {
        s.token_template = [0; 32]
    });
}

#[test]
fn f4d_next_state_start_changed() {
    next_field_case("4d: next state, start one unit earlier", |s| s.start -= 1);
}

#[test]
fn f4e_next_state_cliff_changed() {
    next_field_case("4e: next state, cliff one unit shorter", |s| s.cliff -= 1);
}

#[test]
fn f4f_next_state_duration_changed() {
    next_field_case("4f: next state, duration one unit shorter", |s| {
        s.duration -= 1
    });
}

#[test]
fn f4g_next_state_total_changed() {
    next_field_case("4g: next state, total one token larger", |s| s.total += 1);
}

#[test]
fn f4h_next_state_released_one_short() {
    next_field_case("4h: next state, released one token short", |s| {
        s.released -= 1
    });
}

#[test]
fn f4i_next_state_released_not_advanced() {
    next_field_case("4i: next state, released left as it was", |s| {
        s.released = 0
    });
}

#[test]
fn f4j_next_coin_loses_its_kas() {
    let mut release = named(
        first(),
        "4j: the next vesting coin holds one sompi less KAS",
    );
    release.next_vesting_value = COIN_VALUE - 1;
    expect_vesting_refuses(FILE, &run(&release), NEXT_KAS);
}

#[test]
fn f4k_no_next_coin_while_tokens_remain() {
    let mut release = named(first(), "4k: the vesting coin ends while tokens remain");
    release.next_vesting = None;
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

#[test]
fn f4l_next_coin_runs_another_program() {
    let mut release = named(
        first(),
        "4l: the next coin runs a contract without the time rule",
    );
    release.next_source = Some(SOURCE.replace(TIME, "require(true);"));
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

#[test]
fn f4m_two_next_coins() {
    let release = named(first(), "4m: two next vesting coins");
    let mut scenario = release.scenario();
    let next = release.next_compiled().expect("the coin continues");
    let mut extra = PlainOutput::new(next.script_public_key(), COIN_VALUE);
    extra.binding = Some(CovenantBinding::new(0, vesting_covid()));
    let last = scenario.outputs.len() - 1;
    scenario.outputs.insert(last, OutputSpec::Plain(extra));
    if let OutputSpec::Plain(change) = scenario.outputs.last_mut().unwrap() {
        change.value -= COIN_VALUE;
    }
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, ONE_NEXT);
}

/// After the end the beneficiary may keep the vesting coin once the schedule is released, to
/// take a coin sent later (`tests/honest.rs`,
/// `after_the_end_the_beneficiary_may_keep_the_vesting_coin`). Ending it is refused while a
/// rest of the vault stays with the covenant: that rest could never move again.
#[test]
fn f4n_after_the_end_the_vesting_coin_ends_while_a_rest_stays_with_the_covenant() {
    let mut release = Release::honest(
        "4n: at the end, the schedule's tokens taken from an over-funded vault, the vesting coin ended while 5 tokens stay with the covenant",
        schedule(),
        TOTAL + 5,
        TOTAL,
        at(DURATION),
    );
    release.next_vesting = None;
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

// ---------------------------------------------------------------------------------------
// 5. The token input.

#[test]
fn f5a_two_token_coins_of_the_family() {
    let release = named(
        first(),
        "5a: a second coin of the token family, owned by the covenant, as a delegate",
    );
    let mut scenario = release.scenario();
    scenario.push_contract_input(token_input(
        vault_state(500),
        kcc20_family::shapes::COVENANT_ID_BYTES,
        0x50,
        Vec::new(),
        Entry::TransferDelegator,
    ));
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, ONE_TOKEN);
}

#[test]
fn f5b_a_token_of_another_family_with_the_same_program() {
    let mut release = named(
        first(),
        "5b: the vault is a coin of another family, same KCC20 program and layout",
    );
    release.vault_covid = Hash::from_bytes(OTHER_TOKEN_COVID);
    expect_vesting_refuses(FILE, &run(&release), ONE_TOKEN);
}

#[test]
fn f5c_a_coin_of_the_family_running_another_program() {
    let mut release = named(
        first(),
        "5c: a coin under the token's covenant id running a changed KCC20 program",
    );
    let family = Family::pinned();
    let mut bytecode = family.bytecode(kcc20_family::Contract::Kcc20).to_vec();
    let last = bytecode.len() - 1;
    bytecode[last] ^= 0x01;
    let changed = family
        .with_bytecode(kcc20_family::Contract::Kcc20, bytecode)
        .expect("same length");
    release.token_family = changed.clone();
    let scenario = release.scenario();
    let assembled = scenario.assemble(&changed).expect("assembles");
    let judged = token_vesting::judge::judge(
        &node_for(release.lock_time),
        assembled,
        &[(0, release.compiled())],
    );
    expect_vesting_refuses(FILE, &judged, READ_VAULT);
}

#[test]
fn f5d_vault_owned_by_the_beneficiary_key() {
    let mut release = named(
        first(),
        "5d: the token coin spent is the beneficiary's own (owner scheme 0x00)",
    );
    release.vault = Kcc20State::new(keys::x_only(BENEFICIARY), TOTAL);
    let mut scenario = release.scenario();
    *scenario.contract_input_mut(1).witness_mut() =
        witness::owner_key(BENEFICIARY, EntryRole::Leader, SIG_HASH_ALL);
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, VAULT_OWNER);
}

#[test]
fn f5e_vault_with_our_bytes_under_owner_scheme_0x03() {
    let mut release = named(
        first(),
        "5e: the token coin spent has owner scheme 0x03 with the covenant id's bytes",
    );
    release.vault = vault_state(TOTAL).with_owner(OWNER_P2SH, VESTING_COVID);
    let judged = run(&release);
    expect_vesting_refuses(FILE, &judged, VAULT_SCHEME);
}

#[test]
fn f5f_a_public_mint_coin_of_the_family_in_place_of_the_vault() {
    let release = named(
        first(),
        "5f: a PublicMint coin of the token family in place of the vault",
    );
    let mut scenario = release.scenario();
    scenario.inputs[1] = InputSpec::Contract(ContractInput::new(
        PublicMintState::new(keys::x_only(STRANGER), 25, 10),
        COIN_VALUE,
        Some(token_vesting::release::token_covid()),
        outpoint(0x51, 0),
        Call::typed(Entry::MintReclaimInto, Vec::new()),
    ));
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, READ_VAULT);
}

#[test]
fn f5g_vault_owned_by_another_covenant() {
    let mut release = named(
        first(),
        "5g: the token coin spent is owned by another covenant id (scheme 0x04)",
    );
    release.vault = vault_state(TOTAL).with_owner(OWNER_COVENANT_ID, OTHER_TOKEN_COVID);
    let judged = run(&release);
    expect_vesting_refuses(FILE, &judged, VAULT_OWNER);
}

// ---------------------------------------------------------------------------------------
// 6. Any input of the covenant opens every coin the covenant owns.

#[test]
fn f6a_a_second_token_owned_by_the_covenant_goes_to_a_stranger() {
    let release = named(
        first(),
        "6a: a token of another family, owned by the covenant, leaves to a stranger in the same transaction",
    );
    let mut scenario = release.scenario();
    let other = vault_state(777);
    let index = scenario.push_contract_input(token_input(
        other,
        OTHER_TOKEN_COVID,
        0x60,
        vec![Kcc20State::new(keys::x_only(STRANGER), 777)],
        Entry::Transfer,
    ));
    let last = scenario.outputs.len() - 1;
    scenario.outputs.insert(
        last,
        OutputSpec::Contract(ContractOutput::new(
            Kcc20State::new(keys::x_only(STRANGER), 777),
            COIN_VALUE,
            Some(CovenantBinding::new(
                index as u16,
                Hash::from_bytes(OTHER_TOKEN_COVID),
            )),
        )),
    );
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, NO_OTHER_COVENANT);
    assert!(
        judged.inputs[index].accepted,
        "the other token's own program lets it go: the covenant is present"
    );
}

#[test]
fn f6b_a_token_owned_by_the_covenant_cannot_move_without_the_vesting_coin() {
    // The same theft with the vesting coin left out: the token's program refuses.
    let release = named(
        first(),
        "6b: the vault leaves to a stranger without the vesting coin",
    );
    let mut scenario = kcc20_family::Scenario::new(release.name.clone());
    scenario.lock_time = release.lock_time;
    scenario.push_contract_input(token_input(
        vault_state(TOTAL),
        kcc20_family::shapes::COVENANT_ID_BYTES,
        0x61,
        vec![Kcc20State::new(keys::x_only(STRANGER), TOTAL)],
        Entry::Transfer,
    ));
    scenario.push_ordinary_input(funding_input(outpoint(0x62, 0)));
    scenario.push_contract_output(ContractOutput::new(
        Kcc20State::new(keys::x_only(STRANGER), TOTAL),
        COIN_VALUE,
        Some(CovenantBinding::new(
            0,
            Hash::from_bytes(kcc20_family::shapes::COVENANT_ID_BYTES),
        )),
    ));
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        token_vesting::release::FUNDING_VALUE - token_vesting::release::DEFAULT_FEE,
    ));
    let judged = run_with(scenario, node_for(release.lock_time), &[]);
    expect_token_refuses(FILE, &judged, 0, "OpCovInputCount(gen__glob_owner) > 0");
}

// ---------------------------------------------------------------------------------------
// 7. Signatures.

#[test]
fn f7a_no_signature_zero_bytes() {
    let mut release = named(
        first(),
        "7a: no signature: 64 zero bytes and the type byte of All",
    );
    let mut bytes = vec![0u8; 64];
    bytes.push(0x01);
    release.signer = Signer::Fixed(bytes);
    expect_vesting_refuses(FILE, &run(&release), SIG);
}

#[test]
fn f7b_a_stranger_signs() {
    let mut release = named(
        first(),
        "7b: a stranger's valid signature of the transaction",
    );
    release.signer = Signer::Key(STRANGER, SIG_HASH_ALL);
    expect_vesting_refuses(FILE, &run(&release), SIG);
}

fn signature_type_case(name: &str, hash_type: SigHashType) {
    let mut release = named(first(), name);
    release.signer = Signer::Key(BENEFICIARY, hash_type);
    expect_vesting_refuses(FILE, &run(&release), SIG_TYPE);
}

#[test]
fn f7c_signature_type_none() {
    signature_type_case("7c: the beneficiary signs with type None", SIG_HASH_NONE);
}

#[test]
fn f7d_signature_type_single() {
    signature_type_case(
        "7d: the beneficiary signs with type Single",
        SIG_HASH_SINGLE,
    );
}

#[test]
fn f7e_signature_type_all_anyone_can_pay() {
    signature_type_case(
        "7e: the beneficiary signs with type All | AnyOneCanPay",
        SigHashType::from_u8(0x81).unwrap(),
    );
}

#[test]
fn f7f_signature_type_none_anyone_can_pay() {
    signature_type_case(
        "7f: the beneficiary signs with type None | AnyOneCanPay",
        SigHashType::from_u8(0x82).unwrap(),
    );
}

#[test]
fn f7g_signature_type_single_anyone_can_pay() {
    signature_type_case(
        "7g: the beneficiary signs with type Single | AnyOneCanPay",
        SigHashType::from_u8(0x84).unwrap(),
    );
}

#[test]
fn f7h_a_signature_made_for_another_transaction() {
    // The beneficiary's signature of the honest release, carried into a transaction that
    // sends the claim to a stranger.
    let honest = first();
    let assembled = honest.scenario().assemble(Family::pinned()).unwrap();
    let mutable = kaspa_consensus_core::tx::MutableTransaction::with_entries(
        assembled.tx.clone(),
        assembled.utxos.clone(),
    );
    let ctx = keys::SigningContext {
        tx: &mutable,
        input_index: 0,
    };
    let signature = keys::schnorr_signature(&ctx, &keys::keypair(BENEFICIARY), SIG_HASH_ALL);
    let mut release = named(
        first(),
        "7h: the beneficiary's signature of the honest release, in a release to a stranger",
    );
    release.signer = Signer::Fixed(signature);
    release.token_outputs[0] = Kcc20State::new(keys::x_only(STRANGER), release.claim);
    expect_vesting_refuses(FILE, &run(&release), SIG);
}

// ---------------------------------------------------------------------------------------
// 8. Borrowing.

#[test]
fn f8a_beneficiary_output_open_to_borrowing() {
    let mut release = named(
        first(),
        "8a: the claim's coin is open to borrowing by anyone (scheme 0x01, threshold 0)",
    );
    release.token_outputs[0] = release.token_outputs[0].clone().with_threshold(0);
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f8b_rest_open_to_borrowing() {
    let mut release = named(
        first(),
        "8b: the rest's coin is open to borrowing by anyone (scheme 0x01, threshold 0)",
    );
    release.token_outputs[1] = release.token_outputs[1].clone().with_threshold(0);
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f8c_rest_with_a_strangers_borrow_key() {
    let mut release = named(
        first(),
        "8c: the rest's coin names a stranger's borrow key (scheme 0x02)",
    );
    release.token_outputs[1] = release.token_outputs[1].clone().with_borrow(
        kcc20_family::states::BORROW_SCHNORR_SIGNATURE,
        keys::x_only(STRANGER),
    );
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f8d_the_vault_on_the_borrow_path() {
    let release = named(
        first(),
        "8d: the vault's transfer takes the borrow path in a release",
    );
    let mut scenario = release.scenario();
    *scenario.contract_input_mut(1).witness_mut() = LateBytes::fixed(vec![PATH_BORROW]);
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_token_refuses(FILE, &judged, 1, "require(false); | rule K-50");
}

// ---------------------------------------------------------------------------------------
// 9. Positions out of range and overflow.

#[test]
fn f9a_no_token_input() {
    let release = named(first(), "9a: no token input at all, the vesting coin alone");
    let mut scenario = release.scenario();
    scenario.inputs.remove(1);
    scenario
        .outputs
        .retain(|output| !matches!(output, OutputSpec::Contract(_)));
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, ONE_TOKEN);
}

#[test]
fn f9b_one_token_output_while_tokens_remain() {
    let mut release = named(
        first(),
        "9b: tokens remain but only the claim's output exists",
    );
    release.token_outputs.truncate(1);
    release.token_output_values.truncate(1);
    let judged = run(&release);
    expect_vesting_refuses(FILE, &judged, TWO_TOKEN_OUTPUTS);
}

#[test]
fn f9c_too_many_inputs() {
    let release = named(first(), "9c: nine inputs, one more than the loop's bound");
    let mut scenario = release.scenario();
    for seed in 0..6u8 {
        scenario.push_ordinary_input(funding_input(outpoint(0x70 + seed, 0)));
    }
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, MAX_INPUTS);
}

#[test]
fn f9d_claim_at_the_top_of_the_range() {
    let base = first();
    let release = Release::honest(
        "9d: a claim of 2^63 - 1",
        base.vesting,
        TOTAL,
        i64::MAX,
        base.lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f9e_a_coin_whose_duration_would_overflow_the_formula() {
    // Duration 4e9: without the bound, (total % duration) * elapsed reaches 1.6e19, past 2^63.
    let mut state = schedule();
    state.total = 3_999_999_999;
    state.duration = 4_000_000_000;
    state.cliff = 0;
    let lock_time = at(3_999_999_998);
    let release = Release::honest(
        "9e: a coin made with duration 4,000,000,000: the formula would overflow without the bound",
        state.clone(),
        state.total,
        1,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), DURATION_BOUND);
}

#[test]
fn f9f_a_coin_whose_start_plus_cliff_would_overflow() {
    let mut state = schedule();
    state.start = i64::MAX - 10;
    state.cliff = 100;
    let release = Release::honest(
        "9f: a coin made with start 2^63 - 11 and cliff 100: start + cliff would overflow",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), START_BOUND);
}

#[test]
fn f9i_a_coin_whose_cliff_would_overflow() {
    let mut state = schedule();
    state.cliff = i64::MAX - 5;
    let release = Release::honest(
        "9i: a coin made with cliff 2^63 - 6: start + cliff would overflow",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), CLIFF_BOUND);
}

#[test]
fn f9j_a_coin_made_with_duration_zero() {
    let mut state = schedule();
    state.duration = 0;
    let release = Release::honest(
        "9j: a coin made with duration 0",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), DURATION_POSITIVE);
}

#[test]
fn f9k_a_coin_made_with_total_zero() {
    let mut state = schedule();
    state.total = 0;
    let release = Release::honest(
        "9k: a coin made with total 0",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), TOTAL_POSITIVE);
}

#[test]
fn f9l_a_coin_made_with_a_negative_start() {
    let mut state = schedule();
    state.start = -1;
    state.cliff += start_of_schedule();
    let release = Release::honest(
        "9l: a coin made with start -1",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), START_LOW);
}

#[test]
fn f9m_a_coin_made_with_a_negative_cliff() {
    let mut state = schedule();
    state.cliff = -1;
    let release = Release::honest(
        "9m: a coin made with cliff -1",
        state,
        TOTAL,
        1,
        at(CLIFF + 500),
    );
    expect_vesting_refuses(FILE, &run(&release), CLIFF_LOW);
}

#[test]
fn f9g_a_coin_made_with_released_above_total() {
    let state = schedule().with_released(TOTAL + 1);
    let release = Release::honest(
        "9g: a coin made with released > total",
        state,
        TOTAL,
        1,
        at(DURATION),
    );
    expect_vesting_refuses(FILE, &run(&release), RELEASED_HIGH);
}

/// 9k after the end of the schedule: the amount rule does not run there, so only
/// `require(total > 0)` refuses this coin.
#[test]
fn f9n_a_coin_made_with_total_zero_after_the_end() {
    let mut state = schedule();
    state.total = 0;
    let release = Release::honest(
        "9n: a coin made with total 0, the whole vault after the end",
        state,
        TOTAL,
        TOTAL,
        at(DURATION),
    );
    expect_vesting_refuses(FILE, &run(&release), TOTAL_POSITIVE);
}

#[test]
fn f9h_a_coin_made_with_negative_released() {
    let state = schedule().with_released(-1_000);
    let release = Release::honest(
        "9h: a coin made with released = -1,000",
        state,
        TOTAL,
        1,
        at(DURATION),
    );
    expect_vesting_refuses(FILE, &run(&release), RELEASED_LOW);
}

// ---------------------------------------------------------------------------------------
// 10. Call arguments that do not match the outputs.

#[test]
fn f10a_claim_argument_smaller_than_the_beneficiary_output() {
    let mut release = named(
        first(),
        "10a: claim 150,000 in the call, 150,001 in the beneficiary's output",
    );
    release.token_outputs[0].amount += 1;
    release.token_outputs[1].amount -= 1;
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f10b_claim_argument_larger_than_the_beneficiary_output() {
    let mut release = named(
        first(),
        "10b: claim 150,000 in the call, 149,999 in the beneficiary's output",
    );
    release.token_outputs[0].amount -= 1;
    release.token_outputs[1].amount += 1;
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f10c_token_declares_other_states_than_it_locks() {
    let mut release = named(
        first(),
        "10c: the token's call declares other states than the outputs carry",
    );
    let mut declared = release.token_outputs.clone();
    declared[0] = Kcc20State::new(keys::x_only(STRANGER), release.claim);
    release.declared = Some(declared);
    let judged = run(&release);
    expect_token_refuses(FILE, &judged, 1, "validateOutputState");
}

// ---------------------------------------------------------------------------------------
// 11. Lock time, sequences and the coins spent.

#[test]
fn f11a_lock_time_of_the_timestamp_type() {
    // A lock time in milliseconds is a far larger number than any DAA score: read as a DAA
    // score it would vest everything at once.
    let base = first();
    let lock_time = 1_779_999_999_000u64;
    let release = Release::honest(
        "11a: a timestamp lock time, asking for everything",
        base.vesting,
        TOTAL,
        TOTAL,
        lock_time,
    );
    let judged = run_scenario(&release, release.scenario(), node_for(base.lock_time));
    expect_vesting_refuses(FILE, &judged, TIME);
}

#[test]
fn f11b_lock_time_ahead_of_the_chain() {
    // Every script passes (the contract believes the lock time), the node refuses the
    // transaction as not final until the DAA score passes the lock time.
    let base = first();
    let lock_time = at(DURATION);
    let release = Release::honest(
        "11b: lock time at the end of the schedule, judged 500 units after the cliff",
        base.vesting,
        TOTAL,
        TOTAL,
        lock_time,
    );
    let judged = run_scenario(&release, release.scenario(), node_for(base.lock_time));
    expect_consensus_refuses(FILE, &judged, Stage::HeaderContext, "check_tx_is_finalized");
    assert!(
        judged.inputs.iter().all(|input| input.accepted),
        "the scripts alone accept it"
    );
}

#[test]
fn f11c_final_sequence_on_the_vesting_input() {
    let mut release = named(
        first(),
        "11c: the vesting input's sequence is final (lock time switched off for it)",
    );
    release.vesting_sequence = u64::MAX;
    expect_vesting_refuses(FILE, &run(&release), TIME);
}

#[test]
fn f11d_two_vesting_coins_of_the_covenant() {
    let release = named(
        first(),
        "11d: a second coin of the vesting covenant in the same transaction",
    );
    let mut scenario = release.scenario();
    let compiled = release.compiled();
    let copy = compiled.clone();
    let claim = release.claim;
    let index = scenario.push_ordinary_input(
        OrdinaryInput::new(
            compiled.script_public_key(),
            COIN_VALUE,
            outpoint(0x80, 0),
            LateBytes::from_tx(move |ctx| {
                copy.release_signature_script(
                    &keys::schnorr_signature(ctx, &keys::keypair(BENEFICIARY), SIG_HASH_ALL),
                    claim,
                )
            }),
        )
        .with_covenant_id(vesting_covid()),
    );
    let judged = run_with(
        scenario,
        node_for(release.lock_time),
        &[(0, compiled.clone()), (index, compiled)],
    );
    expect_vesting_refuses(FILE, &judged, ONE_OWN);
}

#[test]
fn f11e_vault_holds_less_than_the_claim() {
    // A coin of the family owned by the covenant but holding fewer tokens than claimed
    // (another, smaller coin the covenant owns).
    let base = first();
    let release = Release::honest(
        "11e: the token coin spent holds 10 tokens, the claim is 150,000",
        base.vesting,
        10,
        base.claim,
        base.lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), "require(vault.amount >= claim);");
}

#[test]
fn f11f_the_vesting_coin_ends_early_with_nothing_left_in_the_vault() {
    // The vault is emptied by a claim the schedule allows only partly: refused by the amount.
    let base = first();
    let release = Release::honest(
        "11f: the whole vault at 1,500",
        base.vesting,
        TOTAL,
        TOTAL,
        base.lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

// ---------------------------------------------------------------------------------------
// 12. Tokens beyond the schedule (from the end of the schedule on the beneficiary may take a
// token coin of the covenant whole, and only then).

#[test]
fn f12a_the_extra_tokens_one_unit_before_the_end() {
    let state = schedule();
    let lock_time = at(DURATION - 1);
    let claim = state.vested_at(lock_time as i64) + 5;
    let release = Release::honest(
        "12a: one unit before the end, what is vested plus the 5 tokens beyond the schedule",
        state,
        TOTAL + 5,
        claim,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f12b_the_whole_over_funded_vault_one_unit_before_the_end() {
    let release = Release::honest(
        "12b: one unit before the end, the whole over-funded vault",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION - 1),
    );
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f12c_a_coin_sent_later_taken_before_the_end() {
    // Everything vested so far is released; a coin of 7 tokens sent to the covenant later.
    let state = schedule().with_released(150_000);
    let mut release = Release::honest(
        "12c: before the end, a coin of 7 tokens sent to the covenant later, whole",
        state,
        7,
        7,
        at(CLIFF + 500),
    )
    .with_other_coins_on_the_covenant();
    release.vault_outpoint = outpoint(0x61, 0);
    expect_vesting_refuses(FILE, &run(&release), AMOUNT);
}

#[test]
fn f12d_the_whole_over_funded_vault_to_a_strangers_key() {
    let mut release = Release::honest(
        "12d: after the end, the whole over-funded vault to a stranger's key",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    release.token_outputs[0] = Kcc20State::new(keys::x_only(STRANGER), TOTAL + 5);
    expect_vesting_refuses(FILE, &run(&release), RECIPIENT);
}

#[test]
fn f12e_the_schedule_to_the_beneficiary_the_extra_to_a_stranger() {
    let mut release = Release::honest(
        "12e: after the end, total to the beneficiary and the 5 extra tokens to a stranger's key",
        schedule(),
        TOTAL + 5,
        TOTAL,
        at(DURATION),
    );
    release.token_outputs[1] = Kcc20State::new(keys::x_only(STRANGER), 5);
    expect_vesting_refuses(FILE, &run(&release), CHANGE);
}

#[test]
fn f12f_a_stranger_signs_for_the_whole_vault_after_the_end() {
    let mut release = Release::honest(
        "12f: after the end, a stranger signs for the whole over-funded vault, to the stranger's key",
        schedule(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    release.signer = Signer::Key(STRANGER, SIG_HASH_ALL);
    release.token_outputs[0] = Kcc20State::new(keys::x_only(STRANGER), TOTAL + 5);
    expect_vesting_refuses(FILE, &run(&release), SIG);
}

/// After the end with nothing left in the vault the beneficiary may keep the vesting coin,
/// but one coin only, with the schedule and `released` as the contract computes them.
fn kept_after_the_end(name: &str) -> Release {
    let state = schedule();
    let mut release = Release::honest(name, state.clone(), TOTAL, TOTAL, at(DURATION));
    release.next_vesting = Some(state.with_released(TOTAL));
    release
}

#[test]
fn f12g_two_kept_vesting_coins_after_the_end() {
    let release = kept_after_the_end("12g: after the end, the vesting coin kept twice");
    let mut scenario = release.scenario();
    let next = release.next_compiled().expect("the coin is kept");
    let mut extra = PlainOutput::new(next.script_public_key(), COIN_VALUE);
    extra.binding = Some(CovenantBinding::new(0, vesting_covid()));
    let last = scenario.outputs.len() - 1;
    scenario.outputs.insert(last, OutputSpec::Plain(extra));
    if let OutputSpec::Plain(change) = scenario.outputs.last_mut().unwrap() {
        change.value -= COIN_VALUE;
    }
    let judged = run_scenario(&release, scenario, node_for(release.lock_time));
    expect_vesting_refuses(FILE, &judged, ONE_NEXT);
}

#[test]
fn f12h_the_kept_vesting_coin_starts_the_schedule_again() {
    let mut release =
        kept_after_the_end("12h: after the end, the kept vesting coin has released = 0 again");
    release.next_vesting = Some(schedule());
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

#[test]
fn f12i_the_kept_vesting_coin_counts_the_extra_tokens_in_released() {
    // released must stop at total: total + 5 would make a coin the contract refuses at
    // `released <= total` for ever.
    let state = schedule();
    let mut release = Release::honest(
        "12i: the whole over-funded vault at the end, the vesting coin kept with released = total + 5",
        state.clone(),
        TOTAL + 5,
        TOTAL + 5,
        at(DURATION),
    );
    release.next_vesting = Some(state.with_released(TOTAL + 5));
    expect_vesting_refuses(FILE, &run(&release), NEXT_STATE);
}

#[test]
fn f12j_the_kept_vesting_coin_loses_its_kas() {
    let mut release = kept_after_the_end("12j: after the end, the kept vesting coin with less KAS");
    release.next_vesting_value = COIN_VALUE - 1;
    expect_vesting_refuses(FILE, &run(&release), NEXT_KAS);
}

/// After the end the vesting coin may end only once released has reached total: here a
/// coin of 7 tokens sent later is taken whole while the schedule still has 850,000 to
/// release, and the vesting coin is ended, which would lock the rest of the vault for good.
/// The same move as `s2_c3a` in `tests/review_beyond_continuation.rs`.
#[test]
fn f12k_after_the_end_the_vesting_coin_ended_while_released_is_below_total() {
    let mut release = Release::honest(
        "12k: after the end, a coin of 7 sent later taken whole, the vesting coin ended with released 150,007",
        schedule().with_released(150_000),
        7,
        7,
        at(DURATION + 100),
    )
    .with_other_coins_on_the_covenant();
    release.vault_outpoint = outpoint(0x61, 0);
    assert!(release.next_vesting.is_some());
    release.next_vesting = None;
    expect_vesting_refuses(FILE, &run(&release), ONE_NEXT);
}

// ---------------------------------------------------------------------------------------
// 13. The moves of tests/review_mutants.rs (s_m1 to s_m5): each is refused by one rule, and
// the contract without that rule accepts it, so the checking of the checks catches these
// five rules by the verdict, not only by the place of a refusal.

#[test]
fn f13a_r04_a_coin_made_with_start_above_max_daa_inside_the_daa_domain() {
    let mut state = schedule();
    state.start = 450_000_000_000;
    let lock_time = (state.start + CLIFF + 500) as u64;
    let claim = state.vested_at(lock_time as i64);
    let release = Release::honest(
        "13a (s-m1): a coin made with start 4.5e11, the honest first release",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), START_BOUND);
}

#[test]
fn f13b_r06_a_coin_made_with_cliff_above_max_daa_inside_the_daa_domain() {
    let mut state = schedule();
    state.cliff = 410_000_000_000;
    let lock_time = (state.start + state.cliff + 500) as u64;
    let release = Release::honest(
        "13b (s-m2): a coin made with cliff 4.1e11, everything after the cliff",
        state,
        TOTAL,
        TOTAL,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), CLIFF_BOUND);
}

#[test]
fn f13c_r08_a_coin_made_with_duration_4e9_without_any_overflow() {
    let mut state = schedule();
    state.duration = 4_000_000_000;
    state.total = 1_000_000_000_000;
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    assert_eq!(claim, 375_000);
    let release = Release::honest(
        "13c (s-m3): a coin made with duration 4e9 and total 1e12, the honest first release",
        state.clone(),
        state.total,
        claim,
        lock_time,
    );
    expect_vesting_refuses(FILE, &run(&release), DURATION_BOUND);
}

#[test]
fn f13d_r19_a_token_coin_owned_by_the_tokens_own_covenant_in_place_of_the_vault() {
    let mut release = named(
        first(),
        "13d (s-m4): the vault is a token coin owned by the token's own covenant id",
    );
    release.vault = release
        .vault
        .clone()
        .with_owner(OWNER_COVENANT_ID, TOKEN_COVID);
    expect_vesting_refuses(FILE, &run(&release), VAULT_OWNER);
}

#[test]
fn f13e_r22_an_extra_empty_token_coin_beside_claim_and_rest() {
    let mut release = named(
        first(),
        "13e (s-m5): claim, rest and an extra empty token coin for a stranger",
    );
    release
        .token_outputs
        .push(Kcc20State::new(keys::x_only(STRANGER), 0));
    release.token_output_values.push(COIN_VALUE);
    expect_vesting_refuses(FILE, &run(&release), TWO_TOKEN_OUTPUTS);
}
