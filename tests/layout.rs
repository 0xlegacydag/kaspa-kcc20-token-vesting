//! The state of a KCC20 coin of the Argent reference has seven fields, not the six of the
//! KCC-20 text: the compiler-owned `gen__kcc20_template` comes first. Shown on the engine:
//! the honest release passes with the seven-field layout (see `honest.rs`) and is refused
//! with the six-field one.

mod common;

use common::{at, run, schedule, show};
use token_vesting::judge::write_record;
use token_vesting::release::{CLIFF, Release, TOTAL};

const FILE: &str = "layout";

/// The contract with the six fields of the KCC-20 text and no template field.
fn six_field_source() -> String {
    let source = token_vesting::contract::SOURCE;
    let source = source.replace("        byte[32] kcc20_template;\n", "");
    let source = source.replace(
        "                kcc20_template: vault.kcc20_template,\n",
        "",
    );
    let source = source.replace(
        "                    kcc20_template: vault.kcc20_template,\n",
        "",
    );
    assert!(
        !source.contains("kcc20_template"),
        "every use of the field is gone"
    );
    source
}

#[test]
fn the_six_field_layout_of_the_kcc20_text_reads_the_reference_coin_wrongly() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(
        "layout: the honest first release, contract with the six-field token layout",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    release.source = six_field_source();
    let judged = run(&release);
    show(&judged);
    let ok = judged.refused() && !judged.inputs[0].accepted && judged.inputs[1].accepted;
    write_record(
        FILE,
        &judged.record(
            "refused at the vesting input; the token input passes",
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    assert!(ok);
}

#[test]
fn the_seven_field_layout_passes() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let release = Release::honest(
        "layout: the honest first release, contract with the seven-field token layout",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    let judged = run(&release);
    show(&judged);
    let ok = judged.accepted();
    write_record(
        FILE,
        &judged.record(
            "accepted",
            if ok { "as expected" } else { "NOT AS EXPECTED" },
        ),
    );
    assert!(ok);
}
