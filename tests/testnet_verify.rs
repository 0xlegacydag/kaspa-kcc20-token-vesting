//! Checks a chain file of the vesting run (`vesting-launch-chain/1`) from the file alone.
//!
//! When `VESTING_VERIFY_CHAIN` names a chain file: for every output of every move that
//! carries a state, the lock is computed again from that state: a TokenVesting output by
//! compiling the contract for its state (`Compiled::new`), a KCC20 output with the pinned
//! KCC20 program of `kcc20-family`. The covenant id of the vesting coin is derived again from
//! A1 by the consensus rule, the token coin's script from its stated state, and the template
//! hash of the token program from the pinned family. One line per check, OK or MISMATCH; any
//! mismatch fails the run. A move without outputs is a mismatch.
//!
//! When `VESTING_VERIFY_REPORT` names a file that does not exist yet, and every check
//! passed, a report (`vesting-verify-report/2`) is written there: the SHA-256 of the chain
//! file and every vesting and token output whose lock was computed again. On a mismatch
//! nothing is written.
//!
//! Without `VESTING_VERIFY_CHAIN`, the self-test: the chain file of the run of 2026-10-15
//! passes, and the same file changed in memory, one thing at a time, does not.
//!
//! This program judges locks, not transactions: it runs no script and reads no signature or
//! compute budget. The builder judges those, also on a file that exists
//! (`VESTING_JUDGE_CHAIN`, `tests/testnet_chain.rs`).
//!
//! educational / unaudited / not for real funds

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{ScriptPublicKey, TransactionOutpoint, TransactionOutput};
use kaspa_hashes::Hash;
use kcc20_family::shapes::pay_to_public_key_script;
use kcc20_family::{Contract, Family, Kcc20State};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use token_vesting::contract::{Compiled, VestingState};

const CHAIN_ENV: &str = "VESTING_VERIFY_CHAIN";
const CHAIN_FORMAT: &str = "vesting-launch-chain/1";
/// Environment variable: where to write the report for the sender (must not exist yet).
const REPORT_ENV: &str = "VESTING_VERIFY_REPORT";
const REPORT_FORMAT: &str = "vesting-verify-report/2";

fn hex(bytes: &[u8]) -> String {
    faster_hex::hex_string(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let mut bytes = vec![0u8; text.len() / 2];
    faster_hex::hex_decode(text.as_bytes(), &mut bytes).ok()?;
    Some(bytes)
}

fn hex32(value: &Value) -> Result<[u8; 32], String> {
    value
        .as_str()
        .and_then(unhex)
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| format!("not 32 bytes of hex: {value}"))
}

fn byte(value: &Value) -> Result<u8, String> {
    value
        .as_u64()
        .and_then(|number| u8::try_from(number).ok())
        .ok_or_else(|| format!("not a byte: {value}"))
}

fn int(value: &Value) -> Result<i64, String> {
    value
        .as_i64()
        .ok_or_else(|| format!("not a whole number: {value}"))
}

fn script_of(object: &Value) -> Result<ScriptPublicKey, String> {
    let version = object["script_public_key_version"]
        .as_u64()
        .and_then(|version| u16::try_from(version).ok())
        .ok_or("no script version")?;
    let script = object["script_public_key"]
        .as_str()
        .and_then(unhex)
        .ok_or("no script")?;
    Ok(ScriptPublicKey::from_vec(version, script))
}

fn kcc20(state: &Value) -> Result<Kcc20State, String> {
    Ok(Kcc20State {
        amount: int(&state["amount"])?,
        owner: hex32(&state["owner"])?,
        owner_scheme: byte(&state["owner_scheme"])?,
        borrow_scheme: byte(&state["borrow_scheme"])?,
        borrow_guard: hex32(&state["borrow_guard"])?,
        extension_commitment: hex32(&state["extension_commitment"])?,
    })
}

fn vesting(state: &Value) -> Result<VestingState, String> {
    Ok(VestingState {
        beneficiary: hex32(&state["beneficiary"])?,
        token_covid: hex32(&state["token_covid"])?,
        token_template: hex32(&state["token_template"])?,
        start: int(&state["start"])?,
        cliff: int(&state["cliff"])?,
        duration: int(&state["duration"])?,
        total: int(&state["total"])?,
        released: int(&state["released"])?,
    })
}

/// The owner of a KCC20 state is one of the parties' keys (owner scheme 0) or the vesting
/// covenant id (owner scheme 4); anything else is a mismatch.
fn owner_is_known(chain: &Value, state: &Kcc20State) -> Result<bool, String> {
    match state.owner_scheme {
        0 => {
            let mut keys = Vec::new();
            for party in ["payer", "alice", "beneficiary"] {
                keys.push(hex32(&chain["parties"][party]["x_only_public_key"])?);
            }
            Ok(keys.contains(&state.owner))
        }
        4 => Ok(state.owner == hex32(&chain["vesting_covenant_id"])?),
        _ => Ok(false),
    }
}

/// Collects the lines and counts the mismatches.
#[derive(Default)]
struct Report {
    lines: Vec<String>,
    mismatches: usize,
    /// Every vesting and token output whose lock was computed again, for the report file.
    outputs: Vec<Value>,
}

impl Report {
    fn check(&mut self, what: String, ok: bool) {
        if !ok {
            self.mismatches += 1;
        }
        self.lines.push(format!(
            "{} {what}",
            if ok { "OK      " } else { "MISMATCH" }
        ));
    }

    /// A check whose inputs could not even be read is a mismatch with the reason.
    fn result(&mut self, what: String, outcome: Result<bool, String>) {
        match outcome {
            Ok(ok) => self.check(what, ok),
            Err(reason) => self.check(format!("{what}: {reason}"), false),
        }
    }
}

fn verify(chain: &Value) -> Report {
    let mut report = Report::default();
    let family = Family::pinned();
    let template = family.template_hash(Contract::Kcc20);
    report.check(
        format!("format is {CHAIN_FORMAT}"),
        chain["format"] == CHAIN_FORMAT,
    );
    report.result(
        "token.template_hash is the template hash of the pinned KCC20 program".to_string(),
        hex32(&chain["token"]["template_hash"]).map(|stated| stated == template),
    );
    let token_covid = hex32(&chain["token"]["covenant_id"]);
    let vesting_covid = hex32(&chain["vesting_covenant_id"]);

    // The token coin: its script is the pinned KCC20 script of its stated state.
    report.result(
        "token.coin script is the KCC20 script of token.state, owned by Alice's key".to_string(),
        (|| {
            let state = kcc20(&chain["token"]["state"])?;
            let alice = hex32(&chain["parties"]["alice"]["x_only_public_key"])?;
            let owned = state.owner_scheme == 0 && state.owner == alice;
            let script = family
                .script_public_key(&state.into())
                .map_err(|error| error.to_string())?;
            Ok(owned && script == script_of(&chain["token"]["coin"])?)
        })(),
    );
    // The parties: each script is the pay-to-public-key script of its key.
    for party in ["payer", "alice", "beneficiary"] {
        report.result(
            format!("parties.{party} script is the pay-to-public-key script of its key"),
            (|| {
                let key = hex32(&chain["parties"][party]["x_only_public_key"])?;
                let stated = chain["parties"][party]["script_public_key"]
                    .as_str()
                    .ok_or("no script")?;
                Ok(hex(pay_to_public_key_script(&key).script()) == stated)
            })(),
        );
    }

    let moves = chain["moves"].as_array().cloned().unwrap_or_default();
    report.check("the chain has moves".to_string(), !moves.is_empty());

    // The vesting covenant id, derived again from A1: the outpoint of the input that
    // authorizes the outputs bound to it, and those outputs in order.
    if let Some(a1) = moves.first() {
        report.result(
            format!(
                "vesting_covenant_id is derived from {} by the consensus rule",
                a1["name"].as_str().unwrap_or("?")
            ),
            (|| {
                let stated = vesting_covid.clone()?;
                let outputs = a1["outputs"].as_array().ok_or("no outputs")?;
                let mut group = Vec::new();
                let mut authorizing = None;
                for (index, output) in outputs.iter().enumerate() {
                    let binding = &output["covenant_binding"];
                    if binding.is_null() || hex32(&binding["covenant_id"])? != stated {
                        continue;
                    }
                    let input = binding["authorizing_input"]
                        .as_u64()
                        .ok_or("no authorizing input")?;
                    if authorizing.is_some_and(|known| known != input) {
                        return Err("two authorizing inputs".to_string());
                    }
                    authorizing = Some(input);
                    group.push((
                        index as u32,
                        TransactionOutput::new(
                            output["value"].as_u64().ok_or("no value")?,
                            script_of(output)?,
                        ),
                    ));
                }
                let input = &a1["inputs"][authorizing.ok_or("no output carries the id")? as usize];
                let outpoint = TransactionOutpoint::new(
                    Hash::from_bytes(hex32(&input["transaction_id"])?),
                    input["index"]
                        .as_u64()
                        .and_then(|index| u32::try_from(index).ok())
                        .ok_or("no index")?,
                );
                let derived = covenant_id(
                    outpoint,
                    group.iter().map(|(index, output)| (*index, output)),
                );
                Ok(derived.as_bytes() == stated && group.len() == 1)
            })(),
        );
    }

    // Every output of every move.
    for entry in &moves {
        let name = entry["name"].as_str().unwrap_or("?");
        let outputs = entry["outputs"].as_array().cloned().unwrap_or_default();
        if outputs.is_empty() {
            report.check(format!("{name} has outputs"), false);
        }
        for (index, output) in outputs.iter().enumerate() {
            let role = output["role"].as_str().unwrap_or("?");
            let what = format!("{name} output {index} ({role})");
            let outcome = (|| -> Result<bool, String> {
                let stated = script_of(output)?;
                let binding = &output["covenant_binding"];
                match role {
                    "vesting" => {
                        let state = vesting(&output["state"])?;
                        let lock = Compiled::new(&state).script_public_key();
                        let bound = hex32(&binding["covenant_id"])? == vesting_covid.clone()?;
                        let names = state.token_template == template
                            && state.token_covid == token_covid.clone()?
                            && state.beneficiary
                                == hex32(&chain["parties"]["beneficiary"]["x_only_public_key"])?;
                        Ok(lock == stated
                            && bound
                            && names
                            && output["state"]["contract"] == "TokenVesting")
                    }
                    "token" => {
                        let state = kcc20(&output["state"])?;
                        let lock = family
                            .script_public_key(&state.clone().into())
                            .map_err(|error| error.to_string())?;
                        let bound = hex32(&binding["covenant_id"])? == token_covid.clone()?;
                        let owned = owner_is_known(chain, &state)?;
                        Ok(lock == stated
                            && bound
                            && owned
                            && output["state"]["contract"] == "KCC20")
                    }
                    "change" => {
                        // Change goes to the payer, and only to the payer's key.
                        let key = hex32(&chain["parties"]["payer"]["x_only_public_key"])?;
                        Ok(output["to"] == "payer"
                            && binding.is_null()
                            && output["state"].is_null()
                            && pay_to_public_key_script(&key) == stated)
                    }
                    other => Err(format!("an unknown role `{other}`")),
                }
            })();
            if role == "vesting" || role == "token" {
                report.outputs.push(json!({
                    "move": name,
                    "output": index,
                    "role": role,
                    "script_public_key": output["script_public_key"],
                    "ok": outcome == Ok(true),
                }));
            }
            report.result(what, outcome);
        }
    }
    report
}

/// The self-test: the chain file of the run passes, and each change to it is caught at the
/// line named. Nothing is written.
fn self_test() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testnet/results/2026-10-15-testnet-10/chain.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let chain: Value = serde_json::from_str(&text).expect("the chain file of the run is JSON");
    let clean = verify(&chain);
    assert_eq!(clean.mismatches, 0, "{:#?}", clean.lines);
    println!(
        "the chain file of the run: {} checks, 0 mismatches",
        clean.lines.len()
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    type Change = fn(&mut Value);
    let changes: [(&str, &str, Change); 13] = [
        ("the format", "format is", |chain| {
            chain["format"] = json!("vesting-launch-chain/0");
        }),
        ("the template hash", "token.template_hash", |chain| {
            chain["token"]["template_hash"] = json!("11".repeat(32));
        }),
        (
            "the vesting covenant id",
            "vesting_covenant_id is derived",
            |chain| chain["vesting_covenant_id"] = json!("11".repeat(32)),
        ),
        (
            "the token's covenant id",
            "R1 first release output 0 (token)",
            |chain| chain["token"]["covenant_id"] = json!("11".repeat(32)),
        ),
        ("a party's key", "parties.payer script", |chain| {
            chain["parties"]["payer"]["x_only_public_key"] = json!("11".repeat(32));
        }),
        (
            "what the vesting coin has released",
            "R1 first release output 2 (vesting)",
            |chain| chain["moves"][6]["outputs"][2]["state"]["released"] = json!(4),
        ),
        (
            "the schedule of a vesting coin",
            "A1 vesting genesis output 0 (vesting)",
            |chain| chain["moves"][0]["outputs"][0]["state"]["cliff"] = json!(1),
        ),
        (
            "the tokens of a claim",
            "R1 first release output 0 (token)",
            |chain| chain["moves"][6]["outputs"][0]["state"]["amount"] = json!(9),
        ),
        (
            "the owner of a claim",
            "R1 first release output 0 (token)",
            |chain| chain["moves"][6]["outputs"][0]["state"]["owner"] = json!("11".repeat(32)),
        ),
        (
            "the script of the change",
            "R3 the rest output 1 (change)",
            |chain| chain["moves"][8]["outputs"][1]["script_public_key"] = json!("00"),
        ),
        ("a role", "an unknown role", |chain| {
            chain["moves"][0]["outputs"][1]["role"] = json!("other");
        }),
        (
            "a move's outputs taken away",
            "B3 more than vested has outputs",
            |chain| {
                chain["moves"][4]
                    .as_object_mut()
                    .expect("a move")
                    .remove("outputs");
            },
        ),
        (
            "a move's outputs emptied",
            "R3 the rest has outputs",
            |chain| chain["moves"][8]["outputs"] = json!([]),
        ),
    ];
    for (what, line, change) in changes {
        let mut copy = chain.clone();
        change(&mut copy);
        let report = verify(&copy);
        let caught = report
            .lines
            .iter()
            .find(|found| found.starts_with("MISMATCH") && found.contains(line))
            .unwrap_or_else(|| panic!("{what}: no mismatch at `{line}`: {:#?}", report.lines));
        println!("changed {what}: {caught}");
    }
    println!("self-test: as intended");
}

fn main() {
    let Some(path) = std::env::var_os(CHAIN_ENV).filter(|value| !value.is_empty()) else {
        self_test();
        return;
    };
    // The report file for the sender: it must not exist yet.
    let report_path = std::env::var_os(REPORT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(report_path) = &report_path {
        assert!(
            !report_path.exists(),
            "{REPORT_ENV}: {} exists already; it is not overwritten",
            report_path.display()
        );
    }
    let bytes =
        fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.to_string_lossy()));
    let chain: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("{}: not JSON: {error}", path.to_string_lossy()));
    let report = verify(&chain);
    println!("verify {}", path.to_string_lossy());
    for line in &report.lines {
        println!("{line}");
    }
    println!(
        "{} checks, {} mismatches",
        report.lines.len(),
        report.mismatches
    );
    assert_eq!(report.mismatches, 0, "the chain file does not verify");
    // Written only when every check passed.
    if let Some(report_path) = report_path {
        assert!(report.outputs.iter().all(|output| output["ok"] == true));
        let document = json!({
            "format": REPORT_FORMAT,
            "chain_sha256": sha256_hex(&bytes),
            "vesting_covenant_id": chain["vesting_covenant_id"],
            "mismatches": 0,
            "outputs": report.outputs,
        });
        let text = serde_json::to_string_pretty(&document).expect("json") + "\n";
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&report_path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .unwrap_or_else(|error| panic!("{}: {error}", report_path.display()));
        println!(
            "report written to {} ({} outputs)",
            report_path.display(),
            report.outputs.len()
        );
    }
}
