//! The launch check (`token_vesting::launch`) and the ways a coin with the vesting covenant's
//! id could appear.
//!
//! - `s_l1` to `s_l5`: `check_vesting_genesis` against launches built to fool it, each judged
//!   by the node's rules too.
//! - `s_l6`, `s_l7`: `check_parameters` and `check_vault` must refuse launches whose every
//!   release the contract refuses: coins with parameters beyond the contract's bounds, a vault
//!   of another token family with the state the check wants.
//! - `s_l8`: a coin of the token family sent to the vesting covenant after the launch is
//!   never checked; the contract releases from it whatever its borrow policy.

mod common;

use common::{
    at, expect_accepted, expect_consensus_refuses, expect_token_refuses, expect_vesting_refuses,
    node_for, run, run_with, schedule, show,
};
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, TransactionOutpoint, TransactionOutput,
};
use kaspa_hashes::Hash;
use kcc20_family::scenario::{Call, OutputSpec};
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::{
    Arg, Assembled, ContractInput, ContractOutput, Entry, Family, Kcc20State, LateBytes,
    OrdinaryInput, PlainOutput, Scenario, keys,
};
use node_gate::GateParams;
use node_gate::verdict::Stage;
use token_vesting::contract::{Compiled, VestingState};
use token_vesting::judge::{Judged, judge, write_record};
use token_vesting::launch::{LaunchError, check_parameters, check_vault, check_vesting_genesis};
use token_vesting::release::{
    BENEFICIARY, CLIFF, COIN_VALUE, DEFAULT_FEE, FUNDING_VALUE, OTHER_TOKEN_COVID, Release, TOTAL,
    funding_input, token_covid, vault_state, vesting_covid,
};

const FILE: &str = "review_launch";

/// One output of a launch: its script, and the group it belongs to. Outputs of one group
/// share the authorizing input and the covenant id, which is hashed from that input's
/// outpoint and all outputs of the group, as the node does
/// (rusty-kaspa a41a333 crypto/txscript/src/covenants.rs:137-160).
#[derive(Clone)]
struct Out {
    script: ScriptPublicKey,
    /// `(authorizing input, group number)`, or none for a plain output.
    group: Option<(u16, u8)>,
}

/// The ids of the groups of a launch, in the order of `groups`.
struct Launch {
    assembled: Assembled,
    ids: Vec<((u16, u8), Hash)>,
}

impl Launch {
    fn id(&self, group: (u16, u8)) -> Hash {
        self.ids
            .iter()
            .find(|(key, _)| *key == group)
            .map(|(_, id)| *id)
            .expect("the group exists")
    }
}

/// A launch from the funder's coins at `funding`: every output holds one KAS, the last one
/// is the funder's change. `overwrite` may replace the id written on an output after the ids
/// are computed: `(output index, id)`.
fn launch(
    name: &str,
    funding: &[TransactionOutpoint],
    outs: &[Out],
    overwrite: Option<(usize, Hash)>,
) -> Launch {
    let build = |ids: &[((u16, u8), Hash)]| {
        let mut scenario = Scenario::new(name);
        for point in funding {
            scenario.push_ordinary_input(funding_input(*point));
        }
        for (index, out) in outs.iter().enumerate() {
            let mut output = PlainOutput::new(out.script.clone(), COIN_VALUE);
            output.binding = out.group.map(|group| {
                let id = ids
                    .iter()
                    .find(|(key, _)| *key == group)
                    .map(|(_, id)| *id)
                    .unwrap_or(Hash::from_bytes([0; 32]));
                let id = match overwrite {
                    Some((at, forced)) if at == index => forced,
                    _ => id,
                };
                CovenantBinding::new(group.0, id)
            });
            scenario.push_plain_output(output);
        }
        let used: u64 = scenario.outputs.iter().map(OutputSpec::value).sum();
        scenario.push_plain_output(PlainOutput::new(
            pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
            FUNDING_VALUE * funding.len() as u64 - used - DEFAULT_FEE,
        ));
        scenario.assemble(Family::pinned()).expect("assembles")
    };
    let draft = build(&[]);
    let mut groups: Vec<(u16, u8)> = outs.iter().filter_map(|out| out.group).collect();
    groups.dedup();
    groups.sort();
    groups.dedup();
    let ids: Vec<((u16, u8), Hash)> = groups
        .iter()
        .map(|&group| {
            let members: Vec<(u32, &TransactionOutput)> = outs
                .iter()
                .enumerate()
                .filter(|(_, out)| out.group == Some(group))
                .map(|(index, _)| (index as u32, &draft.tx.outputs[index]))
                .collect();
            (
                group,
                covenant_id(funding[group.0 as usize], members.into_iter()),
            )
        })
        .collect();
    Launch {
        assembled: build(&ids),
        ids,
    }
}

fn judge_launch(launch: &Launch) -> Judged {
    judge(&GateParams::testnet10(), launch.assembled.clone(), &[])
}

fn vesting_script() -> ScriptPublicKey {
    Compiled::new(&schedule()).script_public_key()
}

fn key_script() -> ScriptPublicKey {
    pay_to_public_key_script(&keys::x_only(BENEFICIARY))
}

/// The beneficiary spends the key coin at output `index` of `launch` (carrying covenant id
/// `id`) beside a vault owned by `vault_owner`, taking the whole vault before the cliff: the
/// move of `g2` in `tests/genesis.rs`. `label` starts the name of the case.
fn take_the_vault_with(
    label: &str,
    launch: &Launch,
    index: u32,
    id: Hash,
    vault_owner: Hash,
) -> Judged {
    let mut scenario = Scenario::new(format!(
        "{label}: the beneficiary's key coin with id {id} takes a vault owned by {vault_owner}"
    ));
    let coin = launch.assembled.tx.outputs[index as usize].clone();
    scenario.push_ordinary_input(
        OrdinaryInput::new(
            coin.script_public_key.clone(),
            coin.value,
            TransactionOutpoint::new(launch.assembled.tx.id(), index),
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
        vault_state(TOTAL).with_owner(
            kcc20_family::states::OWNER_COVENANT_ID,
            vault_owner.as_bytes(),
        ),
        COIN_VALUE,
        Some(token_covid()),
        outpoint(0xa1, 0),
        Call::typed(
            Entry::Transfer,
            vec![
                Arg::States(vec![everything.clone()]),
                Arg::Bytes(LateBytes::fixed(vec![0x00])),
            ],
        ),
    ));
    scenario.push_ordinary_input(funding_input(outpoint(0xa2, 0)));
    scenario.push_contract_output(ContractOutput::new(
        everything,
        COIN_VALUE,
        Some(CovenantBinding::new(1, token_covid())),
    ));
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        FUNDING_VALUE + COIN_VALUE - DEFAULT_FEE,
    ));
    run_with(scenario, node_for(at(0)), &[])
}

/// A coin with the vesting id placed BEFORE the vesting coin: the check still counts both.
#[test]
fn s_l1_a_second_coin_of_the_id_placed_before_the_vesting_coin() {
    let launch = launch(
        "s-l1: genesis, output 0 a key coin and output 1 the vesting coin, one group",
        &[outpoint(0x90, 0)],
        &[
            Out {
                script: key_script(),
                group: Some((0, 0)),
            },
            Out {
                script: vesting_script(),
                group: Some((0, 0)),
            },
        ],
        None,
    );
    expect_accepted(FILE, &judge_launch(&launch));
    let answer = check_vesting_genesis(&launch.assembled.tx, 1, &schedule());
    println!("{answer:?}");
    assert_eq!(
        answer,
        Err(LaunchError::MoreThanOneCoin {
            outputs: vec![0, 1]
        })
    );
}

/// One input starts two covenants: the vesting coin in one group, the key coin in another.
/// The ids differ, the check passes, and the key coin does not open the vault.
#[test]
fn s_l2_one_input_two_groups_the_other_id_does_not_open_the_vault() {
    let launch = launch(
        "s-l2: genesis, one input, the vesting coin and a key coin in two groups",
        &[outpoint(0x90, 0)],
        &[
            Out {
                script: vesting_script(),
                group: Some((0, 0)),
            },
            Out {
                script: key_script(),
                group: Some((0, 1)),
            },
        ],
        None,
    );
    expect_accepted(FILE, &judge_launch(&launch));
    let vesting_id = launch.id((0, 0));
    let other_id = launch.id((0, 1));
    assert_ne!(vesting_id, other_id);
    assert_eq!(
        check_vesting_genesis(&launch.assembled.tx, 0, &schedule()),
        Ok(vesting_id)
    );
    let judged = take_the_vault_with("s-l2", &launch, 1, other_id, vesting_id);
    expect_token_refuses(FILE, &judged, 1, "OpCovInputCount(gen__glob_owner) > 0");
}

/// Two genesis drafts from the same coin: one with the vesting coin alone, one with an extra
/// key coin. Only one can be mined; their ids differ, so the key coin of the other draft does
/// not open a vault owned by the first one's id.
#[test]
fn s_l3_a_second_genesis_from_the_same_coin_has_another_id() {
    let honest = launch(
        "s-l3 1/2: genesis with the vesting coin alone",
        &[outpoint(0x90, 0)],
        &[Out {
            script: vesting_script(),
            group: Some((0, 0)),
        }],
        None,
    );
    let other = launch(
        "s-l3 2/2: another genesis from the same coin, with a key coin in the group",
        &[outpoint(0x90, 0)],
        &[
            Out {
                script: vesting_script(),
                group: Some((0, 0)),
            },
            Out {
                script: key_script(),
                group: Some((0, 0)),
            },
        ],
        None,
    );
    let honest_id = honest.id((0, 0));
    let other_id = other.id((0, 0));
    assert_ne!(honest_id, other_id);
    assert_eq!(
        check_vesting_genesis(&honest.assembled.tx, 0, &schedule()),
        Ok(honest_id)
    );
    let judged = take_the_vault_with("s-l3", &other, 1, other_id, honest_id);
    expect_token_refuses(FILE, &judged, 1, "OpCovInputCount(gen__glob_owner) > 0");
}

/// The vesting id written on an output bound to ANOTHER input of the genesis: the node
/// refuses (the group of that input hashes to another id), and so does the check.
#[test]
fn s_l4_the_vesting_id_on_an_output_bound_to_another_input() {
    let first = launch(
        "s-l4: the vesting coin bound to input 0, a key coin bound to input 1",
        &[outpoint(0x90, 0), outpoint(0x91, 0)],
        &[
            Out {
                script: vesting_script(),
                group: Some((0, 0)),
            },
            Out {
                script: key_script(),
                group: Some((1, 0)),
            },
        ],
        None,
    );
    let vesting_id = first.id((0, 0));
    let forged = launch(
        "s-l4: the key coin bound to input 1 carries the vesting id of input 0's group",
        &[outpoint(0x90, 0), outpoint(0x91, 0)],
        &[
            Out {
                script: vesting_script(),
                group: Some((0, 0)),
            },
            Out {
                script: key_script(),
                group: Some((1, 0)),
            },
        ],
        Some((1, vesting_id)),
    );
    let judged = judge_launch(&forged);
    expect_consensus_refuses(
        FILE,
        &judged,
        Stage::UtxoContext,
        "do not correspond to the expected genesis hashing",
    );
    let answer = check_vesting_genesis(&forged.assembled.tx, 0, &schedule());
    println!("{answer:?}");
    assert!(matches!(answer, Err(LaunchError::MoreThanOneCoin { .. })));
}

/// A later transaction writes the vesting id on a new output bound to a plain input: the node
/// refuses, the id does not hash from that input.
#[test]
fn s_l5_a_later_transaction_cannot_write_the_vesting_id() {
    let mut scenario = Scenario::new(
        "s-l5: a plain coin of the beneficiary's key, a new output carrying the vesting id",
    );
    scenario.push_ordinary_input(funding_input(outpoint(0x92, 0)));
    let mut forged = PlainOutput::new(key_script(), COIN_VALUE);
    forged.binding = Some(CovenantBinding::new(0, vesting_covid()));
    scenario.push_plain_output(forged);
    scenario.push_plain_output(PlainOutput::new(
        pay_to_public_key_script(&keys::x_only(keys::FUNDER)),
        FUNDING_VALUE - COIN_VALUE - DEFAULT_FEE,
    ));
    let judged = run_with(scenario, node_for(at(0)), &[]);
    expect_consensus_refuses(
        FILE,
        &judged,
        Stage::UtxoContext,
        "do not correspond to the expected genesis hashing",
    );
}

/// `check_parameters` must refuse every coin the contract refuses at every release (the coin
/// and its vault would be dead). Checking `start + cliff < 5e11` alone is not enough: the
/// contract also demands `start <= 4e11` and `cliff <= 4e11`. Two coins inside the first bound
/// and beyond the second.
#[test]
fn s_l6_check_parameters_passes_coins_that_no_release_can_open() {
    let mut late_start = schedule();
    late_start.start = 450_000_000_000;
    let mut long_cliff = schedule();
    long_cliff.cliff = 410_000_000_000;
    let mut dead_but_passed = Vec::new();
    for (label, state, rule) in [
        (
            "s-l6 1/2: start 4.5e11, cliff 1,000",
            late_start,
            "require(start <= MAX_DAA);",
        ),
        (
            "s-l6 2/2: cliff 4.1e11",
            long_cliff,
            "require(cliff <= MAX_DAA);",
        ),
    ] {
        let passed = check_parameters(&state).is_ok();
        let lock_time = (state.start + state.cliff + 500) as u64;
        let claim = state.vested_at(lock_time as i64);
        let release = Release::honest(
            format!("{label}: the honest first release after the cliff"),
            state.clone(),
            TOTAL,
            claim,
            lock_time,
        );
        // The release is refused at the bound (the same check passes in review_mutants).
        expect_vesting_refuses(FILE, &run(&release), rule);
        println!("{label}: check_parameters passes it: {passed}");
        if passed {
            dead_but_passed.push(label);
        }
    }
    write_record(
        FILE,
        &serde_json::json!({
            "case": "s-l6: check_parameters on coins whose every release is refused",
            "expectation": "check_parameters refuses both",
            "result": if dead_but_passed.is_empty() { "as expected" } else { "NOT AS EXPECTED: passed" },
            "passed": dead_but_passed,
        }),
    );
    assert!(
        dead_but_passed.is_empty(),
        "WEAKNESS: check_parameters passes dead coins: {dead_but_passed:?}"
    );
}

/// `check_vault` must see the vault coin's covenant id (the token family) and its program,
/// not only a decoded token state: a vault of another family with the right state would
/// pass a check of the state alone, and the contract refuses every release from it.
#[test]
fn s_l7_check_vault_passes_a_vault_of_another_token_family() {
    let state: VestingState = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(
        "s-l7: the vault is a coin of another token family, with the state check_vault wants",
        state.clone(),
        TOTAL,
        claim,
        lock_time,
    );
    release.vault_covid = Hash::from_bytes(OTHER_TOKEN_COVID);
    // The check gets the coin the release spends, whole: its UTXO entry and the state it is
    // said to carry (`release.vault`, the decoded state `vault_state(TOTAL)`).
    let passed = check_vault(vesting_covid(), &state, &release.vault_coin()).is_ok();
    let judged = run(&release);
    show(&judged);
    let fooled = passed && !judged.accepted();
    write_record(
        FILE,
        &judged.record(
            "check_vault refuses a vault the contract can never release",
            if fooled {
                "NOT AS EXPECTED: check_vault passes it"
            } else {
                "as expected"
            },
        ),
    );
    assert!(
        !fooled,
        "WEAKNESS: check_vault passed a vault of another token family; the release is refused: {:?}",
        judged.vesting_text()
    );
}

/// A coin of the token family sent to the vesting covenant after the launch, open to
/// borrowing by anybody (scheme 0x01, threshold 0): nobody ran `check_vault` on it, and the
/// contract releases from it (it checks the borrow scheme of the outputs only).
#[test]
fn s_l8_a_later_coin_open_to_borrowing_is_released_from_like_the_vault() {
    let state = schedule();
    let lock_time = at(CLIFF + 500);
    let claim = state.vested_at(lock_time as i64);
    let mut release = Release::honest(
        "s-l8: release from a later coin of the family owned by the covenant, open to borrowing",
        state,
        TOTAL,
        claim,
        lock_time,
    );
    release.vault = release.vault.clone().with_threshold(0);
    assert!(check_vault(vesting_covid(), &schedule(), &release.vault_coin()).is_err());
    expect_accepted(FILE, &run(&release));
}

/// A question worth a case: with `start + cliff` at or above 5e11 a lock time in milliseconds
/// might pass `OpCheckLockTimeVerify` and vest everything at once. It does not: `tx.daa >= x`
/// is compiled with `OpWithin(0, LOCK_TIME_THRESHOLD)` in front of the opcode (silverscript
/// 3ed9733 compile/statement.rs:375-387). `l2` tries this coin with a DAA lock time only, and
/// `11a` a timestamp lock time on an ordinary coin only.
#[test]
fn s_l9_start_plus_cliff_past_the_threshold_with_a_timestamp_lock_time() {
    let mut state = schedule();
    state.start = 400_000_000_000;
    state.cliff = 100_000_000_000;
    let release = Release::honest(
        "s-l9: start 4e11 + cliff 1e11 = 5e11, a timestamp lock time, asking for everything",
        state,
        TOTAL,
        TOTAL,
        1_779_999_999_000,
    );
    let judged = common::run_scenario(&release, release.scenario(), node_for(at(CLIFF + 500)));
    expect_vesting_refuses(FILE, &judged, "require(tx.daa >= start + cliff);");
}
