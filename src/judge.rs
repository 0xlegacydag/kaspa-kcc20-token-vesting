//! Puts an assembled transaction to the node's rules (`node-gate`) and says, in
//! plain data, what refused it and where.
//!
//! Every transaction is judged twice: submitted to a mempool and inside a block. Before
//! that, every input gets the smallest compute budget that covers its own run; an input
//! that fails, fails the same way under it. When the covenant bindings are so broken that
//! nothing can be measured, every input gets [`FALLBACK_BUDGET`].

use std::io::Write;
use std::path::PathBuf;

use kaspa_consensus_core::errors::tx::TxRuleError;
use kaspa_consensus_core::mass::units::ComputeBudget;
use kaspa_consensus_core::tx::{ComputeCommit, PopulatedTransaction};
use kcc20_family::{Assembled, Contract, SourceMap};
use node_gate::budget::commit_minimal_budgets;
use node_gate::utxo::check_covenant_info;
use node_gate::verdict::Stage;
use node_gate::{Evaluation, GateOptions, GateParams, ScriptKind, StopPoint, evaluate};
use serde_json::json;

use crate::contract::{Compiled, SourceLine};

/// Budget written into every input when budgets cannot be measured.
pub const FALLBACK_BUDGET: u16 = 100;

/// Where an input stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// The input passed.
    Passed,
    /// A statement of the vesting contract.
    Vesting(SourceLine),
    /// A place in the KCC20 program, as the source map of `kcc20-family` describes it.
    Token(String),
    /// Anything else: the engine's stop, in words.
    Other(String),
}

/// The answer for one input, run under its committed budget.
#[derive(Clone, Debug)]
pub struct InputAnswer {
    /// Index of the input.
    pub index: usize,
    /// The engine accepted it.
    pub accepted: bool,
    /// The engine's error, as text.
    pub error: Option<String>,
    /// Where it stopped.
    pub place: Place,
    /// Script units used.
    pub used_script_units: u64,
    /// Committed budget.
    pub budget: u16,
}

/// What a node would say.
#[derive(Clone, Debug)]
pub struct Judged {
    /// Name of the case.
    pub name: String,
    /// The transaction as judged (budgets committed).
    pub assembled: Assembled,
    /// Submitted to a mempool.
    pub submit: Evaluation,
    /// Inside a block.
    pub block: Evaluation,
    /// The script answer of every input, from the submit evaluation.
    pub inputs: Vec<InputAnswer>,
    /// The budgets were measured (false: the fallback budget was written).
    pub budgets_measured: bool,
}

impl Judged {
    /// Accepted both as a submission and inside a block.
    pub fn accepted(&self) -> bool {
        self.submit.accepted() && self.block.accepted()
    }

    /// Refused both as a submission and inside a block.
    pub fn refused(&self) -> bool {
        !self.submit.accepted() && !self.block.accepted()
    }

    /// The stage at which the block evaluation refused, if it did.
    pub fn block_stage(&self) -> Option<Stage> {
        self.block
            .verdict
            .consensus_refusal
            .as_ref()
            .map(|refusal| refusal.stage)
    }

    /// The consensus refusal of the block evaluation, as `rule: message`.
    pub fn block_refusal(&self) -> Option<String> {
        self.block
            .verdict
            .consensus_refusal
            .as_ref()
            .map(|refusal| format!("{}: {}", refusal.rule, refusal.message))
    }

    /// The first refusal of the submission, in node order, as `stage/rule: message`.
    pub fn submit_refusal(&self) -> Option<String> {
        self.submit
            .verdict
            .first_refusal
            .as_ref()
            .map(|refusal| format!("{:?}/{}: {}", refusal.stage, refusal.rule, refusal.message))
    }

    /// The vesting statement at which input `index` stopped, if it stopped in the contract.
    pub fn vesting_line(&self, index: usize) -> Option<&SourceLine> {
        match &self.inputs.get(index)?.place {
            Place::Vesting(line) => Some(line),
            _ => None,
        }
    }

    /// The text of the vesting statement at which input 0 stopped.
    pub fn vesting_text(&self) -> Option<&str> {
        self.vesting_line(0).map(|line| line.text.as_str())
    }

    /// The description of the KCC20 place at which input `index` stopped.
    pub fn token_place(&self, index: usize) -> Option<&str> {
        match &self.inputs.get(index)?.place {
            Place::Token(text) => Some(text),
            _ => None,
        }
    }

    /// One line for the result file.
    pub fn record(&self, expectation: &str, verdict: &str) -> serde_json::Value {
        let masses = self.submit.verdict.masses.as_ref();
        json!({
            "case": self.name,
            "expectation": expectation,
            "result": verdict,
            "tx_id": self.submit.verdict.tx_id,
            "submit_accepted": self.submit.accepted(),
            "block_accepted": self.block.accepted(),
            "consensus_accepted": self.submit.verdict.consensus_accepted,
            "submit_first_refusal": self.submit_refusal(),
            "block_refusal": self.block_refusal(),
            "policy_refusal": self.submit.policy_refusal.as_ref().map(|refusal| format!("{refusal:?}")),
            "budgets_measured": self.budgets_measured,
            "inputs": self.inputs.iter().map(|input| json!({
                "index": input.index,
                "accepted": input.accepted,
                "error": input.error,
                "place": format!("{:?}", input.place),
                "used_script_units": input.used_script_units,
                "budget": input.budget,
            })).collect::<Vec<_>>(),
            "compute_mass": masses.map(|m| m.compute_mass),
            "transient_mass": masses.map(|m| m.transient_mass),
            "storage_mass": masses.and_then(|m| m.calculated_storage_mass),
            "estimated_size": masses.map(|m| m.estimated_size),
            "fee": self.submit.verdict.fee,
            "minimum_fee": self.submit.verdict.policy.as_ref().and_then(|p| p.minimum_fee),
        })
    }
}

/// Commits budgets, evaluates in both modes and locates every stop. `vesting_inputs` maps
/// an input index to the compiled vesting script that input spends.
pub fn judge(
    gate: &GateParams,
    mut assembled: Assembled,
    vesting_inputs: &[(usize, Compiled)],
) -> Judged {
    let budgets_measured =
        commit_minimal_budgets(gate, &mut assembled.tx, &assembled.utxos).is_ok();
    if !budgets_measured {
        for input in &mut assembled.tx.inputs {
            input.compute_commit = ComputeCommit::ComputeBudget(ComputeBudget(FALLBACK_BUDGET));
        }
        assembled.tx.finalize();
    }
    let submit = evaluate(gate, &assembled.tx, &assembled.utxos, GateOptions::submit());
    let block = evaluate(gate, &assembled.tx, &assembled.utxos, GateOptions::block());
    let map = SourceMap::pinned();
    let inputs = submit
        .committed_runs
        .iter()
        .map(|run| {
            let index = run.input_index;
            let budget = match assembled.tx.inputs[index].compute_commit {
                ComputeCommit::ComputeBudget(budget) => budget.0,
                ComputeCommit::SigopCount(_) => 0,
            };
            let place = match (&run.result, &run.stop) {
                (Ok(()), _) => Place::Passed,
                (Err(_), Some(StopPoint::Opcode(stop)))
                    if stop.script == ScriptKind::RedeemScript =>
                {
                    if let Some((_, compiled)) =
                        vesting_inputs.iter().find(|(input, _)| *input == index)
                    {
                        compiled
                            .line_at(stop.byte_offset)
                            .map(Place::Vesting)
                            .unwrap_or_else(|| {
                                Place::Other(format!(
                                    "vesting script byte {} ({:?}), outside any statement",
                                    stop.byte_offset, stop.opcode_name
                                ))
                            })
                    } else if assembled.inputs[index].contract() == Some(Contract::Kcc20) {
                        Place::Token(map.contract(Contract::Kcc20).describe(stop.byte_offset))
                    } else {
                        Place::Other(format!("{stop:?}"))
                    }
                }
                (Err(_), stop) => Place::Other(format!("{stop:?}")),
            };
            InputAnswer {
                index,
                accepted: run.result.is_ok(),
                error: run.error().map(|error| error.to_string()),
                place,
                used_script_units: run.used_script_units,
                budget,
            }
        })
        .collect();
    let judged = Judged {
        name: assembled.name.clone(),
        assembled,
        submit,
        block,
        inputs,
        budgets_measured,
    };
    dump(gate, &judged);
    judged
}

/// When `VESTING_DUMP_DIR` is set, appends the judged transaction, its coins and the
/// verdict of every input to `<dir>/cases.jsonl`: the input of the replay on the rusty-kaspa
/// 2.1.0 packages (`engine-2.1.0/`). A transaction whose covenant context is refused runs no
/// script; its record names that refusal instead, in the words of the replay.
fn dump(gate: &GateParams, judged: &Judged) {
    let Some(dir) = std::env::var_os("VESTING_DUMP_DIR") else {
        return;
    };
    let hex = |bytes: &[u8]| faster_hex::hex_string(bytes);
    let tx = &judged.assembled.tx;
    let inputs: Vec<serde_json::Value> = tx
        .inputs
        .iter()
        .map(|input| {
            json!({
                "txid": hex(&input.previous_outpoint.transaction_id.as_bytes()),
                "index": input.previous_outpoint.index,
                "signature_script": hex(&input.signature_script),
                "sequence": input.sequence,
                "budget": match input.compute_commit {
                    ComputeCommit::ComputeBudget(budget) => u64::from(budget.0),
                    ComputeCommit::SigopCount(_) => 0,
                },
            })
        })
        .collect();
    let outputs: Vec<serde_json::Value> = tx
        .outputs
        .iter()
        .map(|output| {
            json!({
                "value": output.value,
                "spk_version": output.script_public_key.version(),
                "spk": hex(output.script_public_key.script()),
                "authorizing_input": output.covenant.map(|binding| binding.authorizing_input),
                "covenant_id": output.covenant.map(|binding| hex(&binding.covenant_id.as_bytes())),
            })
        })
        .collect();
    let utxos: Vec<serde_json::Value> = judged
        .assembled
        .utxos
        .iter()
        .map(|entry| {
            json!({
                "amount": entry.amount,
                "spk_version": entry.script_public_key.version(),
                "spk": hex(entry.script_public_key.script()),
                "block_daa_score": entry.block_daa_score,
                "is_coinbase": entry.is_coinbase,
                "covenant_id": entry.covenant_id.map(|id| hex(&id.as_bytes())),
            })
        })
        .collect();
    // The covenant context as the gate builds it before any script runs
    // (`check_covenant_info`).
    let populated = PopulatedTransaction::new(tx, judged.assembled.utxos.clone());
    let verdicts: Vec<String> = match check_covenant_info(gate, &populated, gate.block_daa_score) {
        Err(TxRuleError::CovenantsError(error)) => {
            vec![format!("covenant context refused: {error}")]
        }
        _ => judged
            .submit
            .committed_runs
            .iter()
            .map(|run| match run.error() {
                None => format!(
                    "input {}: accepted, {} units",
                    run.input_index, run.used_script_units
                ),
                Some(error) => format!("input {}: refused: {error}", run.input_index),
            })
            .collect(),
    };
    let masses = judged.submit.verdict.masses.as_ref().map(|masses| {
        format!(
            "compute {} transient {} storage {:?}",
            masses.compute_mass, masses.transient_mass, masses.calculated_storage_mass
        )
    });
    let record = json!({
        "name": judged.name,
        "tx_id": tx.id().to_string(),
        "tx": {
            "version": tx.version,
            "inputs": inputs,
            "outputs": outputs,
            "lock_time": tx.lock_time,
            "subnetwork_id": hex(tx.subnetwork_id.as_ref()),
            "gas": tx.gas,
            "payload": hex(&tx.payload),
        },
        "utxos": utxos,
        "a41a333": verdicts,
        "masses": masses,
        "block_accepted": judged.block.accepted(),
        "submit_accepted": judged.submit.accepted(),
    });
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the dump directory can be made");
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cases.jsonl"))
        .expect("the dump file opens");
    handle
        .write_all(format!("{record}\n").as_bytes())
        .expect("the dump file takes a line");
}

/// Appends one record to `<results dir>/<file>.jsonl`. The directory is `CHECK_RESULTS_DIR`
/// or `target/check-results` of this crate.
pub fn write_record(file: &str, record: &serde_json::Value) {
    // Checks run on several threads: one line, one write, one writer at a time.
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = std::env::var_os("CHECK_RESULTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
            target.join("check-results")
        });
    std::fs::create_dir_all(&dir).expect("the results directory can be made");
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{file}.jsonl")))
        .expect("the results file opens");
    handle
        .write_all(format!("{record}\n").as_bytes())
        .expect("the results file takes a line");
}
