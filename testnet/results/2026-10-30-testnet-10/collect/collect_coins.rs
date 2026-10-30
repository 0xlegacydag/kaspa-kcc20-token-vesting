//! A one-off: one KCC20 transfer that turns the beneficiary's three token coins of the run of
//! 2026-10-15 (3, 3 and 4 tokens) into one coin of 10 tokens owned by Alice, for the run of
//! 2026-10-30. The freed deposit pays the fee, the rest goes to the payer. It holds the
//! beneficiary's key only, judges the transaction by the node rules of `node-gate` and writes
//! it. Nothing is sent. It is not one of the programs of `testnet/`: it is kept with the record
//! of that run, and it builds as a test file of this crate, copied to `tests/`. Run it with
//! COLLECT_REQUEST, COLLECT_OUTPUT, COLLECT_BENEFICIARY_KEY_FILE and KCC20_REFERENCE_DIR set;
//! the key is not published.

use std::fs;
use std::path::Path;

use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::tx::{
    ComputeCommit, CovenantBinding, ScriptPublicKey, TransactionOutpoint,
};
use kaspa_hashes::Hash;
use kcc20_family::shapes::pay_to_public_key_script;
use kcc20_family::{
    Arg, Assembled, Call, ContractInput, ContractOutput, Entry, EntryRole, Family, Kcc20State,
    PlainOutput, Scenario, keys, witness,
};
use node_gate::GateParams;
use secp256k1::SecretKey;
use serde_json::{Value, json};
use token_vesting::judge::judge;
use token_vesting::release::BENEFICIARY;

const COIN_VALUE: u64 = 100_000_000;
const FEE_ROUNDS: usize = 6;
const FEERATE: u64 = 200;

fn hex(bytes: &[u8]) -> String {
    let mut text = vec![0u8; bytes.len() * 2];
    faster_hex::hex_encode(bytes, &mut text).expect("twice the length");
    String::from_utf8(text).expect("hex is text")
}

fn hex32(value: &Value, field: &str) -> [u8; 32] {
    let text = value[field].as_str().unwrap_or_else(|| panic!("{field}"));
    let mut bytes = [0u8; 32];
    faster_hex::hex_decode(text.as_bytes(), &mut bytes).unwrap_or_else(|_| panic!("{field}"));
    bytes
}

fn load_key(path: &Path) -> SecretKey {
    let inside = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("the crate's folder");
    let file = path.canonicalize().expect("the key file cannot be found");
    assert!(!file.starts_with(inside), "the key file lies in the crate");
    let text = fs::read_to_string(&file).expect("the key file cannot be read");
    let text = text.trim();
    assert_eq!(text.len(), 64, "the key file does not hold 64 characters");
    let mut bytes = [0u8; 32];
    faster_hex::hex_decode(text.to_ascii_lowercase().as_bytes(), &mut bytes)
        .unwrap_or_else(|_| panic!("the key file is not hex"));
    SecretKey::from_slice(&bytes).unwrap_or_else(|_| panic!("the key file holds no valid key"))
}

#[test]
fn collect() {
    let Some(request_path) = std::env::var_os("COLLECT_REQUEST") else {
        println!("COLLECT_REQUEST is not set: nothing to do");
        return;
    };
    let output_path = std::env::var_os("COLLECT_OUTPUT").expect("COLLECT_OUTPUT");
    let key_path = std::env::var_os("COLLECT_BENEFICIARY_KEY_FILE").expect("the key file");
    assert!(
        !Path::new(&output_path).exists(),
        "the output exists already"
    );
    let request: Value =
        serde_json::from_str(&fs::read_to_string(&request_path).expect("the request"))
            .expect("the request is JSON");

    keys::register_key(BENEFICIARY, load_key(Path::new(&key_path)));
    let beneficiary = hex32(&request, "beneficiary");
    assert_eq!(
        keys::x_only(BENEFICIARY),
        beneficiary,
        "the key file holds another key than the beneficiary's"
    );
    let alice = hex32(&request, "alice");
    let payer = hex32(&request, "payer");
    let covenant = Hash::from_bytes(hex32(&request, "covenant_id"));
    let daa_score = request["virtual_daa_score"].as_u64().expect("the score");
    let gate = GateParams::testnet10().with_daa_score(daa_score);

    let coins = request["coins"].as_array().expect("coins");
    assert_eq!(coins.len(), 3, "three coins");
    let total: i64 = coins
        .iter()
        .map(|coin| coin["amount"].as_i64().expect("amount"))
        .sum();
    assert_eq!(total, 10, "the coins hold 10 tokens");
    let value_in: u64 = coins
        .iter()
        .map(|coin| coin["value"].as_u64().expect("value"))
        .sum();
    let target = Kcc20State::new(alice, total);

    let build = |fee: u64| -> Assembled {
        let mut scenario = Scenario::new("collect");
        for (index, coin) in coins.iter().enumerate() {
            let state = Kcc20State::new(beneficiary, coin["amount"].as_i64().expect("amount"));
            assert_eq!(
                Family::pinned()
                    .script_public_key(&state.clone().into())
                    .expect("a token state has a script")
                    .script(),
                faster_hex_vec(coin["script_public_key"].as_str().expect("script")),
                "the node's script of coin {index} is the lock of its state"
            );
            let call = if index == 0 {
                Call::typed(
                    Entry::Transfer,
                    vec![
                        Arg::States(vec![target.clone()]),
                        Arg::Bytes(witness::owner_key(
                            BENEFICIARY,
                            EntryRole::Leader,
                            SIG_HASH_ALL,
                        )),
                    ],
                )
            } else {
                Call::typed(
                    Entry::TransferDelegator,
                    vec![Arg::Bytes(witness::owner_key(
                        BENEFICIARY,
                        EntryRole::Delegate,
                        SIG_HASH_ALL,
                    ))],
                )
            };
            let mut input = ContractInput::new(
                state,
                coin["value"].as_u64().expect("value"),
                Some(covenant),
                TransactionOutpoint::new(
                    Hash::from_bytes(hex32(coin, "transaction_id")),
                    coin["index"].as_u64().expect("index") as u32,
                ),
                call,
            )
            .labelled("a token coin of the beneficiary");
            input.block_daa_score = coin["block_daa_score"].as_u64().expect("score");
            scenario.push_contract_input(input);
        }
        scenario.push_contract_output(ContractOutput::new(
            target.clone(),
            COIN_VALUE,
            Some(CovenantBinding::new(0, covenant)),
        ));
        let change = value_in - COIN_VALUE - fee;
        scenario.push_plain_output(PlainOutput::new(pay_to_public_key_script(&payer), change));
        scenario.assemble(Family::pinned()).expect("assembled")
    };

    let mut fee = 0u64;
    let mut settled = None;
    for _ in 0..FEE_ROUNDS {
        let assembled = build(fee);
        let storage =
            node_gate::mass::calculated_storage_mass(&gate.params, &assembled.tx, &assembled.utxos)
                .expect("a storage mass")
                .expect("a storage mass");
        assembled.tx.set_storage_mass(storage);
        let judged = judge(&gate, assembled, &[]);
        assert!(judged.budgets_measured, "the budgets could not be measured");
        let masses = judged.submit.verdict.masses.as_ref().expect("masses");
        let policy = judged.submit.verdict.policy.as_ref().expect("policy");
        let storage_mass = masses.calculated_storage_mass.expect("storage mass");
        let fee_mass = policy.fee_mass.expect("fee mass");
        let wanted = FEERATE * fee_mass.max(storage_mass);
        println!(
            "fee {fee}: compute mass {}, transient mass {}, storage mass {storage_mass}, fee mass {fee_mass}, floor {:?}, wanted {wanted}",
            masses.compute_mass, masses.transient_mass, policy.minimum_fee
        );
        if wanted == fee {
            settled = Some(judged);
            break;
        }
        fee = wanted;
    }
    let judged = settled.expect("the fee did not settle");
    assert!(
        judged.accepted(),
        "not accepted: submission {:?}; block {:?}; inputs {:?}",
        judged.submit.verdict.first_refusal,
        judged.block.verdict.first_refusal,
        judged.inputs
    );
    for input in &judged.inputs {
        println!(
            "input {}: accepted {}, script units {}, budget {}",
            input.index, input.accepted, input.used_script_units, input.budget
        );
    }
    let tx = &judged.assembled.tx;

    let inputs: Vec<Value> = tx
        .inputs
        .iter()
        .zip(coins)
        .map(|(input, coin)| {
            let ComputeCommit::ComputeBudget(budget) = input.compute_commit else {
                panic!("an input commits a signature operation count");
            };
            json!({
                "transaction_id": hex(&input.previous_outpoint.transaction_id.as_bytes()),
                "index": input.previous_outpoint.index,
                "sequence": input.sequence,
                "sig_op_count": 0,
                "compute_budget": budget.0,
                "signature_script": hex(&input.signature_script),
                "spends": coin,
            })
        })
        .collect();
    let script_text = |script: &ScriptPublicKey| hex(script.script());
    let outputs: Vec<Value> = tx
        .outputs
        .iter()
        .map(|output| {
            json!({
                "value": output.value,
                "script_public_key_version": output.script_public_key.version(),
                "script_public_key": script_text(&output.script_public_key),
                "covenant_binding": output.covenant.map(|binding| json!({
                    "authorizing_input": binding.authorizing_input,
                    "covenant_id": hex(&binding.covenant_id.as_bytes()),
                })),
            })
        })
        .collect();
    let entry = json!({
        "format": "collect-one-off",
        "nothing_was_sent": true,
        "judged_at_daa": daa_score,
        "transaction_id": hex(&tx.id().as_bytes()),
        "version": tx.version,
        "lock_time": tx.lock_time,
        "subnetwork_id": hex(tx.subnetwork_id.as_ref()),
        "gas": tx.gas,
        "payload": hex(&tx.payload),
        "mass": tx.storage_mass(),
        "fee": fee,
        "inputs": inputs,
        "outputs": outputs,
        "token_state": {
            "contract": "KCC20",
            "amount": target.amount,
            "owner": hex(&target.owner),
            "owner_scheme": target.owner_scheme,
            "borrow_scheme": target.borrow_scheme,
            "borrow_guard": hex(&target.borrow_guard),
            "extension_commitment": hex(&target.extension_commitment),
        },
    });
    let text = serde_json::to_string_pretty(&entry).expect("JSON");
    fs::write(&output_path, format!("{text}\n")).expect("the output cannot be written");
    println!(
        "accepted by scripts, on submission and in a block at {daa_score}; id {}; fee {fee} sompi; written",
        hex(&tx.id().as_bytes())
    );
}

fn faster_hex_vec(text: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; text.len() / 2];
    faster_hex::hex_decode(text.as_bytes(), &mut bytes).expect("hex");
    bytes
}
