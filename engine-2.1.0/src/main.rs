//! The second run of the TokenVesting checks: every transaction the checks built on the
//! engine of rusty-kaspa a41a333 (the revision the KCC-20 reference pins), replayed byte for
//! byte on the script engine and the mass calculator of the rusty-kaspa 2.1.0 packages
//! published on crates.io.
//!
//! Only the engine, the covenant context it runs in, and the masses are replayed. The rules
//! around them (lock-time finality, fees, mempool policy) are `node-gate`'s port of a41a333
//! and have no port to 2.1.0; a case refused there is compared on its scripts only. A
//! transaction whose covenant context is refused runs no script on either side, and both
//! records name that refusal.
//!
//! Usage: `cargo run --frozen --release -- <dump file>` where the dump file is the
//! `cases.jsonl` written by the main checks with `VESTING_DUMP_DIR` set. Exits with 1 when
//! any transaction is judged differently.

use kaspa_consensus_core::config::params::TESTNET_PARAMS;
use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::mass::MassCalculator;
use kaspa_consensus_core::mass::units::{ComputeBudget, Gram};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    ComputeCommit, CovenantBinding, PopulatedTransaction, ScriptPublicKey, Transaction,
    TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::{EngineCtx, EngineFlags, TxScriptEngine};
use serde_json::Value;
use std::process::ExitCode;

fn bytes(value: &Value) -> Vec<u8> {
    let text = value.as_str().expect("hex text");
    let mut out = vec![0u8; text.len() / 2];
    faster_hex::hex_decode(text.as_bytes(), &mut out).expect("valid hex");
    out
}

fn hash(value: &Value) -> Hash {
    Hash::from_slice(&bytes(value))
}

fn number(value: &Value) -> u64 {
    value.as_u64().expect("a number")
}

fn transaction(tx: &Value) -> Transaction {
    let inputs = tx["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| {
            TransactionInput::new_with_mass(
                TransactionOutpoint::new(hash(&input["txid"]), number(&input["index"]) as u32),
                bytes(&input["signature_script"]),
                number(&input["sequence"]),
                ComputeCommit::ComputeBudget(ComputeBudget(number(&input["budget"]) as u16)),
            )
        })
        .collect();
    let outputs = tx["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|output| {
            let script = ScriptPublicKey::from_vec(
                number(&output["spk_version"]) as u16,
                bytes(&output["spk"]),
            );
            let binding = (!output["covenant_id"].is_null()).then(|| {
                CovenantBinding::new(
                    number(&output["authorizing_input"]) as u16,
                    hash(&output["covenant_id"]),
                )
            });
            TransactionOutput::with_covenant(number(&output["value"]), script, binding)
        })
        .collect();
    let subnetwork: [u8; 20] = bytes(&tx["subnetwork_id"]).try_into().unwrap();
    Transaction::new(
        number(&tx["version"]) as u16,
        inputs,
        outputs,
        number(&tx["lock_time"]),
        SubnetworkId::from_bytes(subnetwork),
        number(&tx["gas"]),
        bytes(&tx["payload"]),
    )
}

fn utxos(list: &Value) -> Vec<UtxoEntry> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            UtxoEntry::new(
                number(&entry["amount"]),
                ScriptPublicKey::from_vec(
                    number(&entry["spk_version"]) as u16,
                    bytes(&entry["spk"]),
                ),
                number(&entry["block_daa_score"]),
                entry["is_coinbase"].as_bool().unwrap(),
                (!entry["covenant_id"].is_null()).then(|| hash(&entry["covenant_id"])),
            )
        })
        .collect()
}

fn main() -> ExitCode {
    let path = std::env::args().nth(1).expect("the dump file");
    let text = std::fs::read_to_string(&path).expect("the dump file reads");
    let params = TESTNET_PARAMS;
    let flags = EngineFlags {
        sigop_script_units: Gram(params.mass_per_sig_op).into(),
    };
    let calculator = MassCalculator::new_with_consensus_params(&params);
    let (mut same, mut differ) = (0usize, 0usize);
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let case: Value = serde_json::from_str(line).expect("a JSON line");
        let name = case["name"].as_str().unwrap_or("?").to_string();
        let tx = transaction(&case["tx"]);
        let id_match = tx.id().to_string() == case["tx_id"].as_str().unwrap_or("");
        let entries = utxos(&case["utxos"]);
        let populated = PopulatedTransaction::new(&tx, entries.clone());
        let mut answers = Vec::new();
        match CovenantsContext::from_tx(&populated) {
            Err(error) => answers.push(format!("covenant context refused: {error}")),
            Ok(covenants) => {
                let cache = Cache::new(10_000);
                let reused = SigHashReusedValuesUnsync::new();
                for (index, (input, entry)) in tx.inputs.iter().zip(&entries).enumerate() {
                    let ctx = EngineCtx::new(&cache)
                        .with_reused(&reused)
                        .with_covenants_ctx(&covenants);
                    let mut vm = TxScriptEngine::from_transaction_input_with_script_units_limit(
                        &populated,
                        input,
                        index,
                        entry,
                        ctx,
                        flags,
                        input.compute_commit.allowed_script_units(),
                    );
                    let result = vm.execute();
                    answers.push(match result {
                        Ok(()) => format!(
                            "input {index}: accepted, {} units",
                            vm.used_script_units().0
                        ),
                        Err(error) => format!("input {index}: refused: {error}"),
                    });
                }
            }
        }
        let expected: Vec<String> = case["a41a333"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();
        let masses = calculator.calc_non_contextual_masses(&tx);
        let storage = calculator
            .calc_contextual_masses(&populated)
            .map(|masses| masses.storage_mass);
        let masses_now = format!(
            "compute {} transient {} storage {:?}",
            masses.compute_mass, masses.transient_mass, storage
        );
        let masses_then = case["masses"].as_str().unwrap_or("").to_string();
        let verdicts_same = answers == expected;
        let masses_same = masses_now == masses_then;
        let ok = verdicts_same && masses_same && id_match;
        if ok {
            same += 1;
        } else {
            differ += 1;
        }
        println!("{} | {}", if ok { "same     " } else { "DIFFERENT" }, name);
        if !ok {
            println!("    id the same: {id_match}");
            println!("    a41a333: {expected:?} | {masses_then}");
            println!("    2.1.0:   {answers:?} | {masses_now}");
        }
    }
    println!(
        "\n{} transactions: {same} judged the same on 2.1.0, {differ} differently",
        same + differ
    );
    if differ == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
