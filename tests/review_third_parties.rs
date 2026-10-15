//! Third parties around a release: moves made by somebody other than the beneficiary.
//!
//! - `s_u1`, `s_u2`: a stranger borrows into the vault outside any release. The borrow path
//!   of the token program lets a coin open to borrowing take part in another owner's
//!   transfer without its own owner present.
//! - `s_u3`, `s_u4`: a relayer takes the signed release from a mempool and re-encodes or
//!   rewrites its call. The signature covers the transaction's outputs, not the pushes of
//!   the signature script, so the call's arguments can be changed by whoever relays it.
//! - `s_u5`: an ordinary coin whose script trusts the presence of the vesting covenant.

mod common;

use common::{
    at, expect_accepted, expect_token_refuses, expect_vesting_refuses, node_for, run, run_scenario,
    run_with, schedule, show,
};
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutpoint};
use kaspa_txscript::pay_to_script_hash_script;
use kcc20_family::raw::{PushForm, push_as, push_number, push_redeem, split_pushes};
use kcc20_family::scenario::{Call, InputSpec};
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::{
    Arg, ContractInput, ContractOutput, Entry, EntryRole, Kcc20State, LateBytes, OrdinaryInput,
    PlainOutput, Scenario, keys, witness,
};
use silverscript_abi::{ArtifactValue, encode_contract_entry_sig_script};
use silverscript_lang::ast::parse_contract_ast;
use silverscript_lang::compiler::{
    CompileOptions, artifact_value_to_expr, compile_contract, sil_abi_artifact_from_compiled,
};
use token_vesting::judge::{Judged, write_record};
use token_vesting::release::{
    BENEFICIARY, CLIFF, COIN_VALUE, DEFAULT_FEE, FUNDING_VALUE, Release, STRANGER, TOTAL,
    funding_input, token_covid, vault_state, vesting_covid,
};

const FILE: &str = "review_third_parties";

const RECIPIENT: &str = "validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0),";
const AMOUNT: &str = "require(claim <= vested - released);";

/// A stranger's coin of the token family, owned by the stranger's key.
fn stranger_coin(amount: i64) -> Kcc20State {
    Kcc20State::new(keys::x_only(STRANGER), amount)
}

/// The borrowed receive of `shapes::borrow` with the vault as the receiving coin: the vault
/// leads on the borrow path (witness `01`, nobody of the covenant present), the stranger's
/// coin of 100 tokens joins as delegate and signs, and one token moves into the vault.
fn borrow_into(label: &str, vault: Kcc20State) -> Scenario {
    let mut scenario = Scenario::new(format!(
        "{label}: a stranger borrows one token into the vault, outside any release"
    ));
    let next = vec![
        vault.clone().with_amount(vault.amount + 1),
        stranger_coin(99),
    ];
    scenario.push_contract_input(
        ContractInput::new(
            vault,
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
        )
        .labelled("vault: borrow path"),
    );
    scenario.push_contract_input(
        ContractInput::new(
            stranger_coin(100),
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
        )
        .labelled("stranger: delegate"),
    );
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
    scenario
}

fn judge_borrow(scenario: Scenario) -> Judged {
    run_with(scenario, node_for(at(0)), &[])
}

/// The vault as the launch check wants it (borrowing disabled): the token program refuses.
#[test]
fn s_u1_a_stranger_cannot_borrow_into_a_vault_closed_to_borrowing() {
    let judged = judge_borrow(borrow_into("s-u1", vault_state(TOTAL)));
    expect_token_refuses(FILE, &judged, 0, "rule K-50");
}

/// A vault open to borrowing (scheme 0x01, threshold 0; `check_vault` refuses it, `s_l8`
/// shows a later coin can be one): a stranger moves it with one token, without the covenant
/// present. The vault keeps its owner and policy, gets a new outpoint and one token the
/// schedule never releases; a release prepared for the old outpoint is dead, and the
/// beneficiary has to sign again for the new one.
#[test]
fn s_u2_a_vault_open_to_borrowing_is_moved_by_a_stranger() {
    let open = vault_state(TOTAL).with_threshold(0);
    let borrowed = judge_borrow(borrow_into("s-u2 1/2", open.clone()));
    expect_accepted(FILE, &borrowed);
    let moved = TransactionOutpoint::new(borrowed.assembled.tx.id(), 0);
    assert_ne!(moved, outpoint(0x32, 0), "the vault's outpoint changed");

    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut again = Release::honest(
        "s-u2 2/2: the release signed again for the moved vault (one token more in it)",
        state,
        TOTAL + 1,
        claim,
        lock_time,
    );
    again.vault = open.with_amount(TOTAL + 1);
    again.vault_outpoint = moved;
    expect_accepted(FILE, &run(&again));
}

/// The signature script of input 0 of an honest release, cut into its pushes: the
/// signature, the claim, the dispatch tag, the redeem script.
fn honest_release() -> Release {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    Release::honest(
        "s-u3: the honest first release, as signed",
        state,
        TOTAL,
        claim,
        lock_time,
    )
}

/// The release with the claim push of input 0 rewritten by `rewrite` after signing.
fn relayed(release: &Release, name: &str, rewrite: fn(i64) -> Vec<u8>) -> Judged {
    let claim = release.claim;
    let mut scenario = release.scenario();
    scenario.name = name.to_string();
    let InputSpec::Ordinary(input) = &mut scenario.inputs[0] else {
        panic!("input 0 is the vesting coin");
    };
    let original = std::mem::replace(&mut input.signature_script, LateBytes::empty());
    input.signature_script = original.map(move |script| {
        let mut parts = split_pushes(&script).expect("a push-only script");
        assert_eq!(parts.len(), 4, "signature, claim, tag, redeem script");
        assert_eq!(parts[1], push_number(claim).expect("a number"));
        parts[1] = rewrite(claim);
        parts.concat()
    });
    run_scenario(release, scenario, node_for(release.lock_time))
}

/// The harmless half: the same claim written in longer push forms. The node accepts the copy
/// under the same transaction id, with the same outputs and signatures; only the size grows.
#[test]
fn s_u3_a_relayer_re_encodes_the_claim_and_nothing_changes_but_the_size() {
    let release = honest_release();
    let honest = run(&release);
    expect_accepted(FILE, &honest);
    for (name, rewrite) in [
        (
            "s-u3: the claim padded to 8 bytes by a relayer",
            (|claim: i64| {
                let mut bytes = push_number(claim).expect("a number")[1..].to_vec();
                bytes.resize(8, 0);
                push_as(PushForm::Direct, &bytes).expect("fits")
            }) as fn(i64) -> Vec<u8>,
        ),
        (
            "s-u3: the claim in the OpPushData1 form by a relayer",
            |claim: i64| {
                let bytes = push_number(claim).expect("a number")[1..].to_vec();
                push_as(PushForm::PushData1, &bytes).expect("fits")
            },
        ),
    ] {
        let copy = relayed(&release, name, rewrite);
        expect_accepted(FILE, &copy);
        assert_eq!(copy.assembled.tx.id(), honest.assembled.tx.id(), "same id");
        assert_eq!(copy.assembled.tx.outputs, honest.assembled.tx.outputs);
        assert_ne!(
            copy.assembled.tx.inputs[0].signature_script,
            honest.assembled.tx.inputs[0].signature_script
        );
        let size = |judged: &Judged| {
            judged
                .submit
                .verdict
                .masses
                .as_ref()
                .map(|masses| masses.estimated_size)
        };
        println!("{name}: size {:?} -> {:?}", size(&honest), size(&copy));
        assert!(size(&copy) > size(&honest));
    }
}

/// The other half: the relayer writes another claim. The signature still holds (it does
/// not cover the call) but the outputs it covers no longer match the claim.
#[test]
fn s_u4_a_relayer_rewrites_the_claim_and_the_contract_refuses() {
    // The honest claim is all that is vested, so one more stops at the amount rule first.
    let release = honest_release();
    for (name, rewrite, statement) in [
        (
            "s-u4: the claim lowered by one by a relayer",
            (|claim: i64| push_number(claim - 1).expect("a number")) as fn(i64) -> Vec<u8>,
            RECIPIENT,
        ),
        (
            "s-u4: the claim raised by one by a relayer",
            |claim: i64| push_number(claim + 1).expect("a number"),
            AMOUNT,
        ),
    ] {
        let judged = relayed(&release, name, rewrite);
        expect_vesting_refuses(FILE, &judged, statement);
    }
}

/// An ordinary pay-to-script-hash coin, no covenant id, spendable whenever an input of the
/// covenant `owner` is in the transaction: the same check the token program makes for an
/// owner of scheme 0x04 (`OpCovInputCount(owner) > 0`), in a coin of its own.
const DELEGATED: &str = r#"pragma silverscript ^0.1.0;

contract Delegated(byte[32] init_owner) {
    byte[32] owner = init_owner;

    entry spend() {
        require(OpCovInputCount(owner) > 0);
    }
}
"#;

fn delegated_coin(owner: [u8; 32], at_outpoint: TransactionOutpoint) -> OrdinaryInput {
    let ast = parse_contract_ast(DELEGATED).expect("parses");
    let args = [ArtifactValue::Bytes(owner.to_vec())];
    let exprs: Vec<_> = args
        .iter()
        .zip(&ast.params)
        .map(|(value, param)| artifact_value_to_expr(value, &param.type_ref, &ast).expect("expr"))
        .collect();
    let compiled =
        compile_contract(DELEGATED, &exprs, CompileOptions::default()).expect("compiles");
    let abi = sil_abi_artifact_from_compiled(&compiled, &exprs).expect("abi");
    let mut signature_script =
        encode_contract_entry_sig_script(&abi, "Delegated", "spend", &[]).expect("encodes");
    signature_script.extend(push_redeem(&compiled.bytecode).expect("fits"));
    OrdinaryInput::new(
        pay_to_script_hash_script(&compiled.bytecode),
        COIN_VALUE,
        at_outpoint,
        LateBytes::fixed(signature_script),
    )
    .labelled("a coin that trusts the presence of the vesting covenant")
}

/// The honest release also spends such a coin and sends its KAS to the beneficiary: the
/// vesting contract lets any ordinary input in, so the presence of the covenant is the
/// beneficiary's to lend. Without the vesting coin the same coin does not open.
#[test]
fn s_u5_a_coin_that_trusts_the_presence_of_the_covenant_belongs_to_the_beneficiary() {
    let release = honest_release();
    let mut scenario = release.scenario();
    scenario.name =
        "s-u5 1/2: the honest release also takes a coin delegated to the covenant".to_string();
    scenario.push_ordinary_input(delegated_coin(
        vesting_covid().as_bytes(),
        outpoint(0xc1, 0),
    ));
    let last = scenario.outputs.len() - 1;
    scenario.outputs.insert(
        last,
        kcc20_family::scenario::OutputSpec::Plain(PlainOutput::new(
            pay_to_public_key_script(&keys::x_only(BENEFICIARY)),
            COIN_VALUE,
        )),
    );
    let judged = run_with(
        scenario,
        node_for(release.lock_time),
        &[(0, release.compiled())],
    );
    expect_accepted(FILE, &judged);

    let mut alone = Scenario::new("s-u5 2/2: the delegated coin without the vesting coin");
    alone.push_ordinary_input(delegated_coin(
        vesting_covid().as_bytes(),
        outpoint(0xc2, 0),
    ));
    alone.push_ordinary_input(funding_input(outpoint(0xc3, 0)));
    alone.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(STRANGER)),
        COIN_VALUE + FUNDING_VALUE - DEFAULT_FEE,
    ));
    let judged = run_with(alone, node_for(at(0)), &[]);
    show(&judged);
    let ok = judged.refused() && !judged.inputs[0].accepted;
    write_record(
        FILE,
        &judged.record(
            "refused: the delegated coin's own rule, no input of the covenant",
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    assert!(ok);
}
