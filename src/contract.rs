//! The TokenVesting contract: its source, its state, the compiled script and the bytes of a
//! call.
//!
//! The contract is compiled with the SilverScript compiler the KCC-20 reference pins
//! (3ed9733, v1.0.0), always with debug information, so that a refusal at a byte of the
//! script can be traced to a line of the source.

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use silverscript_abi::{ArtifactValue, SilAbiArtifact, encode_contract_entry_sig_script};
use silverscript_lang::ast::{ExprKind, parse_contract_ast};
use silverscript_lang::compiler::{
    CompileOptions, artifact_value_to_expr, compile_contract, sil_abi_artifact_from_compiled,
};
use silverscript_lang::debug_info::StepKind;

/// The source of the contract.
pub const SOURCE: &str = include_str!("../contracts/TokenVesting.sil");

/// The source the checks run: [`SOURCE`], or the file named by `VESTING_SOURCE_FILE` (the
/// mutation runner sets it to a copy of the contract with one rule taken out).
pub fn active_source() -> String {
    match std::env::var_os("VESTING_SOURCE_FILE") {
        Some(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.to_string_lossy())),
        None => SOURCE.to_string(),
    }
}

/// An `int constant` of the contract in [`SOURCE`], as the compiler parses it: the launch
/// check takes its bounds from here, so the contract and the check share one place.
pub fn int_constant(name: &str) -> i64 {
    let ast = parse_contract_ast(SOURCE).expect("the contract source parses");
    ast.constants
        .iter()
        .find(|constant| constant.name == name)
        .and_then(|constant| match constant.expr.kind {
            ExprKind::Int(value) => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the contract has no int constant `{name}`"))
}

/// The name of the contract in its source.
pub const CONTRACT_NAME: &str = "TokenVesting";

/// The one entry.
pub const RELEASE: &str = "release";

/// The state of a vesting coin, in the order of the constructor parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VestingState {
    /// The beneficiary's x-only public key: signs every release and receives the tokens.
    pub beneficiary: [u8; 32],
    /// Covenant id of the token family.
    pub token_covid: [u8; 32],
    /// Template hash of the token program.
    pub token_template: [u8; 32],
    /// DAA score at which the schedule starts.
    pub start: i64,
    /// DAA units after `start` before the first release.
    pub cliff: i64,
    /// DAA units after `start` at which everything is vested.
    pub duration: i64,
    /// Tokens the schedule releases in all.
    pub total: i64,
    /// Tokens released so far.
    pub released: i64,
}

impl VestingState {
    /// The constructor arguments, in the order of the source.
    pub fn constructor_args(&self) -> Vec<ArtifactValue> {
        vec![
            ArtifactValue::Bytes(self.beneficiary.to_vec()),
            ArtifactValue::Bytes(self.token_covid.to_vec()),
            ArtifactValue::Bytes(self.token_template.to_vec()),
            ArtifactValue::Int(self.start),
            ArtifactValue::Int(self.cliff),
            ArtifactValue::Int(self.duration),
            ArtifactValue::Int(self.total),
            ArtifactValue::Int(self.released),
        ]
    }

    /// The same state with another released amount.
    pub fn with_released(mut self, released: i64) -> Self {
        self.released = released;
        self
    }

    /// Tokens vested at lock time `now`, by the formula of the contract, computed in i128.
    pub fn vested_at(&self, now: i64) -> i64 {
        let elapsed = now - self.start;
        if elapsed >= self.duration {
            return self.total;
        }
        (i128::from(self.total) * i128::from(elapsed) / i128::from(self.duration)) as i64
    }
}

/// One statement of the source and the bytes it was compiled to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// First byte of the statement's code in the script.
    pub start: usize,
    /// One past its last byte.
    pub end: usize,
    /// First line of the statement, from 1.
    pub line: u32,
    /// Last line of the statement.
    pub end_line: u32,
}

/// The contract compiled for one state.
#[derive(Clone, Debug)]
pub struct Compiled {
    /// The source it was compiled from.
    pub source: String,
    /// The script: template prefix, state, template suffix.
    pub bytecode: Vec<u8>,
    /// The portable ABI: entries, dispatch tags, state span.
    pub abi: SilAbiArtifact,
    /// The statements, from the compiler's debug information.
    pub steps: Vec<Step>,
}

/// A line of the source, located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLine {
    /// First line of the statement, from 1.
    pub line: u32,
    /// The statement's text, its lines trimmed and joined by one space.
    pub text: String,
}

impl Compiled {
    /// The contract of [`SOURCE`] for a state.
    pub fn new(state: &VestingState) -> Self {
        Self::from_source(SOURCE, state).expect("the contract source compiles")
    }

    /// Any variant of the source for a state, with debug information.
    pub fn from_source(source: &str, state: &VestingState) -> Result<Self, String> {
        let ast = parse_contract_ast(source).map_err(|error| error.to_string())?;
        let args = state.constructor_args();
        if ast.params.len() != args.len() {
            return Err(format!(
                "the source has {} constructor parameters, the state {}",
                ast.params.len(),
                args.len()
            ));
        }
        let exprs = args
            .iter()
            .zip(&ast.params)
            .map(|(value, param)| {
                artifact_value_to_expr(value, &param.type_ref, &ast)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        let options = CompileOptions {
            record_debug_infos: true,
            ..Default::default()
        };
        let compiled =
            compile_contract(source, &exprs, options).map_err(|error| error.to_string())?;
        let abi =
            sil_abi_artifact_from_compiled(&compiled, &exprs).map_err(|error| error.to_string())?;
        let steps = compiled
            .debug_info
            .as_ref()
            .map(|info| {
                info.steps
                    .iter()
                    .filter(|step| matches!(step.kind, StepKind::Source {}))
                    .map(|step| Step {
                        start: step.bytecode_start,
                        end: step.bytecode_end,
                        line: step.span.line,
                        end_line: step.span.end_line,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            source: source.to_string(),
            bytecode: compiled.bytecode.clone(),
            abi,
            steps,
        })
    }

    /// The pay-to-script-hash lock of a coin with this script.
    pub fn script_public_key(&self) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.bytecode)
    }

    /// The bytes of a `release` call in front of the redeem script.
    pub fn release_call(&self, signature: &[u8], claim: i64) -> Vec<u8> {
        encode_contract_entry_sig_script(
            &self.abi,
            CONTRACT_NAME,
            RELEASE,
            &[
                ArtifactValue::Bytes(signature.to_vec()),
                ArtifactValue::Int(claim),
            ],
        )
        .expect("the codec encodes a 65-byte signature and an int")
    }

    /// The whole signature script of a `release` call: the call, then the redeem script.
    pub fn release_signature_script(&self, signature: &[u8], claim: i64) -> Vec<u8> {
        let mut script = self.release_call(signature, claim);
        script.extend(
            kcc20_family::raw::push_redeem(&self.bytecode)
                .expect("the redeem script fits a signature script"),
        );
        script
    }

    /// The innermost statement whose code covers the byte at `offset` of the script.
    pub fn line_at(&self, offset: usize) -> Option<SourceLine> {
        let step = self
            .steps
            .iter()
            .filter(|step| step.start <= offset && offset < step.end)
            .min_by_key(|step| step.end - step.start)?;
        let first = step.line.saturating_sub(1) as usize;
        let count = (step.end_line.saturating_sub(step.line) + 1) as usize;
        let text = self
            .source
            .lines()
            .skip(first)
            .take(count)
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ");
        Some(SourceLine {
            line: step.line,
            text,
        })
    }
}
