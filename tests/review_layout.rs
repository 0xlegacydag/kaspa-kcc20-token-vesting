//! The seven-field layout of a KCC20 coin, checked against the reference's own source
//! `fixtures/public-mint/sil/KCC20.sil` (kcc20-reference c8a0871, the checkout named by
//! `KCC20_REFERENCE_DIR`), not against the `kcc20-family` crate alone.
//!
//! - `s_y1`: the reference source, compiled here with the pinned SilverScript compiler for a
//!   state with a distinct value in every field, gives byte for byte the coin script the
//!   checks spend; and its seven state fields have the names and types, in order, of the
//!   `KCC20State` struct of TokenVesting.
//! - `s_y2`, `s_y3`: swapping two fields of the struct (two bytes; two 32-byte fields) makes
//!   the honest release fail, so the order is observed, not only declared.

mod common;

use common::{at, expect_accepted, expect_vesting_refuses, run, schedule};
use kcc20_family::states::{BORROW_AMOUNT_THRESHOLD, OWNER_COVENANT_ID};
use kcc20_family::{Contract, Family, Kcc20State};
use silverscript_abi::ArtifactValue;
use silverscript_lang::ast::{TypeRef, parse_contract_ast};
use silverscript_lang::compiler::{CompileOptions, artifact_value_to_expr, compile_contract};
use token_vesting::contract::SOURCE;
use token_vesting::release::{CLIFF, Release, TOTAL};

/// The reference's KCC20 source, read from the reference checkout.
fn reference_source() -> String {
    let dir = std::env::var_os("KCC20_REFERENCE_DIR")
        .expect("KCC20_REFERENCE_DIR names the kcc20-reference checkout at c8a0871");
    let path = std::path::Path::new(&dir).join("fixtures/public-mint/sil/KCC20.sil");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn type_name(type_ref: &TypeRef) -> String {
    format!("{type_ref:?}")
}

#[test]
fn s_y1_the_reference_source_compiles_to_the_coins_of_the_checks_and_matches_the_struct() {
    let source = reference_source();
    let ast = parse_contract_ast(&source).expect("the reference source parses");

    // A state with a distinct value in every field.
    let state = Kcc20State::new([0x0b; 32], 123_456_789)
        .with_owner(OWNER_COVENANT_ID, [0x0c; 32])
        .with_borrow(BORROW_AMOUNT_THRESHOLD, [0x0d; 32])
        .with_extension([0x0e; 32]);
    let template = Family::pinned().template_hash(Contract::Kcc20);
    let args = [
        ArtifactValue::Bytes(template.to_vec()),
        ArtifactValue::Int(state.amount),
        ArtifactValue::Bytes(state.owner.to_vec()),
        ArtifactValue::Byte(state.owner_scheme),
        ArtifactValue::Byte(state.borrow_scheme),
        ArtifactValue::Bytes(state.borrow_guard.to_vec()),
        ArtifactValue::Bytes(state.extension_commitment.to_vec()),
    ];
    assert_eq!(ast.params.len(), args.len(), "seven constructor parameters");
    let exprs: Vec<_> = args
        .iter()
        .zip(&ast.params)
        .map(|(value, param)| {
            artifact_value_to_expr(value, &param.type_ref, &ast).expect("an argument")
        })
        .collect();
    let compiled = compile_contract(&source, &exprs, CompileOptions::default())
        .expect("the reference source compiles with SilverScript 3ed9733");
    let family = Family::pinned()
        .redeem_script(&state.clone().into())
        .expect("a token state has a redeem script");
    println!(
        "reference KCC20.sil compiled here: {} bytes; the family's coin script: {} bytes",
        compiled.bytecode.len(),
        family.len()
    );
    assert_eq!(
        compiled.bytecode, family,
        "the coin the checks spend is the reference source, compiled"
    );

    // The state fields of the reference, in order, against the struct of TokenVesting.
    let reference: Vec<(String, String)> = ast
        .fields
        .iter()
        .map(|field| (field.name.clone(), type_name(&field.type_ref)))
        .collect();
    let vesting = parse_contract_ast(SOURCE).expect("the contract parses");
    let ours: Vec<(String, String)> = vesting
        .structs
        .iter()
        .find(|item| item.name == "KCC20State")
        .expect("TokenVesting declares KCC20State")
        .fields
        .iter()
        .map(|field| (field.name.clone(), type_name(&field.type_ref)))
        .collect();
    println!("reference fields: {reference:?}");
    println!("TokenVesting KCC20State: {ours:?}");
    assert_eq!(reference.len(), 7);
    assert_eq!(reference.len(), ours.len());
    for ((their_name, their_type), (our_name, our_type)) in reference.iter().zip(&ours) {
        assert_eq!(their_type, our_type, "{their_name} / {our_name}");
        assert_eq!(
            their_name.trim_start_matches("gen__"),
            our_name,
            "same field, same place"
        );
    }
}

/// The honest first release, with a vault whose extension commitment is not zero, so that
/// the two 32-byte fields at the end of the state differ.
fn release_with_extension(name: &str, source: String) -> Release {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(name, state, TOTAL, claim, lock_time);
    let extension = [0xee; 32];
    release.vault = release.vault.clone().with_extension(extension);
    for output in &mut release.token_outputs {
        *output = output.clone().with_extension(extension);
    }
    release.source = source;
    release
}

fn swapped(first: &str, second: &str) -> String {
    let marker = "@@";
    let source = SOURCE.replacen(first, marker, 1);
    let source = source.replacen(second, first, 1);
    source.replacen(marker, second, 1)
}

#[test]
fn s_y2_owner_scheme_and_borrow_scheme_swapped_in_the_struct() {
    let release = release_with_extension(
        "s-y2: the contract reads owner_scheme and borrow_scheme in swapped places",
        swapped(
            "        byte owner_scheme;\n",
            "        byte borrow_scheme;\n",
        ),
    );
    expect_vesting_refuses(
        "review_layout",
        &run(&release),
        "require(vault.owner_scheme == OWNER_COVENANT_ID);",
    );
}

#[test]
fn s_y3_borrow_guard_and_extension_commitment_swapped_in_the_struct() {
    let honest = release_with_extension(
        "s-y3 1/2: the honest release with a non-zero extension commitment",
        SOURCE.to_string(),
    );
    expect_accepted("review_layout", &run(&honest));
    let release = release_with_extension(
        "s-y3 2/2: the contract reads borrow_guard and extension_commitment in swapped places",
        swapped(
            "        byte[32] borrow_guard;\n",
            "        byte[32] extension_commitment;\n",
        ),
    );
    expect_vesting_refuses(
        "review_layout",
        &run(&release),
        "validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0),",
    );
}
