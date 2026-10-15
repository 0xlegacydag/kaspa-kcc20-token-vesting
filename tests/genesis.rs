//! The vault opens to ANY coin that carries the vesting covenant's id (owner scheme 0x04:
//! KCC20.sil, line 96, takes the owner as present when `OpCovInputCount(gen__glob_owner) > 0`),
//! and consensus lets one genesis give that id to several outputs. Checked here.
//!
//! - `g1`: consensus accepts a genesis with the vesting coin AND a pay-to-key coin under
//!   the same id.
//! - `g2`, `g3`: forbidden moves that such a launch makes possible. They are written as
//!   forbidden moves (expected: refused) and FAIL: nothing on chain can refuse them, because
//!   the vesting script never runs (g2) or runs once per coin (g3). They stay red on purpose.
//! - `g4`, `g5`, `g7`: the defence that works, outside the chain: a check of the genesis and
//!   of the vault before any token is sent (`token_vesting::launch`); `g7` checks that
//!   `check_vault` takes the vault coin whole.
//! - `g6`: a release that also starts a new covenant anchored to the vesting input is
//!   accepted: such an output is not counted by `OpAuthOutputCount`.

mod common;

use common::{at, expect_accepted, node_for, run, run_with, schedule, show};
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutpoint};
use kaspa_hashes::Hash;
use kcc20_family::scenario::OutputSpec;
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::{
    Arg, Assembled, ContractInput, ContractOutput, Entry, Family, Kcc20State, LateBytes,
    OrdinaryInput, PlainOutput, Scenario, keys,
};
use node_gate::GateParams;
use token_vesting::contract::Compiled;
use token_vesting::judge::write_record;
use token_vesting::launch::{LaunchError, check_vault, check_vesting_genesis};
use token_vesting::release::{
    BENEFICIARY, CLIFF, COIN_VALUE, DEFAULT_FEE, FUNDING_VALUE, OTHER_TOKEN_COVID, Release,
    STRANGER, TOTAL, funding_input, token_coin, token_covid, vault_state, vesting_covid,
};

const FILE: &str = "genesis";

/// What else the genesis gives the vesting id to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extra {
    Nothing,
    BeneficiaryKeyCoin,
    SecondVestingCoin,
}

/// A genesis of the vesting covenant from the funder's coin: output 0 is the vesting coin,
/// output 1 (if any) the extra coin under the same id, the last output the change.
fn genesis(extra: Extra) -> (Assembled, Hash) {
    let state = schedule();
    let vesting_script = Compiled::new(&state).script_public_key();
    let funding = outpoint(0x90, 0);
    let build = |id: Hash| {
        let mut scenario = Scenario::new("genesis of the vesting covenant");
        scenario.push_ordinary_input(funding_input(funding));
        let mut vesting = PlainOutput::new(vesting_script.clone(), COIN_VALUE);
        vesting.binding = Some(CovenantBinding::new(0, id));
        scenario.push_plain_output(vesting);
        let extra_script = match extra {
            Extra::Nothing => None,
            Extra::BeneficiaryKeyCoin => Some(pay_to_public_key_script(&keys::x_only(BENEFICIARY))),
            Extra::SecondVestingCoin => Some(vesting_script.clone()),
        };
        if let Some(script) = extra_script {
            let mut output = PlainOutput::new(script, COIN_VALUE);
            output.binding = Some(CovenantBinding::new(0, id));
            scenario.push_plain_output(output);
        }
        let used: u64 = scenario.outputs.iter().map(OutputSpec::value).sum();
        scenario.push_plain_output(PlainOutput::new(
            pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
            FUNDING_VALUE - used - DEFAULT_FEE,
        ));
        scenario.assemble(Family::pinned()).expect("assembles")
    };
    let draft = build(Hash::from_bytes([0; 32]));
    let group = if extra == Extra::Nothing { 1 } else { 2 };
    let id = covenant_id(
        funding,
        (0..group).map(|index| (index as u32, &draft.tx.outputs[index])),
    );
    (build(id), id)
}

fn judge_genesis(assembled: Assembled) -> token_vesting::judge::Judged {
    token_vesting::judge::judge(&GateParams::testnet10(), assembled, &[])
}

#[test]
fn g1_consensus_accepts_a_genesis_that_gives_the_vesting_id_to_a_key_coin_too() {
    let (assembled, _) = genesis(Extra::BeneficiaryKeyCoin);
    let mut judged = judge_genesis(assembled);
    judged.name = "g1: a genesis gives the vesting id to the vesting coin and to a coin of the beneficiary's key".into();
    expect_accepted(FILE, &judged);
}

#[test]
fn g2_the_key_coin_with_the_vesting_id_moves_the_whole_vault_at_once() {
    // RED ON PURPOSE: the move is accepted. See the module comment.
    let (genesis_tx, id) = genesis(Extra::BeneficiaryKeyCoin);
    let mut scenario = Scenario::new(
        "g2: the beneficiary's key coin carrying the vesting id takes the whole vault before the cliff",
    );
    scenario.lock_time = 0;
    let key_coin = genesis_tx.tx.outputs[1].clone();
    scenario.push_ordinary_input(
        OrdinaryInput::new(
            key_coin.script_public_key.clone(),
            key_coin.value,
            TransactionOutpoint::new(genesis_tx.tx.id(), 1),
            LateBytes::from_tx(|ctx| {
                kcc20_family::raw::push(&keys::schnorr_signature(
                    ctx,
                    &keys::keypair(BENEFICIARY),
                    SIG_HASH_ALL,
                ))
                .expect("fits")
            }),
        )
        .with_covenant_id(id),
    );
    let everything = Kcc20State::new(keys::x_only(BENEFICIARY), TOTAL);
    scenario.push_contract_input(ContractInput::new(
        vault_state(TOTAL).with_owner(kcc20_family::states::OWNER_COVENANT_ID, id.as_bytes()),
        COIN_VALUE,
        Some(token_covid()),
        outpoint(0x91, 0),
        kcc20_family::scenario::Call::typed(
            Entry::Transfer,
            vec![
                Arg::States(vec![everything.clone()]),
                Arg::Bytes(LateBytes::fixed(vec![0x00])),
            ],
        ),
    ));
    scenario.push_ordinary_input(funding_input(outpoint(0x92, 0)));
    scenario.push_contract_output(ContractOutput::new(
        everything,
        COIN_VALUE,
        Some(CovenantBinding::new(1, token_covid())),
    ));
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        FUNDING_VALUE + COIN_VALUE - DEFAULT_FEE,
    ));
    let judged = run_with(scenario, node_for(at(0)), &[]);
    show(&judged);
    let refused = judged.refused();
    write_record(
        FILE,
        &judged.record(
            "refused (a forbidden move: the whole vault before the cliff)",
            if refused {
                "as expected"
            } else {
                "NOT AS EXPECTED: accepted, the hole is real"
            },
        ),
    );
    assert!(
        refused,
        "HOLE: a second coin with the vesting id moved the whole vault; the vesting script never ran"
    );
}

#[test]
fn g3_two_vesting_coins_release_the_schedule_twice() {
    // RED ON PURPOSE: both releases are accepted. See the module comment.
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut first = Release::honest(
        "g3 1/2: coin A releases what is vested",
        state.clone(),
        TOTAL,
        claim,
        lock_time,
    );
    first.vesting_outpoint = outpoint(0x93, 0);
    let one = run(&first);
    expect_accepted(FILE, &one);
    let mut second = Release::honest(
        "g3 2/2: coin B, another coin of the same id, releases the same tokens again",
        state,
        TOTAL - claim,
        claim,
        lock_time,
    );
    second.vesting_outpoint = outpoint(0x94, 0);
    second.vault_outpoint = TransactionOutpoint::new(one.assembled.tx.id(), 1);
    second.funding_outpoint = outpoint(0x95, 0);
    let two = run(&second);
    show(&two);
    let refused = two.refused();
    write_record(
        FILE,
        &two.record(
            "refused (the same schedule released twice)",
            if refused {
                "as expected"
            } else {
                "NOT AS EXPECTED: accepted, the hole is real"
            },
        ),
    );
    assert!(
        refused,
        "HOLE: a second vesting coin of the same id released the schedule again"
    );
}

#[test]
fn g4_the_launch_check_refuses_a_genesis_with_more_than_one_coin_of_the_id() {
    let state = schedule();
    for extra in [Extra::BeneficiaryKeyCoin, Extra::SecondVestingCoin] {
        let (assembled, _) = genesis(extra);
        let answer = check_vesting_genesis(&assembled.tx, 0, &state);
        println!("{answer:?}");
        assert!(matches!(answer, Err(LaunchError::MoreThanOneCoin { .. })));
    }
    let (assembled, id) = genesis(Extra::Nothing);
    assert_eq!(check_vesting_genesis(&assembled.tx, 0, &state), Ok(id));
    let mut other = state.clone();
    other.beneficiary = keys::x_only(STRANGER);
    assert_eq!(
        check_vesting_genesis(&assembled.tx, 0, &other),
        Err(LaunchError::NotTheVestingScript)
    );
    let mut judged = judge_genesis(assembled);
    judged.name = "g4: a genesis with exactly one coin of the vesting id".into();
    expect_accepted(FILE, &judged);
}

#[test]
fn g5_the_launch_check_refuses_a_vault_open_to_borrowing_or_over_funded() {
    let state = schedule();
    let id = vesting_covid();
    assert_eq!(
        check_vault(id, &state, &token_coin(vault_state(TOTAL))),
        Ok(())
    );
    assert!(
        check_vault(
            id,
            &state,
            &token_coin(vault_state(TOTAL).with_threshold(0))
        )
        .is_err()
    );
    assert!(check_vault(id, &state, &token_coin(vault_state(TOTAL + 1))).is_err());
    assert!(
        check_vault(
            id,
            &state,
            &token_coin(vault_state(TOTAL).with_owner(0x03, id.as_bytes()))
        )
        .is_err()
    );
}

/// Why weakening `require(OpAuthOutputCount(this.activeInputIndex) == 1)` changes nothing: the
/// node counts as an input's authorized outputs only the continuations of its own covenant.
/// Here the vesting input also authorizes a genesis output (a new covenant id hashed from
/// its outpoint), and the honest release with it is still accepted: had the genesis output
/// been counted, the count would be 2 and the rule would refuse.
#[test]
fn g6_a_release_that_also_starts_a_new_covenant_anchored_to_the_vesting_input() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let release = Release::honest(
        "g6: an honest release that also starts a new covenant anchored to the vesting input",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    let mut scenario = release.scenario();
    let extra = PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(STRANGER)),
        COIN_VALUE,
    );
    let last = scenario.outputs.len() - 1;
    let draft = kaspa_consensus_core::tx::TransactionOutput::new(
        extra.value,
        extra.script_public_key.clone(),
    );
    let new_id = covenant_id(
        release.vesting_outpoint,
        [(last as u32, &draft)].into_iter(),
    );
    let mut extra = extra;
    extra.binding = Some(CovenantBinding::new(0, new_id));
    scenario.outputs.insert(last, OutputSpec::Plain(extra));
    if let Some(OutputSpec::Plain(change)) = scenario.outputs.last_mut() {
        change.value -= COIN_VALUE;
    }
    let judged = run_with(scenario, node_for(lock_time), &[(0, release.compiled())]);
    expect_accepted(FILE, &judged);
}

/// `check_vault` takes the vault coin whole: the coin the honest release spends passes; a
/// coin of another family, a coin whose script is not the KCC20 program with the given state,
/// a state the script does not carry, and a vesting coin naming another token program are
/// refused, though the state alone would pass.
#[test]
fn g7_the_launch_check_takes_the_vault_coin_whole() {
    let state = schedule();
    let id = vesting_covid();
    let lock_time = at(CLIFF + 500);
    let honest = Release::honest(
        "g7: the honest first release",
        state.clone(),
        TOTAL,
        state.vested_at(lock_time as i64),
        lock_time,
    );
    let coin = honest.vault_coin();
    assert_eq!(check_vault(id, &state, &coin), Ok(()));
    // `token_coin` makes the coin the release spends: the same covenant id, script and KAS.
    let made = token_coin(vault_state(TOTAL)).entry;
    assert_eq!(
        (
            &coin.entry.covenant_id,
            &coin.entry.script_public_key,
            coin.entry.amount
        ),
        (&made.covenant_id, &made.script_public_key, made.amount)
    );

    let mut other_family = coin.clone();
    other_family.entry.covenant_id = Some(Hash::from_bytes(OTHER_TOKEN_COVID));
    let mut other_script = coin.clone();
    other_script.entry.script_public_key = pay_to_public_key_script(&keys::x_only(STRANGER));
    let mut said_state = coin.clone();
    said_state.state = vault_state(TOTAL).with_extension([0xee; 32]);
    let mut other_program = state.clone();
    other_program.token_template = [0x33; 32];
    for (what, vesting, vault) in [
        ("another family", &state, &other_family),
        ("another script", &state, &other_script),
        ("a state the script does not carry", &state, &said_state),
        (
            "a vesting coin naming another program",
            &other_program,
            &coin,
        ),
    ] {
        let answer = check_vault(id, vesting, vault);
        println!("{what}: {answer:?}");
        assert!(
            matches!(answer, Err(LaunchError::Vault(_))),
            "{what}: {answer:?}"
        );
    }
}
