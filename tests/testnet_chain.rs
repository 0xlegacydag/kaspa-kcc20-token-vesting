//! The vesting run on testnet-10, built and signed for a real send.
//!
//! Nothing here can send. The builder reads a request (`vesting-launch-request/1`), builds
//! and signs the nine moves of the run (A1, A2, B1 to B4, R1 to R3), puts each one to the
//! node rules of `node-gate` at the DAA score it is meant to be sent at, runs the launch
//! checks of `token_vesting::launch` on the real coins and writes a chain file
//! (`vesting-launch-chain/1`) with a table for a reader beside it. A separate sender reads
//! the chain file, checks it by its own rules and submits.
//!
//! The run: the payer's coin creates the vesting coin (A1); Alice moves her 10-token coin
//! whole to the vesting covenant (A2); four forbidden moves are built from the coins that R1
//! spends (B1 before the cliff, B2 dated at the end of the schedule, B3 more than vested, B4
//! the claim owned by another key), each must be refused at its rule, and the same move with
//! the one wrong thing put right (its twin) must be accepted; then three releases, 3, 3 and
//! 4 tokens (R1 to R3), the last one ending the vesting coin.
//!
//! After writing, the file is read back, every transaction is built again from the file's
//! fields alone and judged again with the coins the file states: every step must still be
//! accepted and every probe refused in the same way. Only then does the chain file get its
//! final name.
//!
//! Three modes, never in one process:
//!
//! - with `VESTING_LAUNCH_INPUT` (the request), the real run: the keys come from the files
//!   named by `VESTING_PAYER_KEY_FILE`, `VESTING_ALICE_KEY_FILE` and
//!   `VESTING_BENEFICIARY_KEY_FILE` (64 hex characters each), and the chain goes to the file
//!   named by `VESTING_LAUNCH_OUTPUT`, which must not exist yet;
//! - without it, the self-test: made-up keys behind the same handles, a made-up funding coin
//!   and token coin, the chain built at fee rates 100 and 1000 and written into the folder
//!   named by `VESTING_SELFTEST_DIR` (by default `tmp/testnet-selftest` in the build's target
//!   folder); the made-up
//!   key files go to a new folder in the system's temporary folder, outside the crate as in
//!   a real run, and are removed at the end, also when a check of the self-test fails;
//! - with `VESTING_JUDGE_CHAIN` (a chain file that exists), the judging of that file: the
//!   read-back from its fields alone, with no key and no request, against the transactions
//!   of the file itself. Nothing is written.
//!
//! The builder does not run while `VESTING_DUMP_DIR` is set: the replay in `engine-2.1.0/`
//! reads the transactions the checks of the contract judged, and the builder's are not
//! among them.
//!
//! A secret key never reaches an output, a message or a file of this program.
//!
//! educational / unaudited / not for real funds

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kaspa_consensus_core::constants::LOCK_TIME_THRESHOLD;
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash_type::SIG_HASH_ALL;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    ComputeCommit, CovenantBinding, ScriptPublicKey, Transaction, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use kcc20_family::scenario::InputSpec;
use kcc20_family::shapes::pay_to_public_key_script;
use kcc20_family::states::{BORROW_DISABLED, OWNER_COVENANT_ID, OWNER_P2PK_SCHNORR};
use kcc20_family::{
    Arg, Assembled, Call, Contract, ContractInput, ContractOutput, Entry, EntryRole, Family,
    Kcc20State, LateBytes, OrdinaryInput, PlainOutput, Scenario, keys, raw, witness,
};
use node_gate::verdict::Stage;
use node_gate::{Evaluation, GateOptions, GateParams, ScriptKind, StopPoint, evaluate};
use secp256k1::{Keypair, SECP256K1, SecretKey};
use serde_json::{Value, json};
use token_vesting::contract::{Compiled, SOURCE, VestingState};
use token_vesting::judge::judge;
use token_vesting::launch::{VaultCoin, check_parameters, check_vault, check_vesting_genesis};
use token_vesting::release::{BENEFICIARY, Release};

// ---------------------------------------------------------------------------------------
// Names and numbers of the run
// ---------------------------------------------------------------------------------------

/// Environment variable: the request file. Its presence selects the real run.
const INPUT_ENV: &str = "VESTING_LAUNCH_INPUT";
/// Environment variable: a chain file that exists, to be judged again from its fields alone.
const JUDGE_ENV: &str = "VESTING_JUDGE_CHAIN";
/// Environment variable: the chain file to write.
const OUTPUT_ENV: &str = "VESTING_LAUNCH_OUTPUT";
/// Environment variables of the three key files.
const PAYER_KEY_ENV: &str = "VESTING_PAYER_KEY_FILE";
const ALICE_KEY_ENV: &str = "VESTING_ALICE_KEY_FILE";
const BENEFICIARY_KEY_ENV: &str = "VESTING_BENEFICIARY_KEY_FILE";
/// Environment variable: where the self-test writes its chains.
const SELFTEST_DIR_ENV: &str = "VESTING_SELFTEST_DIR";

const REQUEST_FORMAT: &str = "vesting-launch-request/1";
const CHAIN_FORMAT: &str = "vesting-launch-chain/1";
/// The network as the two files name it.
const NETWORK: &str = "testnet-10";

const SOMPI_PER_KAS: u64 = 100_000_000;
/// The value of every new covenant coin of the run.
const COIN_VALUE: u64 = SOMPI_PER_KAS;
/// The schedule: DAA units to the cliff and to the end, and tokens in all.
const CLIFF: i64 = 3_000;
const DURATION: i64 = 10_000;
const TOTAL: i64 = 10;
/// The moves of the run, in their order.
const MOVE_NAMES: [&str; 9] = [
    "A1 vesting genesis",
    "A2 fund the vault",
    "B1 before the cliff",
    "B2 dated at the end",
    "B3 more than vested",
    "B4 to another key",
    "R1 first release",
    "R2 second release",
    "R3 the rest",
];
/// The releases: DAA units after the start, and tokens.
const RELEASES: [(&str, i64, i64, &str); 3] = [
    ("R1 first release", CLIFF, 3, "3 tokens at the cliff"),
    (
        "R2 second release",
        6_500,
        3,
        "3 more tokens at 6500 after the start, 6 vested",
    ),
    (
        "R3 the rest",
        DURATION,
        4,
        "the 4 tokens left, at the end of the schedule; the vesting coin ends",
    ),
];
/// A move with a lock time is sent once the virtual DAA score is above this much past it.
const SEND_MARGIN: u64 = 20;
/// B2 is not sent once the virtual DAA score is this close to its lock time.
const SEND_BEFORE_MARGIN: u64 = 2_000;

/// Bounds the sender enforces, checked here as well.
const MIN_FEERATE: u64 = 100;
const MAX_FUNDING: u64 = 20 * SOMPI_PER_KAS;
const MAX_FEES: u64 = 8 * SOMPI_PER_KAS;
const MAX_MASS: u64 = 500_000;
const MAX_BUDGET: u16 = 1_000;

/// How often a fee is recalculated before the larger of the last two candidates is taken.
const FEE_ROUNDS: usize = 6;
const FEE_RULE: &str = "the fee of a step or of a twin is the fee rate times the larger of its \
                        fee mass (the mass a node charges its fee floor on) and the storage mass \
                        a node calculates for it, both read from that very transaction, \
                        recalculated until the transaction asks for the fee it was built with; \
                        a probe pays the fee of its twin";

/// The contract statements a probe is refused at.
const TIME_LINE: &str = "require(tx.daa >= start + cliff);";
const AMOUNT_LINE: &str = "require(claim <= vested - released);";
const RECIPIENT_LINE: &str =
    "validateOutputStateWithInputTemplate( OpCovOutputIdx(token_covid, 0),";
/// What a node says when a script refuses, and when a lock time has not passed.
const SCRIPTS_MESSAGE: &str = "verification failed";
/// What the engine says when `require(tx.daa >= ...)` refuses: the statement is compiled to
/// a lock-time check (OP_CHECKLOCKTIMEVERIFY), whose error is not "verification failed".
const LOCK_TIME_SCRIPT_MESSAGE: &str = "Unsatisfied lock time";
const FINALITY_MESSAGE: &str = "is not finalized";

/// The handles the three keys are registered behind, in both modes. The fixed seeds of
/// `kcc20-family` (ALICE, BOB, CAROL, DAVE, FUNDER) cannot be handles. The beneficiary uses
/// the seed the crate's `Release` signs with.
const PAYER: u8 = 0x0a;
const ALICE: u8 = 0xa1;
const BENEFICIARY_HANDLE: u8 = BENEFICIARY;

// ---------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    faster_hex::hex_string(bytes)
}

/// The bytes of a lowercase hex text; `None` when it is not hex of whole bytes.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let mut bytes = vec![0u8; text.len() / 2];
    faster_hex::hex_decode(text.as_bytes(), &mut bytes).ok()?;
    Some(bytes)
}

fn hex32(text: &str) -> Option<[u8; 32]> {
    unhex(text).and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
}

fn kas(sompi: u64) -> String {
    format!("{}.{:08}", sompi / SOMPI_PER_KAS, sompi % SOMPI_PER_KAS)
}

/// The time of the clock in UTC, to the second.
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    // The civil date of a day count (days since 1970-01-01), by eras of 400 years.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

fn x_only_of(secret_key: &SecretKey) -> [u8; 32] {
    Keypair::from_secret_key(SECP256K1, secret_key)
        .x_only_public_key()
        .0
        .serialize()
}

fn p2pk(handle: u8) -> ScriptPublicKey {
    pay_to_public_key_script(&keys::x_only(handle))
}

/// The table written next to a chain file: its name with `.txt` appended.
fn table_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".txt");
    PathBuf::from(name)
}

/// Where the chain is written before it is read back.
fn unchecked_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".unchecked");
    PathBuf::from(name)
}

// ---------------------------------------------------------------------------------------
// States as the chain file writes them
// ---------------------------------------------------------------------------------------

fn kcc20_json(state: &Kcc20State) -> Value {
    json!({
        "contract": "KCC20",
        "amount": state.amount,
        "owner": hex(&state.owner),
        "owner_scheme": state.owner_scheme,
        "borrow_scheme": state.borrow_scheme,
        "borrow_guard": hex(&state.borrow_guard),
        "extension_commitment": hex(&state.extension_commitment),
    })
}

fn vesting_json(state: &VestingState) -> Value {
    json!({
        "contract": "TokenVesting",
        "beneficiary": hex(&state.beneficiary),
        "token_covid": hex(&state.token_covid),
        "token_template": hex(&state.token_template),
        "start": state.start,
        "cliff": state.cliff,
        "duration": state.duration,
        "total": state.total,
        "released": state.released,
    })
}

fn kcc20_from_json(value: &Value) -> Result<Kcc20State, String> {
    if value["contract"] != "KCC20" {
        return Err(format!("not a KCC20 state: {value}"));
    }
    let bytes = |field: &str| {
        value[field]
            .as_str()
            .and_then(hex32)
            .ok_or_else(|| format!("the KCC20 state has no 32-byte `{field}`"))
    };
    let byte = |field: &str| {
        value[field]
            .as_u64()
            .and_then(|number| u8::try_from(number).ok())
            .ok_or_else(|| format!("the KCC20 state has no byte `{field}`"))
    };
    Ok(Kcc20State {
        amount: value["amount"]
            .as_i64()
            .ok_or("the KCC20 state has no whole `amount`")?,
        owner: bytes("owner")?,
        owner_scheme: byte("owner_scheme")?,
        borrow_scheme: byte("borrow_scheme")?,
        borrow_guard: bytes("borrow_guard")?,
        extension_commitment: bytes("extension_commitment")?,
    })
}

fn vesting_from_json(value: &Value) -> Result<VestingState, String> {
    if value["contract"] != "TokenVesting" {
        return Err(format!("not a TokenVesting state: {value}"));
    }
    let bytes = |field: &str| {
        value[field]
            .as_str()
            .and_then(hex32)
            .ok_or_else(|| format!("the vesting state has no 32-byte `{field}`"))
    };
    let int = |field: &str| {
        value[field]
            .as_i64()
            .ok_or_else(|| format!("the vesting state has no whole `{field}`"))
    };
    Ok(VestingState {
        beneficiary: bytes("beneficiary")?,
        token_covid: bytes("token_covid")?,
        token_template: bytes("token_template")?,
        start: int("start")?,
        cliff: int("cliff")?,
        duration: int("duration")?,
        total: int("total")?,
        released: int("released")?,
    })
}

// ---------------------------------------------------------------------------------------
// The keys
// ---------------------------------------------------------------------------------------

/// The three secret keys of a run. No `Debug`: a key is never shown.
struct Secrets {
    payer: SecretKey,
    alice: SecretKey,
    beneficiary: SecretKey,
}

impl Secrets {
    /// Registers the keys behind the handles of the run. A handle that already holds another
    /// key stops the process (`keys::register_key`).
    fn register(&self) {
        keys::register_key(PAYER, self.payer);
        keys::register_key(ALICE, self.alice);
        keys::register_key(BENEFICIARY_HANDLE, self.beneficiary);
        assert_eq!(keys::x_only(PAYER), x_only_of(&self.payer));
        assert_eq!(keys::x_only(ALICE), x_only_of(&self.alice));
        assert_eq!(
            keys::x_only(BENEFICIARY_HANDLE),
            x_only_of(&self.beneficiary)
        );
    }
}

/// A path in a form two paths can be compared in.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let directory = directory.canonicalize().unwrap_or(directory);
    match path.file_name() {
        Some(name) => directory.join(name),
        None => directory,
    }
}

/// Reads one key file: 64 hex characters, white space around them allowed. An error names
/// the variable and never the content of the file.
fn load_key(variable: &str, path: &Path) -> Result<SecretKey, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "{variable}: the key file cannot be read ({:?})",
            error.kind()
        )
    })?;
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{variable}: the key file does not hold 64 hex characters"
        ));
    }
    let mut bytes = [0u8; 32];
    faster_hex::hex_decode(text.to_ascii_lowercase().as_bytes(), &mut bytes)
        .map_err(|_| format!("{variable}: the key file is not hex"))?;
    SecretKey::from_slice(&bytes)
        .map_err(|_| format!("{variable}: the 32 bytes of the key file are not a valid secret key"))
}

/// Reads the three keys of a real run. No key file may lie inside this crate's folder or at
/// a path this run writes; the three keys must differ.
fn load_keys(files: &[(&str, PathBuf); 3], written: &[PathBuf]) -> Result<Secrets, String> {
    let repository = resolved(Path::new(env!("CARGO_MANIFEST_DIR")));
    for (variable, path) in files {
        let path = resolved(path);
        if path.starts_with(&repository) {
            return Err(format!(
                "{variable}: the key file lies inside the repository; keep keys outside it"
            ));
        }
        if written.iter().any(|output| resolved(output) == path) {
            return Err(format!(
                "{variable}: the key file is at a path this run writes to"
            ));
        }
    }
    let payer = load_key(files[0].0, &files[0].1)?;
    let alice = load_key(files[1].0, &files[1].1)?;
    let beneficiary = load_key(files[2].0, &files[2].1)?;
    if payer == alice || payer == beneficiary || alice == beneficiary {
        return Err(
            "two of the key files hold the same key; the three parties need three keys".to_string(),
        );
    }
    Ok(Secrets {
        payer,
        alice,
        beneficiary,
    })
}

// ---------------------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------------------

/// A coin as the node reported it.
#[derive(Clone, Debug)]
struct CoinRecord {
    outpoint: TransactionOutpoint,
    value: u64,
    script_public_key: ScriptPublicKey,
    block_daa_score: u64,
}

impl CoinRecord {
    fn json(&self) -> Value {
        json!({
            "transaction_id": hex(&self.outpoint.transaction_id.as_bytes()),
            "index": self.outpoint.index,
            "value": self.value,
            "script_public_key_version": self.script_public_key.version(),
            "script_public_key": hex(self.script_public_key.script()),
            "block_daa_score": self.block_daa_score,
        })
    }
}

/// A request: `vesting-launch-request/1`.
#[derive(Clone, Debug)]
struct Request {
    read_at_utc: String,
    node: String,
    node_version: String,
    virtual_daa_score: u64,
    coin_value: u64,
    feerate: u64,
    fee_cap_total: u64,
    start: u64,
    funding: CoinRecord,
    token_covid: [u8; 32],
    token_coin: CoinRecord,
    token_state: Kcc20State,
}

fn text_of<'a>(object: &'a Value, field: &str) -> Result<&'a str, String> {
    object[field]
        .as_str()
        .ok_or_else(|| format!("the request has no text field `{field}`"))
}

fn number_of(object: &Value, field: &str) -> Result<u64, String> {
    object[field]
        .as_u64()
        .ok_or_else(|| format!("the request has no whole number `{field}`"))
}

fn coin_of(object: &Value, what: &str) -> Result<CoinRecord, String> {
    if !object.is_object() {
        return Err(format!("the request has no `{what}`"));
    }
    let transaction_id = hex32(text_of(object, "transaction_id")?)
        .ok_or_else(|| format!("{what}.transaction_id is not 64 lowercase hex characters"))?;
    let index = u32::try_from(number_of(object, "index")?)
        .map_err(|_| format!("{what}.index does not fit an output index"))?;
    let version = u16::try_from(number_of(object, "script_public_key_version")?)
        .map_err(|_| format!("{what}.script_public_key_version does not fit a version"))?;
    let script = unhex(text_of(object, "script_public_key")?)
        .ok_or_else(|| format!("{what}.script_public_key is not lowercase hex"))?;
    Ok(CoinRecord {
        outpoint: TransactionOutpoint::new(Hash::from_bytes(transaction_id), index),
        value: number_of(object, "value")?,
        script_public_key: ScriptPublicKey::from_vec(version, script),
        block_daa_score: number_of(object, "block_daa_score")?,
    })
}

fn parse_request(text: &str) -> Result<Request, String> {
    let document: Value =
        serde_json::from_str(text).map_err(|error| format!("the request is not JSON: {error}"))?;
    let format = text_of(&document, "format")?;
    if format != REQUEST_FORMAT {
        return Err(format!(
            "the request has format `{format}`, not `{REQUEST_FORMAT}`"
        ));
    }
    let network = text_of(&document, "network")?;
    if network != NETWORK {
        return Err(format!("the request is for `{network}`, not `{NETWORK}`"));
    }
    let schedule = &document["schedule"];
    let start = number_of(schedule, "start")?;
    for (field, expected) in [("cliff", CLIFF), ("duration", DURATION), ("total", TOTAL)] {
        if schedule[field].as_i64() != Some(expected) {
            return Err(format!(
                "schedule.{field} is {}, the run is built for {expected}",
                schedule[field]
            ));
        }
    }
    let token = &document["token"];
    let token_covid = hex32(text_of(token, "covenant_id")?)
        .ok_or("token.covenant_id is not 64 lowercase hex characters")?;
    let request = Request {
        read_at_utc: text_of(&document, "read_at_utc")?.to_string(),
        node: text_of(&document, "node")?.to_string(),
        node_version: text_of(&document, "node_version")?.to_string(),
        virtual_daa_score: number_of(&document, "virtual_daa_score")?,
        coin_value: number_of(&document, "coin_value")?,
        feerate: number_of(&document, "feerate")?,
        fee_cap_total: number_of(&document, "fee_cap_total")?,
        start,
        funding: coin_of(&document["funding"], "funding")?,
        token_covid,
        token_coin: coin_of(&token["coin"], "token.coin")?,
        token_state: kcc20_from_json(&token["state"])
            .map_err(|error| format!("token.state: {error}"))?,
    };
    request.check()?;
    Ok(request)
}

impl Request {
    /// The bounds of the run that do not need the keys.
    fn check(&self) -> Result<(), String> {
        if self.coin_value != COIN_VALUE {
            return Err(format!(
                "coin_value is {}, the run needs exactly 1 KAS",
                self.coin_value
            ));
        }
        if self.feerate < MIN_FEERATE {
            return Err(format!(
                "feerate {} is below {MIN_FEERATE} sompi per gram",
                self.feerate
            ));
        }
        if self.fee_cap_total > MAX_FEES {
            return Err(format!(
                "fee_cap_total is {} KAS, more than the 8 KAS of fees the sender accepts",
                kas(self.fee_cap_total)
            ));
        }
        if self.funding.value > MAX_FUNDING {
            return Err(format!(
                "the funding coin holds {} KAS, more than the 20 KAS the sender accepts",
                kas(self.funding.value)
            ));
        }
        if self.start <= self.virtual_daa_score {
            return Err("schedule.start is not after virtual_daa_score".to_string());
        }
        let last = self.start.checked_add(DURATION as u64 + SEND_MARGIN);
        if last.is_none_or(|last| last >= LOCK_TIME_THRESHOLD) {
            return Err("the schedule leaves the DAA-score domain of lock times".to_string());
        }
        // These three go into the files of the run as they are; a file of the run holds no
        // control character and no long dash.
        for (field, text) in [
            ("read_at_utc", &self.read_at_utc),
            ("node", &self.node),
            ("node_version", &self.node_version),
        ] {
            if text
                .chars()
                .any(|c| c.is_control() || c == '\u{2013}' || c == '\u{2014}')
            {
                return Err(format!(
                    "{field} holds a control character or a long dash, which the files of a run \
                     do not carry"
                ));
            }
        }
        if self.token_coin.value != self.coin_value {
            return Err(format!(
                "the token coin holds {} KAS; it becomes the vault whole, and every new covenant \
                 coin of the run holds exactly coin_value",
                kas(self.token_coin.value)
            ));
        }
        let state = &self.token_state;
        if state.amount != TOTAL {
            return Err(format!(
                "the token coin holds {} tokens; the vault must hold exactly {TOTAL}",
                state.amount
            ));
        }
        if state.owner_scheme != OWNER_P2PK_SCHNORR || state.borrow_scheme != BORROW_DISABLED {
            return Err(
                "the token coin must be owned by a key (scheme 0) with borrowing disabled"
                    .to_string(),
            );
        }
        if self.funding.outpoint == self.token_coin.outpoint {
            return Err("the funding coin and the token coin are the same coin".to_string());
        }
        Ok(())
    }

    /// The checks that need the keys: the funding coin is the payer's pay-to-public-key coin,
    /// and the token coin's script is the pinned KCC20 script of the stated state, owned by
    /// Alice's key.
    fn check_coins(&self) -> Result<(), String> {
        if self.funding.script_public_key != p2pk(PAYER) {
            return Err(
                "the funding coin's script is not the pay-to-public-key script of the payer's key"
                    .to_string(),
            );
        }
        if self.token_state.owner != keys::x_only(ALICE) {
            return Err("the token state's owner is not Alice's key".to_string());
        }
        let script = Family::pinned()
            .script_public_key(&self.token_state.clone().into())
            .map_err(|error| format!("token.state has no KCC20 script: {error}"))?;
        if self.token_coin.script_public_key != script {
            return Err(
                "the token coin's script is not the pinned KCC20 script of the stated state"
                    .to_string(),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Coins, verdicts and fees
// ---------------------------------------------------------------------------------------

/// Where a coin comes from.
#[derive(Clone, Debug)]
enum Origin {
    Funding,
    TokenCoin,
    Move { name: String, output: u32 },
}

impl Origin {
    fn json(&self) -> Value {
        match self {
            Origin::Funding => json!("funding"),
            Origin::TokenCoin => json!("token_coin"),
            Origin::Move { name, output } => json!({ "move": name, "output": output }),
        }
    }
}

/// A coin of the run.
#[derive(Clone, Debug)]
struct Coin {
    outpoint: TransactionOutpoint,
    entry: UtxoEntry,
    origin: Origin,
    what: String,
}

/// What an output of a move is, for the file.
#[derive(Clone, Debug)]
enum Role {
    Vesting(VestingState),
    Token(Kcc20State),
    Change,
}

/// What the gate answered for one transaction at one DAA score.
struct Answer {
    tx: Transaction,
    utxos: Vec<UtxoEntry>,
    submit: Evaluation,
    block: Evaluation,
}

/// A refusal as the chain file writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Refused {
    layer: &'static str,
    input: usize,
    message: String,
    /// The statement of the vesting contract the vesting input stopped at, if it did.
    contract_line: Option<String>,
}

impl Answer {
    fn scripts(&self) -> bool {
        !self.submit.committed_runs.is_empty()
            && self.submit.committed_runs.iter().all(|run| run.is_ok())
    }

    fn consensus(&self) -> bool {
        self.submit.verdict.consensus_accepted
    }

    fn mempool(&self) -> bool {
        self.submit.accepted()
    }

    fn block(&self) -> bool {
        self.block.accepted()
    }

    fn accepted(&self) -> bool {
        self.scripts() && self.consensus() && self.mempool() && self.block()
    }

    fn refused_everywhere(&self) -> bool {
        !self.consensus() && !self.mempool() && !self.block()
    }

    fn budgets(&self) -> Vec<u16> {
        self.tx
            .inputs
            .iter()
            .map(|input| match input.compute_commit {
                ComputeCommit::ComputeBudget(budget) => budget.0,
                ComputeCommit::SigopCount(_) => 0,
            })
            .collect()
    }

    /// The statement of the vesting contract at which input `index` stopped, if it ran the
    /// vesting script given and stopped inside a statement.
    fn vesting_line(&self, index: usize, compiled: &Compiled) -> Option<String> {
        let run = self
            .submit
            .committed_runs
            .iter()
            .find(|run| run.input_index == index)?;
        if run.is_ok() {
            return None;
        }
        match &run.stop {
            Some(StopPoint::Opcode(stop)) if stop.script == ScriptKind::RedeemScript => {
                compiled.line_at(stop.byte_offset).map(|line| line.text)
            }
            _ => None,
        }
    }

    /// The consensus refusal inside a block, as the file writes it. The submission must have
    /// been refused with the same message.
    fn refusal(&self, vesting: Option<(usize, &Compiled)>) -> Result<Refused, String> {
        let block = self
            .block
            .verdict
            .consensus_refusal
            .as_ref()
            .ok_or("a block takes it")?;
        let submit = self
            .submit
            .verdict
            .first_refusal
            .as_ref()
            .ok_or("a mempool takes it")?;
        if submit.message != block.message || submit.stage != block.stage {
            return Err(format!(
                "the submission is refused otherwise than the block: {:?} `{}` against {:?} `{}`",
                submit.stage, submit.message, block.stage, block.message
            ));
        }
        let (layer, input) = match block.stage {
            Stage::Scripts => (
                "scripts",
                block.input.ok_or("a script refusal names no input")?,
            ),
            _ => ("consensus", input_named_in(&block.message).unwrap_or(0)),
        };
        let contract_line = vesting
            .filter(|(index, _)| layer == "scripts" && *index == input)
            .and_then(|(index, compiled)| self.vesting_line(index, compiled));
        Ok(Refused {
            layer,
            input,
            message: block.message.clone(),
            contract_line,
        })
    }

    fn numbers(&self) -> Option<Numbers> {
        let masses = self.submit.verdict.masses.as_ref()?;
        let policy = self.submit.verdict.policy.as_ref()?;
        Some(Numbers {
            compute_mass: masses.compute_mass,
            transient_mass: masses.transient_mass,
            storage_mass: masses.calculated_storage_mass?,
            fee_mass: policy.fee_mass?,
            fee_floor: policy.minimum_fee?,
        })
    }

    fn why_not(&self) -> String {
        format!(
            "scripts {}, consensus {}, mempool {}, block {}; submission: {:?}; block: {:?}",
            self.scripts(),
            self.consensus(),
            self.mempool(),
            self.block(),
            self.submit
                .verdict
                .first_refusal
                .as_ref()
                .map(|refusal| format!(
                    "{:?} {}: {}",
                    refusal.stage, refusal.rule, refusal.message
                )),
            self.block
                .verdict
                .consensus_refusal
                .as_ref()
                .map(|refusal| format!(
                    "{:?} {}: {}",
                    refusal.stage, refusal.rule, refusal.message
                )),
        )
    }
}

/// The input number in a message such as "transaction input #0 is not finalized".
fn input_named_in(message: &str) -> Option<usize> {
    let after = message.split('#').nth(1)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The node at a DAA score: testnet-10, the mempool's and the block's score both set to it.
fn node_at(daa_score: u64) -> GateParams {
    GateParams::testnet10().with_daa_score(daa_score)
}

/// Judges a transaction as it is: its budgets and its mass field are not touched, except
/// that a submission writes the calculated storage mass into the field, as a node does.
fn judge_as_is(gate: &GateParams, tx: Transaction, utxos: Vec<UtxoEntry>) -> Answer {
    let submit = evaluate(gate, &tx, &utxos, GateOptions::submit());
    let block = evaluate(gate, &tx, &utxos, GateOptions::block());
    Answer {
        tx,
        utxos,
        submit,
        block,
    }
}

/// Writes the storage mass a node calculates for the transaction into its mass field.
fn set_calculated_storage_mass(gate: &GateParams, assembled: &Assembled) -> Result<u64, String> {
    let mass =
        node_gate::mass::calculated_storage_mass(&gate.params, &assembled.tx, &assembled.utxos)
            .map_err(|error| format!("{}: no storage mass: {error}", assembled.name))?
            .ok_or_else(|| format!("{}: the storage mass cannot be calculated", assembled.name))?;
    assembled.tx.set_storage_mass(mass);
    Ok(mass)
}

/// The masses and the fee floor of one evaluated transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Numbers {
    compute_mass: u64,
    transient_mass: u64,
    storage_mass: u64,
    fee_mass: u64,
    fee_floor: u64,
}

impl Numbers {
    fn charged_mass(&self) -> u64 {
        self.fee_mass.max(self.storage_mass)
    }

    fn json(&self) -> Value {
        json!({
            "compute_mass": self.compute_mass,
            "transient_mass": self.transient_mass,
            "calculated_storage_mass": self.storage_mass,
            "fee_mass": self.fee_mass,
        })
    }
}

/// A transaction judged by `token_vesting::judge::judge` with its fee settled.
struct Settled {
    answer: Answer,
    numbers: Numbers,
    fee: u64,
    /// The last build asked for exactly the fee it was built with.
    settled: bool,
}

/// Builds a transaction at a fee, judges it with the smallest budgets
/// (`token_vesting::judge::judge`) and recalculates the fee by the fee rule until a build
/// asks for the fee it was built with. If that does not happen within a few rounds, the
/// larger of the last two candidates is taken and must cover what its own build asks.
fn settle(
    name: &str,
    gate: &GateParams,
    feerate: u64,
    vesting: &[(usize, Compiled)],
    build: &dyn Fn(u64) -> Result<Assembled, String>,
) -> Result<Settled, String> {
    let round = |fee: u64| -> Result<(Answer, Numbers, u64), String> {
        let assembled = build(fee)?;
        set_calculated_storage_mass(gate, &assembled)?;
        let judged = judge(gate, assembled, vesting);
        let answer = Answer {
            tx: judged.assembled.tx,
            utxos: judged.assembled.utxos,
            submit: judged.submit,
            block: judged.block,
        };
        if !judged.budgets_measured {
            return Err(format!("{name}: the budgets could not be measured"));
        }
        let numbers = answer
            .numbers()
            .ok_or_else(|| format!("{name}: the gate gives no masses or no fee floor"))?;
        let wanted = feerate
            .checked_mul(numbers.charged_mass())
            .ok_or_else(|| format!("{name}: the fee does not fit 64 bits"))?;
        Ok((answer, numbers, wanted))
    };
    let (mut fee, mut before) = (0u64, 0u64);
    for _ in 0..FEE_ROUNDS {
        let (answer, numbers, wanted) = round(fee)?;
        if wanted == fee {
            return Ok(Settled {
                answer,
                numbers,
                fee,
                settled: true,
            });
        }
        before = fee;
        fee = wanted;
    }
    let fee = fee.max(before);
    let (answer, numbers, wanted) = round(fee)?;
    if fee < wanted {
        return Err(format!(
            "{name}: the fee did not settle; {fee} sompi is below the {wanted} its own \
             transaction asks"
        ));
    }
    println!("{name}: the fee did not settle; the larger of the last two candidates is taken");
    Ok(Settled {
        answer,
        numbers,
        fee,
        settled: false,
    })
}

/// The signature script of a pay-to-public-key coin: one Schnorr signature over the whole
/// transaction by the key behind `handle`.
fn p2pk_signature(handle: u8) -> LateBytes {
    LateBytes::from_tx(move |ctx| {
        raw::push(&keys::schnorr_signature(
            ctx,
            &keys::keypair(handle),
            SIG_HASH_ALL,
        ))
        .expect("a 65-byte push always fits a script")
    })
}

fn payer_input(coin: &Coin) -> OrdinaryInput {
    let mut input = OrdinaryInput::new(
        coin.entry.script_public_key.clone(),
        coin.entry.amount,
        coin.outpoint,
        p2pk_signature(PAYER),
    )
    .labelled(coin.what.clone());
    input.block_daa_score = coin.entry.block_daa_score;
    input
}

// ---------------------------------------------------------------------------------------
// The moves
// ---------------------------------------------------------------------------------------

/// What a probe is expected to be refused by.
#[derive(Clone, Debug)]
enum Expected {
    /// The vesting input (input 0) stops at this statement, and the node's message contains
    /// this text.
    Scripts(&'static str, &'static str),
    /// The node refuses the lock time.
    Finality,
}

impl Expected {
    fn json(&self) -> Value {
        match self {
            Expected::Scripts(line, message) => json!({
                "kind": "scripts",
                "input": 0,
                "contract_line": line,
                "node_message_contains": message,
            }),
            Expected::Finality => json!({
                "kind": "finality",
                "input": 0,
                "contract_line": null,
                "node_message_contains": FINALITY_MESSAGE,
            }),
        }
    }

    /// Whether a refusal is the expected one. `answer` is the probe's answer.
    fn matches(&self, refused: &Refused, answer: &Answer) -> Result<(), String> {
        match self {
            Expected::Scripts(line, message) => {
                let found = refused.contract_line.as_deref().unwrap_or("");
                if refused.layer != "scripts"
                    || refused.input != 0
                    || !refused.message.contains(message)
                    || !found.contains(line)
                {
                    return Err(format!(
                        "expected the scripts to refuse input 0 at `{line}`, got {refused:?}"
                    ));
                }
                if answer
                    .block
                    .verdict
                    .consensus_refusal
                    .as_ref()
                    .map(|r| r.stage)
                    != Some(Stage::Scripts)
                {
                    return Err("the block does not refuse at the scripts".to_string());
                }
                // A node checks the inputs in no fixed order and may report any failing one:
                // every input other than 0 must pass, as a submission and inside a block.
                for evaluation in [&answer.submit, &answer.block] {
                    let runs = &evaluation.committed_runs;
                    if runs.len() != answer.tx.inputs.len() {
                        return Err("not every input was run".to_string());
                    }
                    if let Some(run) = runs.iter().find(|run| run.input_index != 0 && !run.is_ok())
                    {
                        return Err(format!(
                            "input {} fails as well: {:?}",
                            run.input_index,
                            run.error().map(|error| error.to_string())
                        ));
                    }
                }
            }
            Expected::Finality => {
                if refused.layer != "consensus"
                    || refused.input != 0
                    || !refused.message.contains(FINALITY_MESSAGE)
                {
                    return Err(format!(
                        "expected the lock time to be refused, got {refused:?}"
                    ));
                }
                if answer
                    .block
                    .verdict
                    .consensus_refusal
                    .as_ref()
                    .map(|r| r.stage)
                    != Some(Stage::HeaderContext)
                {
                    return Err("the block does not refuse at the header context".to_string());
                }
                if !answer.scripts() {
                    return Err("a script refuses as well; only the lock time may".to_string());
                }
            }
        }
        Ok(())
    }
}

/// A release, described so that a probe can change one thing.
#[derive(Clone)]
struct ReleaseSpec {
    name: String,
    vesting: VestingState,
    vesting_coin: Coin,
    vault_coin: Coin,
    vault_amount: i64,
    claim: i64,
    claim_owner: [u8; 32],
    lock_time: u64,
    payer: Coin,
}

/// The terms of a run that do not change from move to move.
struct Run<'a> {
    request: &'a Request,
    vesting_id: Hash,
    token_covid: Hash,
    extension: [u8; 32],
    /// The moves in the order of the list, as written.
    moves: Vec<Value>,
    /// The judged transaction of every move, for the read-back.
    judged: Vec<Transaction>,
    fees_of_steps: u64,
    fees_of_probes: u64,
    unsettled: usize,
    table: String,
}

impl Run<'_> {
    fn vault_state(&self, amount: i64) -> Kcc20State {
        Kcc20State::new([0; 32], amount)
            .with_owner(OWNER_COVENANT_ID, self.vesting_id.as_bytes())
            .with_extension(self.extension)
    }

    fn claim_state(&self, owner: [u8; 32], amount: i64) -> Kcc20State {
        Kcc20State::new(owner, amount).with_extension(self.extension)
    }

    /// The release as a scenario, with the payer's coin in place of the crate's fixed funder.
    fn release(&self, spec: &ReleaseSpec, fee: u64) -> Result<Assembled, String> {
        let remaining = spec.vault_amount - spec.claim;
        let mut release = Release::honest(
            spec.name.clone(),
            spec.vesting.clone(),
            spec.vault_amount,
            spec.claim,
            spec.lock_time,
        );
        release.source = SOURCE.to_string();
        release.next_source = None;
        release.vesting_value = spec.vesting_coin.entry.amount;
        release.vesting_outpoint = spec.vesting_coin.outpoint;
        release.vesting_coin_covid = self.vesting_id;
        release.vault = self.vault_state(spec.vault_amount);
        release.vault_value = spec.vault_coin.entry.amount;
        release.vault_outpoint = spec.vault_coin.outpoint;
        release.vault_covid = self.token_covid;
        release.token_outputs = vec![self.claim_state(spec.claim_owner, spec.claim)];
        release.token_output_values = vec![self.request.coin_value];
        if remaining > 0 {
            release.token_outputs.push(self.vault_state(remaining));
            release
                .token_output_values
                .push(spec.vault_coin.entry.amount);
        }
        release.next_vesting_value = self.request.coin_value;
        release.fee = fee;
        let mut scenario = release.scenario();
        match &mut scenario.inputs[0] {
            InputSpec::Ordinary(input) => {
                input.block_daa_score = spec.vesting_coin.entry.block_daa_score;
            }
            InputSpec::Contract(_) => panic!("input 0 of a release is the vesting coin"),
        }
        match &mut scenario.inputs[1] {
            InputSpec::Contract(input) => {
                input.block_daa_score = spec.vault_coin.entry.block_daa_score;
            }
            InputSpec::Ordinary(_) => panic!("input 1 of a release is the vault"),
        }
        scenario.inputs[2] = InputSpec::Ordinary(payer_input(&spec.payer));
        let last = scenario.outputs.len() - 1;
        scenario.outputs.pop();
        let change = scenario
            .total_in()
            .checked_sub(scenario.total_out())
            .and_then(|left| left.checked_sub(fee))
            .ok_or_else(|| format!("{}: the payer's coin does not cover it", spec.name))?;
        scenario.push_plain_output(PlainOutput::new(p2pk(PAYER), change));
        assert_eq!(scenario.outputs.len(), last + 1);
        let assembled = scenario
            .assemble(Family::pinned())
            .map_err(|error| format!("{}: {error}", spec.name))?;
        // The coins the transaction spends are the coins of the run, script and all.
        for (index, coin) in [&spec.vesting_coin, &spec.vault_coin, &spec.payer]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                assembled.utxos[index], coin.entry,
                "{}: input {index} spends {}",
                spec.name, coin.what
            );
        }
        Ok(assembled)
    }

    /// The roles of the outputs of a release, in output order.
    fn release_roles(&self, spec: &ReleaseSpec) -> Vec<(Role, String)> {
        let remaining = spec.vault_amount - spec.claim;
        let owner = if spec.claim_owner == keys::x_only(BENEFICIARY_HANDLE) {
            "the beneficiary"
        } else if spec.claim_owner == keys::x_only(PAYER) {
            "the payer"
        } else {
            "another key"
        };
        let mut roles = vec![(
            Role::Token(self.claim_state(spec.claim_owner, spec.claim)),
            format!("claim of {} tokens owned by {owner}", spec.claim),
        )];
        if remaining > 0 {
            roles.push((
                Role::Token(self.vault_state(remaining)),
                format!("rest of the vault: {remaining} tokens"),
            ));
        }
        let released = if spec.claim < spec.vesting.total - spec.vesting.released {
            spec.vesting.released + spec.claim
        } else {
            spec.vesting.total
        };
        if released < spec.vesting.total || remaining > 0 {
            roles.push((
                Role::Vesting(spec.vesting.clone().with_released(released)),
                format!("vesting coin, {released} released"),
            ));
        }
        roles.push((Role::Change, "change of the payer".to_string()));
        roles
    }

    /// The entry of one move, with every check of its outputs, and the coins it creates.
    #[allow(clippy::too_many_arguments)]
    fn written(
        &self,
        name: &str,
        kind: &str,
        what: &str,
        answer: &Answer,
        spent: &[&Coin],
        roles: &[(Role, String)],
        numbers: &Numbers,
        fee: u64,
        judged_at: u64,
        send_before: Option<u64>,
        refusal: Option<(&Refused, &Expected)>,
        twin: Option<Value>,
    ) -> (Value, Vec<Coin>) {
        let tx = &answer.tx;
        let id = tx.id();
        assert_eq!(tx.inputs.len(), spent.len(), "{name}: the number of inputs");
        assert_eq!(
            tx.outputs.len(),
            roles.len(),
            "{name}: the number of outputs"
        );
        assert_eq!(tx.version, 1, "{name}: version 1");
        assert_eq!(tx.gas, 0, "{name}: no gas");
        assert!(tx.payload.is_empty(), "{name}: no payload");
        assert_eq!(
            tx.subnetwork_id,
            SubnetworkId::default(),
            "{name}: the native subnetwork"
        );
        assert_eq!(
            tx.storage_mass(),
            numbers.storage_mass,
            "{name}: the mass field is the calculated storage mass"
        );
        assert!(
            (1..=MAX_MASS).contains(&tx.storage_mass()),
            "{name}: the mass field is within the sender's bounds"
        );
        let mut total_in = 0u64;
        let inputs: Vec<Value> = tx
            .inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let coin = spent[index];
                assert_eq!(
                    input.previous_outpoint, coin.outpoint,
                    "{name}: input {index}"
                );
                assert_eq!(
                    answer.utxos[index], coin.entry,
                    "{name}: coin of input {index}"
                );
                assert_eq!(input.sequence, 0, "{name}: sequence of input {index}");
                assert!(
                    !input.signature_script.is_empty(),
                    "{name}: input {index} signed"
                );
                let ComputeCommit::ComputeBudget(budget) = input.compute_commit else {
                    panic!("{name}: input {index} commits a signature operation count");
                };
                assert!(budget.0 <= MAX_BUDGET, "{name}: budget of input {index}");
                total_in += coin.entry.amount;
                json!({
                    "transaction_id": hex(&input.previous_outpoint.transaction_id.as_bytes()),
                    "index": input.previous_outpoint.index,
                    "sequence": input.sequence,
                    "sig_op_count": 0,
                    "compute_budget": budget.0,
                    "signature_script": hex(&input.signature_script),
                    "spends": {
                        "from": coin.origin.json(),
                        "value": coin.entry.amount,
                        "script_public_key_version": coin.entry.script_public_key.version(),
                        "script_public_key": hex(coin.entry.script_public_key.script()),
                        "covenant_id": coin.entry.covenant_id.map(|id| hex(&id.as_bytes())),
                        "what": coin.what,
                    },
                })
            })
            .collect();
        let mut total_out = 0u64;
        let mut created = Vec::new();
        let mut outputs = Vec::new();
        for (index, (output, (role, what))) in tx.outputs.iter().zip(roles).enumerate() {
            let (role_name, to, state, covenant) = match role {
                Role::Vesting(state) => {
                    assert_eq!(
                        output.script_public_key,
                        Compiled::new(state).script_public_key(),
                        "{name}: output {index} is the vesting script of its state"
                    );
                    ("vesting", None, vesting_json(state), Some(self.vesting_id))
                }
                Role::Token(state) => {
                    assert_eq!(
                        output.script_public_key,
                        Family::pinned()
                            .script_public_key(&state.clone().into())
                            .expect("a token state has a script"),
                        "{name}: output {index} is the KCC20 script of its state"
                    );
                    ("token", None, kcc20_json(state), Some(self.token_covid))
                }
                Role::Change => {
                    assert_eq!(output.script_public_key, p2pk(PAYER), "{name}: change");
                    ("change", Some("payer"), Value::Null, None)
                }
            };
            assert_eq!(
                output.covenant.map(|binding| binding.covenant_id),
                covenant,
                "{name}: the binding of output {index}"
            );
            total_out += output.value;
            outputs.push(json!({
                "value": output.value,
                "script_public_key_version": output.script_public_key.version(),
                "script_public_key": hex(output.script_public_key.script()),
                "covenant_binding": output.covenant.map(|binding| json!({
                    "authorizing_input": binding.authorizing_input,
                    "covenant_id": hex(&binding.covenant_id.as_bytes()),
                })),
                "role": role_name,
                "to": to,
                "state": state,
                "what": what,
            }));
            created.push(Coin {
                outpoint: TransactionOutpoint::new(id, index as u32),
                entry: UtxoEntry::new(
                    output.value,
                    output.script_public_key.clone(),
                    judged_at,
                    false,
                    output.covenant.map(|binding| binding.covenant_id),
                ),
                origin: Origin::Move {
                    name: name.to_string(),
                    output: index as u32,
                },
                what: what.clone(),
            });
        }
        assert_eq!(
            total_in.checked_sub(total_out),
            Some(fee),
            "{name}: the fee is what the inputs hold above the outputs"
        );
        // A probe refused for its own lock time (B2) is sent long before that lock time: at
        // the score it was judged at, which is B1's `send_after_daa`.
        let send_after = if tx.lock_time == 0 {
            0
        } else if matches!(refusal, Some((_, Expected::Finality))) {
            judged_at
        } else {
            tx.lock_time + SEND_MARGIN
        };
        if let Some(before) = send_before {
            assert!(
                send_after < before,
                "{name}: the window to send it is empty"
            );
        }
        let gate_refusal = refusal.map(|(refused, _)| {
            json!({
                "layer": refused.layer,
                "input": refused.input,
                "rule": null,
                "message": refused.message,
            })
        });
        let entry = json!({
            "name": name,
            "kind": kind,
            "what": what,
            "expect": if kind == "step" { "accepted" } else { "refused" },
            "transaction_id": hex(&id.as_bytes()),
            "version": tx.version,
            "lock_time": tx.lock_time,
            "send_after_daa": send_after,
            "send_before_daa": send_before,
            "judged_at_daa": judged_at,
            "subnetwork_id": hex(tx.subnetwork_id.as_ref()),
            "gas": tx.gas,
            "payload": hex(&tx.payload),
            "mass": tx.storage_mass(),
            "inputs": inputs,
            "outputs": outputs,
            "fee": fee,
            "fee_floor": numbers.fee_floor,
            "masses": numbers.json(),
            "gate": {
                "scripts": answer.scripts(),
                "consensus": answer.consensus(),
                "mempool": answer.mempool(),
                "block": answer.block(),
                "refusal": gate_refusal,
            },
            "refusal_expected": refusal.map(|(_, expected)| expected.json()),
            "twin": twin,
        });
        // Built again from the entry alone, it is the transaction the gate judged.
        let rebuilt = rebuilt_from_the_file(&entry).expect("the entry rebuilds");
        assert_eq!(rebuilt.id(), id, "{name}: the id of the entry");
        assert_eq!(
            &rebuilt, tx,
            "{name}: the entry holds the judged transaction"
        );
        (entry, created)
    }

    fn record(&mut self, entry: Value, tx: Transaction, kind: &str, fee: u64, settled: bool) {
        let gate = &entry["gate"];
        let verdict = if kind == "step" {
            "accepted".to_string()
        } else {
            format!(
                "refused: {} input {}: {}",
                gate["refusal"]["layer"].as_str().unwrap_or("?"),
                gate["refusal"]["input"],
                gate["refusal"]["message"].as_str().unwrap_or("?")
            )
        };
        let _ = writeln!(
            self.table,
            "{:<22} {:<5} lock {:>10}  send after {:>10}  judged at {:>10}  fee {:>11} sompi  floor {:>9}  storage {:>6}  fee mass {:>6}  compute {:>6}  transient {:>6}  {}",
            entry["name"].as_str().unwrap_or("?"),
            kind,
            entry["lock_time"],
            entry["send_after_daa"],
            entry["judged_at_daa"],
            fee,
            entry["fee_floor"],
            entry["masses"]["calculated_storage_mass"],
            entry["masses"]["fee_mass"],
            entry["masses"]["compute_mass"],
            entry["masses"]["transient_mass"],
            verdict,
        );
        if kind == "step" {
            self.fees_of_steps += fee;
        } else {
            self.fees_of_probes += fee;
        }
        if !settled {
            self.unsettled += 1;
        }
        self.moves.push(entry);
        self.judged.push(tx);
    }

    /// Fails when the fees of the moves so far pass the cap of the request or the sender's
    /// bound.
    fn within_the_cap(&self, name: &str) -> Result<(), String> {
        let fees = self.fees_of_steps + self.fees_of_probes;
        let cap = self.request.fee_cap_total;
        if fees > cap {
            return Err(format!(
                "{name}: the fees reach {fees} sompi with this move, above the cap of {cap} sompi"
            ));
        }
        Ok(())
    }

    /// A settled release that must be accepted: a step, or the twin of a probe.
    fn honest(&self, spec: &ReleaseSpec, judged_at: u64) -> Result<Settled, String> {
        let compiled = Compiled::new(&spec.vesting);
        let settled = settle(
            &spec.name,
            &node_at(judged_at),
            self.request.feerate,
            &[(0, compiled)],
            &|fee| self.release(spec, fee),
        )?;
        if !settled.answer.accepted() {
            return Err(format!(
                "{}: not accepted at {judged_at}: {}",
                spec.name,
                settled.answer.why_not()
            ));
        }
        check_step_numbers(&spec.name, &settled, self.request.feerate)?;
        Ok(settled)
    }

    /// A probe: the release of `spec`, paying the fee and carrying the budgets of `twin`,
    /// judged as it is at `judged_at`; it must be refused as `expected` says.
    #[allow(clippy::too_many_arguments)]
    fn probe(
        &mut self,
        spec: &ReleaseSpec,
        what: &str,
        twin: &Settled,
        twin_judged_at: u64,
        twin_what: &str,
        judged_at: u64,
        send_before: Option<u64>,
        expected: Expected,
    ) -> Result<(), String> {
        let name = spec.name.clone();
        let gate = node_at(judged_at);
        let mut assembled = self.release(spec, twin.fee)?;
        assembled
            .set_compute_budgets(&twin.answer.budgets())
            .map_err(|error| format!("{name}: {error}"))?;
        set_calculated_storage_mass(&gate, &assembled)?;
        let answer = judge_as_is(&gate, assembled.tx, assembled.utxos);
        let compiled = Compiled::new(&spec.vesting);
        if !answer.refused_everywhere() {
            return Err(format!("{name}: not refused: {}", answer.why_not()));
        }
        let refused = answer
            .refusal(Some((0, &compiled)))
            .map_err(|error| format!("{name}: {error}"))?;
        expected
            .matches(&refused, &answer)
            .map_err(|error| format!("{name}: {error}"))?;
        let numbers = answer
            .numbers()
            .ok_or_else(|| format!("{name}: the gate gives no masses"))?;
        if twin.fee < numbers.fee_floor {
            return Err(format!(
                "{name}: the fee of its twin, {} sompi, is below its own floor of {} sompi",
                twin.fee, numbers.fee_floor
            ));
        }
        if !(1..=MAX_MASS).contains(&answer.tx.storage_mass()) {
            return Err(format!("{name}: the mass field is outside 1 to {MAX_MASS}"));
        }
        let spent = [&spec.vesting_coin, &spec.vault_coin, &spec.payer];
        let roles = self.release_roles(spec);
        let twin_json = json!({
            "accepted": true,
            "judged_at_daa": twin_judged_at,
            "what": twin_what,
        });
        let (entry, _) = self.written(
            &name,
            "probe",
            what,
            &answer,
            &spent,
            &roles,
            &numbers,
            twin.fee,
            judged_at,
            send_before,
            Some((&refused, &expected)),
            Some(twin_json),
        );
        println!(
            "{name}: refused as intended ({} input {}: {}{}); twin accepted at {twin_judged_at}",
            refused.layer,
            refused.input,
            refused.message,
            refused
                .contract_line
                .as_deref()
                .map(|line| format!("; line `{line}`"))
                .unwrap_or_default()
        );
        let tx = answer.tx;
        self.record(entry, tx, "probe", twin.fee, twin.settled);
        self.within_the_cap(&name)
    }
}

/// What every step and twin must show: the fee by the fee rule, at least the floor; the mass
/// field and the budgets within the sender's bounds.
fn check_step_numbers(name: &str, settled: &Settled, feerate: u64) -> Result<(), String> {
    let numbers = &settled.numbers;
    if settled.fee < numbers.fee_floor {
        return Err(format!(
            "{name}: the fee of {} sompi is below the floor of {} sompi",
            settled.fee, numbers.fee_floor
        ));
    }
    if settled.fee < feerate * numbers.charged_mass() {
        return Err(format!("{name}: the fee is below the fee rule"));
    }
    if settled.answer.submit.verdict.fee != Some(settled.fee) {
        return Err(format!("{name}: the node sees another fee"));
    }
    if !(1..=MAX_MASS).contains(&settled.answer.tx.storage_mass()) {
        return Err(format!("{name}: the mass field is outside 1 to {MAX_MASS}"));
    }
    if settled
        .answer
        .budgets()
        .iter()
        .any(|budget| *budget > MAX_BUDGET)
    {
        return Err(format!("{name}: a compute budget passes {MAX_BUDGET}"));
    }
    Ok(())
}

/// The transaction a reader builds from the fields of one entry of the chain file, and from
/// nothing else.
fn rebuilt_from_the_file(entry: &Value) -> Result<Transaction, String> {
    let number = |value: &Value, what: &str| {
        value
            .as_u64()
            .ok_or_else(|| format!("`{what}` is not a whole number"))
    };
    let bytes = |value: &Value, what: &str| {
        value
            .as_str()
            .and_then(unhex)
            .ok_or_else(|| format!("`{what}` is not lowercase hex"))
    };
    let hash = |value: &Value, what: &str| -> Result<Hash, String> {
        let bytes = bytes(value, what)?;
        <[u8; 32]>::try_from(bytes)
            .map(Hash::from_bytes)
            .map_err(|_| format!("`{what}` is not 32 bytes"))
    };
    let version = u16::try_from(number(&entry["version"], "version")?).map_err(|_| "a version")?;
    if version != 1 {
        return Err("only version 1 is built".to_string());
    }
    let mut inputs = Vec::new();
    for input in entry["inputs"].as_array().ok_or("no inputs")? {
        if input["sig_op_count"] != 0 {
            return Err("a version-1 input with a signature operation count".to_string());
        }
        inputs.push(TransactionInput::new_with_compute_budget(
            TransactionOutpoint::new(
                hash(&input["transaction_id"], "transaction_id")?,
                u32::try_from(number(&input["index"], "index")?).map_err(|_| "an index")?,
            ),
            bytes(&input["signature_script"], "signature_script")?,
            number(&input["sequence"], "sequence")?,
            u16::try_from(number(&input["compute_budget"], "compute_budget")?)
                .map_err(|_| "a compute budget")?,
        ));
    }
    let mut outputs = Vec::new();
    for output in entry["outputs"].as_array().ok_or("no outputs")? {
        let binding = &output["covenant_binding"];
        let binding = if binding.is_null() {
            None
        } else {
            Some(CovenantBinding::new(
                u16::try_from(number(&binding["authorizing_input"], "authorizing_input")?)
                    .map_err(|_| "an input index")?,
                hash(&binding["covenant_id"], "covenant_id")?,
            ))
        };
        outputs.push(TransactionOutput::with_covenant(
            number(&output["value"], "value")?,
            ScriptPublicKey::from_vec(
                u16::try_from(number(
                    &output["script_public_key_version"],
                    "script_public_key_version",
                )?)
                .map_err(|_| "a script version")?,
                bytes(&output["script_public_key"], "script_public_key")?,
            ),
            binding,
        ));
    }
    let subnetwork = <[u8; 20]>::try_from(bytes(&entry["subnetwork_id"], "subnetwork_id")?)
        .map_err(|_| "a subnetwork id of 20 bytes")?;
    let tx = Transaction::new(
        version,
        inputs,
        outputs,
        number(&entry["lock_time"], "lock_time")?,
        SubnetworkId::from(subnetwork),
        number(&entry["gas"], "gas")?,
        bytes(&entry["payload"], "payload")?,
    );
    tx.set_storage_mass(number(&entry["mass"], "mass")?);
    Ok(tx)
}

// ---------------------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------------------

/// A built chain: the file's document and the table for a reader.
struct Built {
    chain: Value,
    table: String,
    judged: Vec<Transaction>,
}

/// Builds the whole chain from a request, with the keys registered behind the handles.
fn build(request: &Request) -> Result<Built, String> {
    request.check()?;
    request.check_coins()?;
    let parties = [
        ("payer", keys::x_only(PAYER)),
        ("alice", keys::x_only(ALICE)),
        ("beneficiary", keys::x_only(BENEFICIARY_HANDLE)),
    ];
    if parties[0].1 == parties[1].1 || parties[0].1 == parties[2].1 || parties[1].1 == parties[2].1
    {
        return Err("two parties share a key".to_string());
    }
    let family = Family::pinned();
    let token_template = family.template_hash(Contract::Kcc20);
    let token_covid = Hash::from_bytes(request.token_covid);
    let start = request.start;
    let state = VestingState {
        beneficiary: keys::x_only(BENEFICIARY_HANDLE),
        token_covid: request.token_covid,
        token_template,
        start: start as i64,
        cliff: CLIFF,
        duration: DURATION,
        total: TOTAL,
        released: 0,
    };
    check_parameters(&state).map_err(|error| format!("check_parameters: {error:?}"))?;
    let vesting_spk = Compiled::new(&state).script_public_key();
    let funding = Coin {
        outpoint: request.funding.outpoint,
        entry: UtxoEntry::new(
            request.funding.value,
            request.funding.script_public_key.clone(),
            request.funding.block_daa_score,
            false,
            None,
        ),
        origin: Origin::Funding,
        what: "the funding coin of the payer".to_string(),
    };
    let token_coin = Coin {
        outpoint: request.token_coin.outpoint,
        entry: UtxoEntry::new(
            request.token_coin.value,
            request.token_coin.script_public_key.clone(),
            request.token_coin.block_daa_score,
            false,
            Some(token_covid),
        ),
        origin: Origin::TokenCoin,
        what: format!("Alice's token coin: {} tokens", request.token_state.amount),
    };

    // The vesting covenant id: the consensus rule over the authorizing outpoint (the funding
    // coin, input 0 of A1) and the one output it authorizes.
    let vesting_output = TransactionOutput::new(request.coin_value, vesting_spk.clone());
    let vesting_id = covenant_id(funding.outpoint, [(0u32, &vesting_output)].into_iter());
    let mut run = Run {
        request,
        vesting_id,
        token_covid,
        extension: request.token_state.extension_commitment,
        moves: Vec::new(),
        judged: Vec::new(),
        fees_of_steps: 0,
        fees_of_probes: 0,
        unsettled: 0,
        table: String::new(),
    };
    // A1 and A2 have no lock time: they are judged at the virtual DAA score of the request,
    // which is the start of the schedule minus the lead.
    let early = request.virtual_daa_score;

    // A1: the vesting genesis.
    let a1_name = "A1 vesting genesis";
    let a1 = settle(a1_name, &node_at(early), request.feerate, &[], &|fee| {
        let mut scenario = Scenario::new(a1_name);
        scenario.push_ordinary_input(payer_input(&funding));
        let mut vesting = PlainOutput::new(vesting_spk.clone(), request.coin_value);
        vesting.binding = Some(CovenantBinding::new(0, vesting_id));
        scenario.push_plain_output(vesting);
        let change = request
            .funding
            .value
            .checked_sub(request.coin_value)
            .and_then(|left| left.checked_sub(fee))
            .ok_or("A1: the funding coin does not cover it")?;
        scenario.push_plain_output(PlainOutput::new(p2pk(PAYER), change));
        scenario
            .assemble(Family::pinned())
            .map_err(|error| format!("A1: {error}"))
    })?;
    if !a1.answer.accepted() {
        return Err(format!("A1: not accepted: {}", a1.answer.why_not()));
    }
    check_step_numbers(a1_name, &a1, request.feerate)?;
    let checked = check_vesting_genesis(&a1.answer.tx, 0, &state)
        .map_err(|error| format!("check_vesting_genesis: {error:?}"))?;
    if checked != vesting_id {
        return Err("check_vesting_genesis: another covenant id".to_string());
    }
    let (entry, made) = run.written(
        a1_name,
        "step",
        "the payer's coin creates the vesting coin with a new covenant id (1 KAS), the change \
         goes back to the payer",
        &a1.answer,
        &[&funding],
        &[
            (
                Role::Vesting(state.clone()),
                "vesting coin, nothing released".to_string(),
            ),
            (Role::Change, "change of the payer".to_string()),
        ],
        &a1.numbers,
        a1.fee,
        early,
        None,
        None,
        None,
    );
    let (vesting_coin, payer_after_a1) = (made[0].clone(), made[1].clone());
    run.record(entry, a1.answer.tx, "step", a1.fee, a1.settled);
    run.within_the_cap(a1_name)?;

    // A2: Alice's token coin, whole, into the vault of the vesting covenant.
    let a2_name = "A2 fund the vault";
    let vault = run.vault_state(TOTAL);
    let a2 = settle(a2_name, &node_at(early), request.feerate, &[], &|fee| {
        let mut scenario = Scenario::new(a2_name);
        let mut token = ContractInput::new(
            request.token_state.clone(),
            token_coin.entry.amount,
            Some(token_covid),
            token_coin.outpoint,
            Call::typed(
                Entry::Transfer,
                vec![
                    Arg::States(vec![vault.clone()]),
                    Arg::Bytes(witness::owner_key(ALICE, EntryRole::Leader, SIG_HASH_ALL)),
                ],
            ),
        )
        .labelled("Alice's token coin: KCC20 transfer");
        token.block_daa_score = token_coin.entry.block_daa_score;
        scenario.push_contract_input(token);
        scenario.push_ordinary_input(payer_input(&payer_after_a1));
        scenario.push_contract_output(ContractOutput::new(
            vault.clone(),
            token_coin.entry.amount,
            Some(CovenantBinding::new(0, token_covid)),
        ));
        let change = payer_after_a1
            .entry
            .amount
            .checked_sub(fee)
            .ok_or("A2: the payer's change does not cover the fee")?;
        scenario.push_plain_output(PlainOutput::new(p2pk(PAYER), change));
        let assembled = scenario
            .assemble(Family::pinned())
            .map_err(|error| format!("A2: {error}"))?;
        assert_eq!(
            assembled.utxos[0], token_coin.entry,
            "A2 spends the token coin"
        );
        Ok(assembled)
    })?;
    if !a2.answer.accepted() {
        return Err(format!("A2: not accepted: {}", a2.answer.why_not()));
    }
    check_step_numbers(a2_name, &a2, request.feerate)?;
    let (entry, made) = run.written(
        a2_name,
        "step",
        "Alice moves her 10-token coin whole to the vesting covenant (owner scheme 4, borrowing \
         disabled, the same extension commitment); the payer pays the fee",
        &a2.answer,
        &[&token_coin, &payer_after_a1],
        &[
            (
                Role::Token(vault.clone()),
                "the vault: 10 tokens owned by the vesting covenant".to_string(),
            ),
            (Role::Change, "change of the payer".to_string()),
        ],
        &a2.numbers,
        a2.fee,
        early,
        None,
        None,
        None,
    );
    let (vault_coin, payer_after_a2) = (made[0].clone(), made[1].clone());
    check_vault(
        vesting_id,
        &state,
        &VaultCoin {
            entry: vault_coin.entry.clone(),
            state: vault.clone(),
        },
    )
    .map_err(|error| format!("check_vault: {error:?}"))?;
    run.record(entry, a2.answer.tx, "step", a2.fee, a2.settled);
    run.within_the_cap(a2_name)?;

    // The releases and the probes, all from the coins R1 spends.
    let at = |offset: i64| start + offset as u64;
    let beneficiary = keys::x_only(BENEFICIARY_HANDLE);
    let first = ReleaseSpec {
        name: RELEASES[0].0.to_string(),
        vesting: state.clone(),
        vesting_coin: vesting_coin.clone(),
        vault_coin: vault_coin.clone(),
        vault_amount: TOTAL,
        claim: RELEASES[0].2,
        claim_owner: beneficiary,
        lock_time: at(RELEASES[0].1),
        payer: payer_after_a2.clone(),
    };
    let cliff_judged = at(CLIFF) + SEND_MARGIN;
    let r1 = run.honest(&first, cliff_judged)?;

    // B1: a release of 1 token dated before the cliff. Twin: dated at the cliff.
    let b1_lock = at(1_000);
    let b1_judged = b1_lock + SEND_MARGIN;
    let b1 = ReleaseSpec {
        name: "B1 before the cliff".to_string(),
        claim: 1,
        lock_time: b1_lock,
        ..first.clone()
    };
    let b1_twin_spec = ReleaseSpec {
        name: "B1 twin: dated at the cliff".to_string(),
        lock_time: at(CLIFF),
        ..b1.clone()
    };
    let b1_twin = run.honest(&b1_twin_spec, cliff_judged)?;
    run.probe(
        &b1,
        "a release of 1 token dated 1000 after the start, before the cliff: the contract refuses",
        &b1_twin,
        cliff_judged,
        "the lock time at the cliff",
        b1_judged,
        None,
        Expected::Scripts(TIME_LINE, LOCK_TIME_SCRIPT_MESSAGE),
    )?;

    // B2: all 10 tokens dated at the end of the schedule, sent long before it.
    let b2_lock = at(DURATION);
    let b2 = ReleaseSpec {
        name: "B2 dated at the end".to_string(),
        claim: TOTAL,
        lock_time: b2_lock,
        ..first.clone()
    };
    let end_judged = b2_lock + SEND_MARGIN;
    let b2_twin_spec = ReleaseSpec {
        name: "B2 twin: judged after its lock time".to_string(),
        ..b2.clone()
    };
    let b2_twin = run.honest(&b2_twin_spec, end_judged)?;
    run.probe(
        &b2,
        "all 10 tokens, dated at the end of the schedule, sent long before it: the node refuses, \
         the lock time has not passed",
        &b2_twin,
        end_judged,
        "judged after its lock time",
        b1_judged,
        Some(b2_lock - SEND_BEFORE_MARGIN),
        Expected::Finality,
    )?;

    // B3: 4 tokens at the cliff, where 3 have vested. Twin: 3 tokens, which is R1.
    let b3 = ReleaseSpec {
        name: "B3 more than vested".to_string(),
        claim: 4,
        ..first.clone()
    };
    run.probe(
        &b3,
        "4 tokens at the cliff, where 3 have vested: the contract refuses",
        &r1,
        cliff_judged,
        "3 tokens",
        cliff_judged,
        None,
        Expected::Scripts(AMOUNT_LINE, SCRIPTS_MESSAGE),
    )?;

    // B4: 3 tokens at the cliff, signed by the beneficiary, the claim owned by the payer's
    // key. Twin: the claim owned by the beneficiary, which is R1.
    let b4 = ReleaseSpec {
        name: "B4 to another key".to_string(),
        claim_owner: keys::x_only(PAYER),
        ..first.clone()
    };
    run.probe(
        &b4,
        "3 tokens at the cliff, signed by the beneficiary, but the claim is owned by the payer's \
         key: the contract refuses",
        &r1,
        cliff_judged,
        "the claim owned by the beneficiary",
        cliff_judged,
        None,
        Expected::Scripts(RECIPIENT_LINE, SCRIPTS_MESSAGE),
    )?;

    // R1, R2, R3.
    let mut spec = first;
    let mut r1 = Some(r1);
    let mut released = 0;
    let mut vault_amount = TOTAL;
    let mut change_at_the_end = 0;
    let mut beneficiary_coins = Vec::new();
    for (index, (name, offset, claim, what)) in RELEASES.into_iter().enumerate() {
        let judged_at = at(offset) + SEND_MARGIN;
        let settled = match r1.take() {
            Some(settled) => settled,
            None => run.honest(&spec, judged_at)?,
        };
        assert_eq!(spec.name, name);
        assert!(claim <= spec.vesting.vested_at(spec.lock_time as i64) - released);
        let spent = [&spec.vesting_coin, &spec.vault_coin, &spec.payer];
        let roles = run.release_roles(&spec);
        let (entry, made) = run.written(
            name,
            "step",
            what,
            &settled.answer,
            &spent,
            &roles,
            &settled.numbers,
            settled.fee,
            judged_at,
            None,
            None,
            None,
        );
        let tx = settled.answer.tx;
        run.record(entry, tx, "step", settled.fee, settled.settled);
        run.within_the_cap(name)?;
        released += claim;
        vault_amount -= claim;
        beneficiary_coins.push(made[0].clone());
        if index + 1 < RELEASES.len() {
            let (next_name, next_offset, next_claim, _) = RELEASES[index + 1];
            spec = ReleaseSpec {
                name: next_name.to_string(),
                vesting: spec.vesting.clone().with_released(released),
                vesting_coin: made[2].clone(),
                vault_coin: made[1].clone(),
                vault_amount,
                claim: next_claim,
                claim_owner: beneficiary,
                lock_time: at(next_offset),
                payer: made[3].clone(),
            };
        } else {
            // The last release ends the vesting coin and empties the vault: the claim and
            // the change only.
            assert_eq!(made.len(), 2, "R3 makes the claim and the change");
            assert_eq!((released, vault_amount), (TOTAL, 0));
            change_at_the_end = made[1].entry.amount;
        }
    }

    let kas_locked_at_the_end: u64 = beneficiary_coins.iter().map(|coin| coin.entry.amount).sum();
    let fee_rule = format!(
        "{FEE_RULE}; {} of the steps and twins did not settle within {FEE_ROUNDS} rounds, and \
         for those the larger of the last two candidates was taken",
        run.unsettled
    );
    let parties_json = |name: &str| {
        let key = parties
            .iter()
            .find(|(party, _)| *party == name)
            .map(|(_, key)| *key)
            .expect("a party");
        json!({
            "x_only_public_key": hex(&key),
            "script_public_key": hex(pay_to_public_key_script(&key).script()),
        })
    };
    let mut funding_json = request.funding.json();
    funding_json["owner"] = json!("payer");
    let chain = json!({
        "format": CHAIN_FORMAT,
        "network": NETWORK,
        "built_at_utc": utc_now(),
        "nothing_was_sent": true,
        "coin_value": request.coin_value,
        "feerate": request.feerate,
        "fee_rule": fee_rule,
        "fee_cap_total": request.fee_cap_total,
        "schedule": { "start": start, "cliff": CLIFF, "duration": DURATION, "total": TOTAL },
        "funding": funding_json,
        "token": {
            "covenant_id": hex(&request.token_covid),
            "template_hash": hex(&token_template),
            "coin": request.token_coin.json(),
            "state": kcc20_json(&request.token_state),
        },
        "vesting_covenant_id": hex(&vesting_id.as_bytes()),
        "parties": {
            "payer": parties_json("payer"),
            "alice": parties_json("alice"),
            "beneficiary": parties_json("beneficiary"),
        },
        "launch_checks": {
            "check_vesting_genesis": "ok",
            "check_parameters": "ok",
            "check_vault": "ok",
        },
        "moves": run.moves,
        "totals": {
            "fees_of_steps": run.fees_of_steps,
            "fees_of_probes": run.fees_of_probes,
            "tokens_to_beneficiary": released,
            "kas_locked_at_the_end": kas_locked_at_the_end,
            "change_at_the_end": change_at_the_end,
        },
    });

    let mut table = String::new();
    let _ = writeln!(
        table,
        "vesting run on {NETWORK}: {CHAIN_FORMAT}, built offline, nothing sent"
    );
    let _ = writeln!(
        table,
        "request read at {} from {} (node {}), virtual DAA score {}",
        request.read_at_utc, request.node, request.node_version, request.virtual_daa_score
    );
    let _ = writeln!(
        table,
        "schedule: start {start}, cliff {CLIFF}, duration {DURATION}, total {TOTAL}; fee rate {} sompi per gram; fee cap {} KAS",
        request.feerate,
        kas(request.fee_cap_total)
    );
    for (name, key) in &parties {
        let _ = writeln!(table, "party {name:<12} x-only key {}", hex(key));
    }
    let _ = writeln!(
        table,
        "token covenant id {}; template hash {}",
        hex(&request.token_covid),
        hex(&token_template)
    );
    let _ = writeln!(table, "vesting covenant id {}", hex(&vesting_id.as_bytes()));
    let _ = writeln!(
        table,
        "launch checks: check_vesting_genesis ok, check_parameters ok, check_vault ok"
    );
    let _ = writeln!(table);
    table.push_str(&run.table);
    let _ = writeln!(table);
    let _ = writeln!(
        table,
        "fees of the steps {} KAS, of the probes {} KAS, in all {} KAS; tokens to the beneficiary {released}; KAS in the beneficiary's token coins at the end {} KAS; change at the end {} KAS",
        kas(run.fees_of_steps),
        kas(run.fees_of_probes),
        kas(run.fees_of_steps + run.fees_of_probes),
        kas(kas_locked_at_the_end),
        kas(change_at_the_end)
    );
    for entry in chain["moves"].as_array().expect("moves") {
        let _ = writeln!(
            table,
            "{:<22} {}",
            entry["name"].as_str().unwrap_or("?"),
            entry["transaction_id"].as_str().unwrap_or("?")
        );
    }
    Ok(Built {
        chain,
        table,
        judged: run.judged,
    })
}

// ---------------------------------------------------------------------------------------
// The read-back
// ---------------------------------------------------------------------------------------

/// Reads a chain file back, builds every transaction from its fields alone and judges it
/// again with the coins the file states, at the DAA score the file states. Every step must
/// be accepted, every probe refused as the file says; each rebuilt transaction must be the
/// one judged when the chain was built. Returns one line per move.
fn read_back(path: &Path, judged: &[Transaction]) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("read back: {error}"))?;
    if text.contains('\r') {
        return Err("read back: the file has CR line ends".to_string());
    }
    let chain: Value =
        serde_json::from_str(&text).map_err(|error| format!("read back: not JSON: {error}"))?;
    if chain["format"] != CHAIN_FORMAT || chain["nothing_was_sent"] != true {
        return Err("read back: not a chain of this run".to_string());
    }
    let moves = chain["moves"].as_array().ok_or("read back: no moves")?;
    if moves.len() != judged.len() {
        return Err("read back: another number of moves".to_string());
    }
    let start = chain["schedule"]["start"]
        .as_u64()
        .ok_or("read back: no start")?;
    let mut lines = Vec::new();
    let (mut fees_of_steps, mut fees_of_probes) = (0u64, 0u64);
    let mut spent_by_steps: Vec<(Value, Value)> = Vec::new();
    for (position, (entry, original)) in moves.iter().zip(judged).enumerate() {
        let name = entry["name"]
            .as_str()
            .ok_or("read back: a move without a name")?;
        let fail = |what: String| format!("read back, {name}: {what}");
        let tx = rebuilt_from_the_file(entry).map_err(fail)?;
        let id = hex(&tx.id().as_bytes());
        if entry["transaction_id"] != id.as_str() {
            return Err(fail(format!(
                "the id is {id}, the file says {}",
                entry["transaction_id"]
            )));
        }
        if &tx != original {
            return Err(fail("not the transaction that was judged".to_string()));
        }
        // The coins, from the file alone.
        let mut utxos = Vec::new();
        let mut vesting_input = None;
        for (index, input) in entry["inputs"]
            .as_array()
            .ok_or("inputs")?
            .iter()
            .enumerate()
        {
            let spends = &input["spends"];
            let from = &spends["from"];
            let outpoint = (input["transaction_id"].clone(), input["index"].clone());
            // A coin a step has spent is gone: no later move may spend it.
            if spent_by_steps.contains(&outpoint) {
                return Err(fail(format!(
                    "input {index} spends a coin an earlier step spent"
                )));
            }
            if entry["kind"] == "step" {
                spent_by_steps.push(outpoint.clone());
            }
            let block_daa_score = if from == "funding" || from == "token_coin" {
                let coin = if from == "funding" {
                    &chain["funding"]
                } else {
                    &chain["token"]["coin"]
                };
                let covenant = if from == "funding" {
                    Value::Null
                } else {
                    chain["token"]["covenant_id"].clone()
                };
                if (coin["transaction_id"].clone(), coin["index"].clone()) != outpoint
                    || coin["value"] != spends["value"]
                    || coin["script_public_key"] != spends["script_public_key"]
                    || coin["script_public_key_version"] != spends["script_public_key_version"]
                    || covenant != spends["covenant_id"]
                {
                    return Err(fail(format!(
                        "input {index} is not the {} the file states",
                        from.as_str().unwrap_or("?")
                    )));
                }
                coin["block_daa_score"].as_u64()
            } else {
                let source = moves[..position]
                    .iter()
                    .find(|earlier| earlier["name"] == from["move"] && earlier["kind"] == "step")
                    .ok_or_else(|| fail(format!("input {index} spends no earlier step")))?;
                let output =
                    &source["outputs"][from["output"].as_u64().unwrap_or(u64::MAX) as usize];
                if output["role"] == "vesting" {
                    let state = vesting_from_json(&output["state"]).map_err(fail)?;
                    vesting_input = Some((index, Compiled::new(&state)));
                }
                if (source["transaction_id"].clone(), from["output"].clone()) != outpoint
                    || output["value"] != spends["value"]
                    || output["script_public_key"] != spends["script_public_key"]
                    || output["script_public_key_version"] != spends["script_public_key_version"]
                    || output["covenant_binding"]["covenant_id"] != spends["covenant_id"]
                {
                    return Err(fail(format!(
                        "input {index} spends another coin than the step made"
                    )));
                }
                source["judged_at_daa"].as_u64()
            }
            .ok_or_else(|| fail(format!("no DAA score for the coin of input {index}")))?;
            let script = spends["script_public_key"]
                .as_str()
                .and_then(unhex)
                .ok_or_else(|| fail("a spent script".to_string()))?;
            let covenant = match &spends["covenant_id"] {
                Value::Null => None,
                value => Some(Hash::from_bytes(
                    value
                        .as_str()
                        .and_then(hex32)
                        .ok_or_else(|| fail("a spent covenant id".to_string()))?,
                )),
            };
            utxos.push(UtxoEntry::new(
                spends["value"]
                    .as_u64()
                    .ok_or_else(|| fail("a spent value".to_string()))?,
                ScriptPublicKey::from_vec(
                    spends["script_public_key_version"]
                        .as_u64()
                        .unwrap_or(u64::MAX) as u16,
                    script,
                ),
                block_daa_score,
                false,
                covenant,
            ));
        }
        let total_in: u64 = utxos.iter().map(|entry| entry.amount).sum();
        let total_out: u64 = tx.outputs.iter().map(|output| output.value).sum();
        let fee = entry["fee"]
            .as_u64()
            .ok_or_else(|| fail("no fee".to_string()))?;
        if total_in.checked_sub(total_out) != Some(fee) {
            return Err(fail(
                "the fee is not the inputs above the outputs".to_string(),
            ));
        }
        if entry["mass"] != entry["masses"]["calculated_storage_mass"] {
            return Err(fail(
                "the mass field is not the calculated storage mass".to_string(),
            ));
        }
        let lock_time = tx.lock_time;
        if lock_time != 0 && lock_time < start {
            return Err(fail("a lock time before the start".to_string()));
        }
        let judged_at = entry["judged_at_daa"]
            .as_u64()
            .ok_or_else(|| fail("no judged_at_daa".to_string()))?;
        // When it is sent: after `lock_time + 20`, except a probe refused for its own lock
        // time, which is sent at the score it was judged at, before `lock_time - 2000`.
        let send_after = entry["send_after_daa"]
            .as_u64()
            .ok_or_else(|| fail("no send_after_daa".to_string()))?;
        let finality = entry["refusal_expected"]["kind"] == "finality";
        match entry["send_before_daa"].as_u64() {
            Some(before) => {
                if !finality
                    || lock_time < SEND_BEFORE_MARGIN
                    || before != lock_time - SEND_BEFORE_MARGIN
                    || send_after != judged_at
                    || send_after >= before
                {
                    return Err(fail(format!(
                        "the window to send it is wrong: after {send_after}, before {before}"
                    )));
                }
            }
            None => {
                let expected = if lock_time == 0 {
                    0
                } else {
                    lock_time + SEND_MARGIN
                };
                if finality
                    || !entry["send_before_daa"].is_null()
                    || send_after != expected
                    || (lock_time != 0 && judged_at != send_after)
                {
                    return Err(fail(format!(
                        "sent after {send_after} and judged at {judged_at}, not as its lock \
                         time says"
                    )));
                }
            }
        }
        let answer = judge_as_is(&node_at(judged_at), tx, utxos);
        if answer.answer_mass() != entry["mass"].as_u64() {
            return Err(fail("a node calculates another storage mass".to_string()));
        }
        match entry["kind"].as_str() {
            Some("step") => {
                if !answer.accepted() {
                    return Err(fail(format!("not accepted: {}", answer.why_not())));
                }
                fees_of_steps += fee;
                lines.push(format!("{name}: accepted at {judged_at}, id {id}"));
            }
            Some("probe") => {
                if !answer.refused_everywhere() {
                    return Err(fail(format!("not refused: {}", answer.why_not())));
                }
                let refused = answer
                    .refusal(
                        vesting_input
                            .as_ref()
                            .map(|(index, compiled)| (*index, compiled)),
                    )
                    .map_err(fail)?;
                let stated = &entry["gate"]["refusal"];
                let expected = &entry["refusal_expected"];
                if stated["layer"] != refused.layer
                    || stated["input"] != refused.input
                    || stated["message"] != refused.message.as_str()
                {
                    return Err(fail(format!(
                        "refused otherwise than the file says: {refused:?}"
                    )));
                }
                let expected = match expected["kind"].as_str() {
                    Some("scripts") => Expected::Scripts(
                        match expected["contract_line"].as_str() {
                            Some(TIME_LINE) => TIME_LINE,
                            Some(AMOUNT_LINE) => AMOUNT_LINE,
                            Some(RECIPIENT_LINE) => RECIPIENT_LINE,
                            other => {
                                return Err(fail(format!("an unknown contract line {other:?}")));
                            }
                        },
                        match expected["node_message_contains"].as_str() {
                            Some(SCRIPTS_MESSAGE) => SCRIPTS_MESSAGE,
                            Some(LOCK_TIME_SCRIPT_MESSAGE) => LOCK_TIME_SCRIPT_MESSAGE,
                            other => return Err(fail(format!("an unknown message {other:?}"))),
                        },
                    ),
                    Some("finality") => Expected::Finality,
                    other => return Err(fail(format!("an unknown refusal kind {other:?}"))),
                };
                expected.matches(&refused, &answer).map_err(fail)?;
                // A probe refused for its lock time alone: its own bytes are refused, not
                // finalized, at both ends of its window to be sent, and accepted once the lock
                // time has passed (judged again at the DAA score of its twin).
                if matches!(expected, Expected::Finality) {
                    let before = entry["send_before_daa"]
                        .as_u64()
                        .ok_or_else(|| fail("no send_before_daa".to_string()))?;
                    for at in [send_after, before - 1] {
                        let early =
                            judge_as_is(&node_at(at), answer.tx.clone(), answer.utxos.clone());
                        let refused_early = early.refusal(None).map_err(fail)?;
                        if !early.refused_everywhere() || refused_early != refused {
                            return Err(fail(format!(
                                "at {at} it is not refused as at {judged_at}: {}",
                                early.why_not()
                            )));
                        }
                        Expected::Finality
                            .matches(&refused_early, &early)
                            .map_err(fail)?;
                    }
                    let twin_at = entry["twin"]["judged_at_daa"]
                        .as_u64()
                        .ok_or_else(|| fail("no twin score".to_string()))?;
                    let later =
                        judge_as_is(&node_at(twin_at), answer.tx.clone(), answer.utxos.clone());
                    if !later.accepted() {
                        return Err(fail(format!(
                            "not accepted after its lock time: {}",
                            later.why_not()
                        )));
                    }
                }
                fees_of_probes += fee;
                lines.push(format!(
                    "{name}: refused at {judged_at} as the file says ({} input {}: {}), id {id}",
                    refused.layer, refused.input, refused.message
                ));
            }
            other => return Err(fail(format!("an unknown kind {other:?}"))),
        }
        // No id and no signature covers a compute budget: a budget raised in the file shows
        // only in the compute mass a node calculates.
        let numbers = answer
            .numbers()
            .ok_or_else(|| fail("the gate gives no masses or no fee floor".to_string()))?;
        if entry["masses"] != numbers.json() || entry["fee_floor"] != numbers.fee_floor {
            return Err(fail(format!(
                "a node calculates other masses or another fee floor than the file says: \
                 {numbers:?}"
            )));
        }
    }
    if chain["totals"]["fees_of_steps"] != fees_of_steps
        || chain["totals"]["fees_of_probes"] != fees_of_probes
    {
        return Err("read back: the totals of the fees do not add up".to_string());
    }
    Ok(lines)
}

impl Answer {
    /// The storage mass the submission wrote into the mass field.
    fn answer_mass(&self) -> Option<u64> {
        self.submit
            .verdict
            .masses
            .as_ref()
            .and_then(|masses| masses.calculated_storage_mass)
    }
}

/// The text of a chain file: pretty JSON with LF line ends and a last line end.
fn chain_text(chain: &Value) -> String {
    serde_json::to_string_pretty(chain).expect("json") + "\n"
}

/// Writes the chain under a temporary name, reads it back and judges it again, and only
/// then writes the table and gives the chain its final name. When `overwrite` is false, none of
/// the three paths may exist.
fn write_and_read_back(
    built: &Built,
    output: &Path,
    overwrite: bool,
) -> Result<Vec<String>, String> {
    let table = table_path(output);
    let unchecked = unchecked_path(output);
    if !overwrite {
        for path in [output, table.as_path(), unchecked.as_path()] {
            if path.exists() {
                return Err(format!(
                    "{} exists already; it is not overwritten",
                    path.display()
                ));
            }
        }
    }
    let text = chain_text(&built.chain);
    for forbidden in ['\r', '\u{2014}', '\u{2013}'] {
        assert!(!text.contains(forbidden) && !built.table.contains(forbidden));
    }
    fs::write(&unchecked, &text).map_err(|error| format!("{}: {error}", unchecked.display()))?;
    let lines = match read_back(&unchecked, &built.judged) {
        Ok(lines) => lines,
        Err(error) => {
            let _ = fs::remove_file(&unchecked);
            return Err(error);
        }
    };
    // The table first, then the chain under its final name: a chain file under that name
    // exists only once everything else has been written.
    let give_up = |error: String| {
        let _ = fs::remove_file(&unchecked);
        error
    };
    if overwrite && output.exists() {
        fs::remove_file(output)
            .map_err(|error| give_up(format!("{}: {error}", output.display())))?;
    }
    fs::write(&table, &built.table)
        .map_err(|error| give_up(format!("{}: {error}", table.display())))?;
    if let Err(error) = fs::rename(&unchecked, output) {
        let _ = fs::remove_file(&table);
        let _ = fs::remove_file(output);
        return Err(give_up(format!("{}: {error}", output.display())));
    }
    Ok(lines)
}

// ---------------------------------------------------------------------------------------
// The real run
// ---------------------------------------------------------------------------------------

fn path_of(variable: &str) -> Result<PathBuf, String> {
    match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Err(format!("{variable} is not set")),
    }
}

fn real_run(input: &Path) -> Result<(), String> {
    for variable in ["VESTING_SOURCE_FILE", "VESTING_MUTANT"] {
        if std::env::var_os(variable).is_some() {
            return Err(format!(
                "{variable} is set: a real chain is built only from the contract as it is"
            ));
        }
    }
    let output = path_of(OUTPUT_ENV)?;
    let files = [
        (PAYER_KEY_ENV, path_of(PAYER_KEY_ENV)?),
        (ALICE_KEY_ENV, path_of(ALICE_KEY_ENV)?),
        (BENEFICIARY_KEY_ENV, path_of(BENEFICIARY_KEY_ENV)?),
    ];
    let written = [
        output.clone(),
        table_path(&output),
        unchecked_path(&output),
        input.to_path_buf(),
    ];
    for path in &written[..3] {
        if path.exists() {
            return Err(format!(
                "{} exists already; it is not overwritten",
                path.display()
            ));
        }
    }
    let text = fs::read_to_string(input)
        .map_err(|error| format!("{INPUT_ENV}: the request cannot be read: {error}"))?;
    let request = parse_request(&text)?;
    let secrets = load_keys(&files, &written)?;
    secrets.register();
    let built = build(&request)?;
    let lines = write_and_read_back(&built, &output, false)?;
    for line in &lines {
        println!("  {line}");
    }
    println!(
        "chain written to {} and read back clean: {} moves; fees of the steps {} KAS, of the probes {} KAS",
        output.display(),
        built.chain["moves"].as_array().map_or(0, Vec::len),
        kas(built.chain["totals"]["fees_of_steps"].as_u64().unwrap_or(0)),
        kas(built.chain["totals"]["fees_of_probes"]
            .as_u64()
            .unwrap_or(0)),
    );
    Ok(())
}

/// Judges a chain file that exists, from its fields alone: the read-back against the
/// transactions of the file itself. It takes no key: what the signatures and the compute
/// budgets are worth is for the node rules to say.
fn judge_file(path: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let chain: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{}: not JSON: {error}", path.display()))?;
    let moves = chain["moves"].as_array().ok_or("the file has no moves")?;
    let names: Vec<&str> = moves
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or("?"))
        .collect();
    if names != MOVE_NAMES {
        return Err(format!(
            "the moves are {names:?}, the run has {MOVE_NAMES:?}"
        ));
    }
    let mut own = Vec::new();
    for (entry, name) in moves.iter().zip(names) {
        own.push(rebuilt_from_the_file(entry).map_err(|error| format!("{name}: {error}"))?);
    }
    read_back(path, &own)
}

/// The judging of a file, only when `VESTING_JUDGE_CHAIN` names one.
fn the_judged_chain() -> bool {
    let Some(path) = std::env::var_os(JUDGE_ENV).filter(|value| !value.is_empty()) else {
        return false;
    };
    let others: Vec<&str> = [
        INPUT_ENV,
        OUTPUT_ENV,
        PAYER_KEY_ENV,
        ALICE_KEY_ENV,
        BENEFICIARY_KEY_ENV,
        "VESTING_SOURCE_FILE",
        "VESTING_MUTANT",
    ]
    .into_iter()
    .filter(|variable| std::env::var_os(variable).is_some_and(|value| !value.is_empty()))
    .collect();
    assert!(
        others.is_empty(),
        "{JUDGE_ENV} is set, and so is {}: a file is judged with no request, no key and the \
         contract as it is",
        others.join(", ")
    );
    let lines = judge_file(Path::new(&path))
        .unwrap_or_else(|error| panic!("the chain file does not pass: {error}"));
    for line in &lines {
        println!("  {line}");
    }
    println!(
        "{}: judged from its fields alone, {} moves as the file says",
        path.to_string_lossy(),
        lines.len()
    );
    true
}

/// The real run, only when `VESTING_LAUNCH_INPUT` names a request.
fn the_real_chain() {
    let Some(input) = std::env::var_os(INPUT_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    if let Err(error) = real_run(Path::new(&input)) {
        panic!("no chain was written: {error}");
    }
}

// ---------------------------------------------------------------------------------------
// The self-test
// ---------------------------------------------------------------------------------------

/// A made-up secret key: a fixed pattern that is no repeated byte.
fn made_up_key(label: u8) -> SecretKey {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = label ^ (index as u8).wrapping_mul(29) ^ 0x5c;
    }
    SecretKey::from_slice(&bytes).expect("a made-up key")
}

fn made_up_secrets() -> Secrets {
    Secrets {
        payer: made_up_key(0x11),
        alice: made_up_key(0x22),
        beneficiary: made_up_key(0x33),
    }
}

/// A request with a made-up funding coin of 16.614 KAS locked to the made-up payer, a
/// made-up token coin of 10 tokens owned by the made-up Alice, and a start near the real one.
fn made_up_request(feerate: u64) -> Request {
    let token_covid = [0x7c; 32];
    let token_state = Kcc20State::new(keys::x_only(ALICE), TOTAL).with_extension([0x3e; 32]);
    let virtual_daa_score = 597_194_000;
    Request {
        read_at_utc: "made up".to_string(),
        node: "none: self-test".to_string(),
        node_version: "2.1.0".to_string(),
        virtual_daa_score,
        coin_value: COIN_VALUE,
        feerate,
        fee_cap_total: MAX_FEES,
        start: virtual_daa_score + 6_000,
        funding: CoinRecord {
            outpoint: TransactionOutpoint::new(Hash::from_bytes([0xf1; 32]), 2),
            value: 1_661_402_000,
            script_public_key: p2pk(PAYER),
            block_daa_score: 593_874_770,
        },
        token_covid,
        token_coin: CoinRecord {
            outpoint: TransactionOutpoint::new(Hash::from_bytes([0xe7; 32]), 1),
            value: COIN_VALUE,
            script_public_key: Family::pinned()
                .script_public_key(&token_state.clone().into())
                .expect("a token state has a script"),
            block_daa_score: 593_874_700,
        },
        token_state,
    }
}

/// A request as the sender's `prepare` writes it (`vesting-launch-request/1`).
fn request_text(request: &Request) -> String {
    let document = json!({
        "format": REQUEST_FORMAT,
        "network": NETWORK,
        "read_at_utc": request.read_at_utc,
        "node": request.node,
        "node_version": request.node_version,
        "virtual_daa_score": request.virtual_daa_score,
        "coin_value": request.coin_value,
        "feerate": request.feerate,
        "fee_cap_total": request.fee_cap_total,
        "schedule": {
            "start": request.start,
            "cliff": CLIFF,
            "duration": DURATION,
            "total": TOTAL,
        },
        "funding": request.funding.json(),
        "token": {
            "covenant_id": hex(&request.token_covid),
            "coin": request.token_coin.json(),
            "state": kcc20_json(&request.token_state),
        },
    });
    serde_json::to_string_pretty(&document).expect("json") + "\n"
}

/// A folder that goes when this value goes, also when a panic unwinds through it.
struct RemovedAtTheEnd(PathBuf);

impl Drop for RemovedAtTheEnd {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn selftest_dir() -> PathBuf {
    std::env::var_os(SELFTEST_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_TARGET_TMPDIR")).join("testnet-selftest"))
}

/// The self-test at one fee rate: every step accepted, every probe refused as intended,
/// the read-back clean. Writes the chain and its table into the self-test folder.
fn self_test_at(feerate: u64, secrets: &Secrets) -> Value {
    // The request goes through its text, as in a real run.
    let made_up = made_up_request(feerate);
    let request = parse_request(&request_text(&made_up)).expect("the made-up request parses");
    assert_eq!(request_text(&request), request_text(&made_up));
    let built = build(&request)
        .unwrap_or_else(|error| panic!("the self-test at fee rate {feerate} fails: {error}"));
    let chain = &built.chain;
    let moves = chain["moves"].as_array().expect("moves");
    let names: Vec<&str> = moves
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(names, MOVE_NAMES);
    for entry in moves {
        let step = entry["kind"] == "step";
        for layer in ["consensus", "mempool", "block"] {
            assert_eq!(entry["gate"][layer], step, "{}: {layer}", entry["name"]);
        }
        let fee = entry["fee"].as_u64().expect("a fee");
        assert!(fee >= entry["fee_floor"].as_u64().expect("a floor"));
        let charged = entry["masses"]["fee_mass"]
            .as_u64()
            .expect("a fee mass")
            .max(entry["mass"].as_u64().expect("a mass"));
        if step {
            assert_eq!(entry["gate"]["scripts"], true);
            assert!(entry["gate"]["refusal"].is_null());
            assert_eq!(
                fee,
                feerate * charged,
                "{}: the fee rule settled",
                entry["name"]
            );
        }
    }
    // B2's scripts accept; the other probes are refused by the scripts.
    assert_eq!(moves[3]["gate"]["scripts"], true);
    for index in [2, 4, 5] {
        assert_eq!(moves[index]["gate"]["scripts"], false);
        assert_eq!(moves[index]["gate"]["refusal"]["layer"], "scripts");
    }
    assert_eq!(moves[3]["gate"]["refusal"]["layer"], "consensus");
    assert_eq!(
        moves[3]["send_before_daa"],
        moves[3]["lock_time"].as_u64().expect("lock") - SEND_BEFORE_MARGIN
    );
    assert_eq!(moves[3]["judged_at_daa"], moves[2]["send_after_daa"]);
    // B2 is sent at B1's score, long before its own lock time.
    assert_eq!(moves[3]["send_after_daa"], moves[2]["send_after_daa"]);
    for entry in moves {
        if let Some(before) = entry["send_before_daa"].as_u64() {
            let after = entry["send_after_daa"].as_u64().expect("send_after_daa");
            assert!(after < before, "{}: an empty window", entry["name"]);
        }
    }
    // A probe pays the fee of its twin: B3 and B4 of R1.
    assert_eq!(moves[4]["fee"], moves[6]["fee"]);
    assert_eq!(moves[5]["fee"], moves[6]["fee"]);
    let fees = chain["totals"]["fees_of_steps"].as_u64().expect("fees")
        + chain["totals"]["fees_of_probes"].as_u64().expect("fees");
    assert!(fees <= request.fee_cap_total);
    assert_eq!(chain["totals"]["tokens_to_beneficiary"], TOTAL);

    // No secret key in the file or the table.
    let text = chain_text(chain);
    for secret in [&secrets.payer, &secrets.alice, &secrets.beneficiary] {
        let shown = hex(&secret.secret_bytes());
        assert!(!text.contains(&shown) && !built.table.contains(&shown));
    }

    let directory = selftest_dir();
    fs::create_dir_all(&directory).expect("the self-test folder");
    // The made-up request too, as an example of the file the sender's `prepare` writes.
    fs::write(
        directory.join(format!("vesting-selftest-{feerate}.request.json")),
        request_text(&request),
    )
    .expect("the request of the self-test");
    let path = directory.join(format!("vesting-selftest-{feerate}.chain.json"));
    let lines = write_and_read_back(&built, &path, true)
        .unwrap_or_else(|error| panic!("the self-test at fee rate {feerate}: {error}"));
    println!("self-test, fee rate {feerate}: {}", path.display());
    print!("{}", built.table);
    for line in lines {
        println!("  read back: {line}");
    }
    built.chain
}

/// The self-test, only when `VESTING_LAUNCH_INPUT` is not set.
fn the_self_test() {
    if std::env::var_os(INPUT_ENV).is_some_and(|value| !value.is_empty()) {
        return;
    }
    let others: Vec<&str> = [
        OUTPUT_ENV,
        PAYER_KEY_ENV,
        ALICE_KEY_ENV,
        BENEFICIARY_KEY_ENV,
    ]
    .into_iter()
    .filter(|variable| std::env::var_os(variable).is_some_and(|value| !value.is_empty()))
    .collect();
    assert!(
        others.is_empty(),
        "{INPUT_ENV} is not set, but {} is: a real run needs the request file",
        others.join(", ")
    );
    let secrets = made_up_secrets();
    // The made-up keys go through key files outside the crate, as in a real run.
    let directory = RemovedAtTheEnd(std::env::temp_dir().join(format!(
        "vesting-selftest-made-up-keys-{}",
        std::process::id()
    )));
    fs::create_dir_all(&directory.0).expect("a folder for the made-up keys");
    let files = [
        (PAYER_KEY_ENV, directory.0.join("made-up-payer.key")),
        (ALICE_KEY_ENV, directory.0.join("made-up-alice.key")),
        (
            BENEFICIARY_KEY_ENV,
            directory.0.join("made-up-beneficiary.key"),
        ),
    ];
    for ((_, path), secret) in
        files
            .iter()
            .zip([&secrets.payer, &secrets.alice, &secrets.beneficiary])
    {
        // White space around the 64 characters is allowed.
        fs::write(path, format!("  {}\n", hex(&secret.secret_bytes()))).expect("a key file");
    }
    let loaded = load_keys(&files, &[]).expect("the made-up key files load");
    assert!(
        loaded.payer == secrets.payer
            && loaded.alice == secrets.alice
            && loaded.beneficiary == secrets.beneficiary
    );
    loaded.register();
    let low = self_test_at(100, &secrets);
    let high = self_test_at(1_000, &secrets);
    // One request builds to the same moves, whenever it is built: the sender goes on from a
    // record with a new build of its request, and with nothing else.
    let again = parse_request(&request_text(&made_up_request(100))).expect("the request parses");
    let again = build(&again).unwrap_or_else(|error| panic!("the second build: {error}"));
    assert_eq!(low["moves"], again.chain["moves"]);
    // The covenant id comes from the funding outpoint and the vesting output alone.
    assert_eq!(low["vesting_covenant_id"], high["vesting_covenant_id"]);
    assert_eq!(low["parties"], high["parties"]);
    assert_ne!(
        low["moves"][0]["transaction_id"],
        high["moves"][0]["transaction_id"]
    );
    refusals(&files);
    let path = directory.0.clone();
    drop(directory);
    assert!(!path.exists(), "the made-up key files are removed");
}

/// What the builder must refuse: a key file inside the crate, a request outside the run's
/// bounds, coins not of the parties, a chain file that exists, and a chain file changed after
/// it was written.
fn refusals(files: &[(&str, PathBuf); 3]) {
    // A panic unwinds through a folder of made-up keys and takes it along.
    let unwound =
        std::env::temp_dir().join(format!("vesting-selftest-unwound-{}", std::process::id()));
    fs::create_dir_all(&unwound).expect("a folder to unwind through");
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(|| {
        let _folder = RemovedAtTheEnd(unwound.clone());
        panic!("the self-test's own");
    });
    std::panic::set_hook(quiet);
    assert!(caught.is_err() && !unwound.exists());

    // A key file inside the crate, by the crate's own folder (the build's target folder may
    // lie anywhere). It is refused by its path: no file needs to be there.
    let mut inside_files = files.clone();
    inside_files[0].1 = Path::new(env!("CARGO_MANIFEST_DIR")).join("made-up-payer.key");
    let refused = load_keys(&inside_files, &[])
        .err()
        .expect("a key file inside the crate");
    assert!(refused.contains("inside the repository"), "{refused}");
    let refused = load_keys(files, &[files[1].1.clone()])
        .err()
        .expect("a key file at a path the run writes");
    assert!(refused.contains("writes to"), "{refused}");

    let good = request_text(&made_up_request(100));
    for (from, to, why) in [
        ("\"total\": 10", "\"total\": 11", "schedule.total"),
        ("\"feerate\": 100", "\"feerate\": 99", "feerate"),
        (
            "\"fee_cap_total\": 800000000",
            "\"fee_cap_total\": 800000001",
            "fee_cap_total",
        ),
        (
            "\"coin_value\": 100000000",
            "\"coin_value\": 100000001",
            "coin_value",
        ),
        (
            "\"network\": \"testnet-10\"",
            "\"network\": \"mainnet\"",
            "mainnet",
        ),
        (
            "\"format\": \"vesting-launch-request/1\"",
            "\"format\": \"vesting-launch-request/2\"",
            "format",
        ),
    ] {
        assert_eq!(good.matches(from).count(), 1, "{from}");
        let refused = parse_request(&good.replace(from, to))
            .err()
            .unwrap_or_else(|| panic!("a request with another {why} is refused"));
        assert!(refused.contains(why), "{refused}");
    }
    // The schedule's last score does not fit: a refusal, not an overflow.
    let mut last_score = made_up_request(100);
    last_score.start = u64::MAX;
    let refused = last_score.check().expect_err("a start at the end of u64");
    assert!(refused.contains("DAA-score domain"), "{refused}");
    // A long dash or a control character in a text of the request: a refusal, not a panic later.
    for (from, to) in [
        ("\"node_version\": \"", "\"node_version\": \"\u{2013}"),
        ("\"node\": \"", "\"node\": \"\u{2014}"),
        ("\"read_at_utc\": \"", "\"read_at_utc\": \"\\r"),
    ] {
        assert_eq!(good.matches(from).count(), 1, "{from}");
        let refused = parse_request(&good.replace(from, to))
            .err()
            .unwrap_or_else(|| panic!("a request with {to} is refused"));
        assert!(refused.contains(" holds a control character"), "{refused}");
    }
    let mut not_the_payers = made_up_request(100);
    not_the_payers.funding.script_public_key = p2pk(ALICE);
    assert!(not_the_payers.check_coins().is_err());
    let mut not_alices = made_up_request(100);
    not_alices.token_state.owner = keys::x_only(PAYER);
    assert!(not_alices.check_coins().is_err());
    let mut another_script = made_up_request(100);
    another_script.token_coin.script_public_key = p2pk(ALICE);
    assert!(another_script.check_coins().is_err());

    // The chain of the self-test exists: a run that may not overwrite refuses it.
    let path = selftest_dir().join("vesting-selftest-100.chain.json");
    let text = fs::read_to_string(&path).expect("the self-test chain");
    let built = Built {
        chain: serde_json::from_str(&text).expect("json"),
        table: String::new(),
        judged: Vec::new(),
    };
    let refused = write_and_read_back(&built, &path, false).expect_err("an existing chain file");
    assert!(refused.contains("exists already"), "{refused}");

    // One byte of a signature, one compute budget, one lock time changed in the file: the
    // read-back refuses each.
    let chain: Value = serde_json::from_str(&text).expect("json");
    let judged: Vec<Transaction> = chain["moves"]
        .as_array()
        .expect("moves")
        .iter()
        .map(|entry| rebuilt_from_the_file(entry).expect("an entry"))
        .collect();
    let changed = selftest_dir().join("changed.chain.json");
    let r1 = 6;
    type Change = fn(&mut Value);
    let changes: [(&str, &str, Change); 3] = [
        ("a signature byte", "not accepted", |chain| {
            let script = chain["moves"][6]["inputs"][0]["signature_script"]
                .as_str()
                .expect("a script")
                .to_string();
            let flipped = if &script[2..4] == "00" { "01" } else { "00" };
            chain["moves"][6]["inputs"][0]["signature_script"] =
                json!(format!("{}{flipped}{}", &script[..2], &script[4..]));
        }),
        ("a compute budget", "not accepted", |chain| {
            let budget = chain["moves"][6]["inputs"][1]["compute_budget"]
                .as_u64()
                .expect("a budget");
            chain["moves"][6]["inputs"][1]["compute_budget"] = json!(budget - 1);
        }),
        ("a lock time", "the id is", |chain| {
            let lock = chain["moves"][6]["lock_time"]
                .as_u64()
                .expect("a lock time");
            chain["moves"][6]["lock_time"] = json!(lock + 1);
        }),
    ];
    for (what, own_words, change) in changes {
        let mut copy = chain.clone();
        change(&mut copy);
        fs::write(&changed, chain_text(&copy)).expect("a changed chain");
        // Against the transactions of the file itself, so that only the node rules and the
        // ids can catch the change.
        let mut own = judged.clone();
        own[r1] = rebuilt_from_the_file(&copy["moves"][r1]).expect("an entry");
        let refused = read_back(&changed, &own)
            .err()
            .unwrap_or_else(|| panic!("a chain with {what} changed is refused"));
        println!("changed {what}: {refused}");
        assert!(
            refused.contains("R1 first release") && refused.contains(own_words),
            "{refused}"
        );
        let refused = read_back(&changed, &judged)
            .err()
            .unwrap_or_else(|| panic!("a chain with {what} changed is refused"));
        assert!(refused.contains("R1 first release"), "{refused}");
    }
    // B2 sent only after its own lock time would be accepted: the read-back refuses that
    // window.
    let mut late = chain.clone();
    let lock = late["moves"][3]["lock_time"].as_u64().expect("a lock time");
    late["moves"][3]["send_after_daa"] = json!(lock + SEND_MARGIN);
    fs::write(&changed, chain_text(&late)).expect("a changed chain");
    let refused = read_back(&changed, &judged).expect_err("B2 sent after its lock time");
    assert!(refused.contains("B2 dated at the end"), "{refused}");
    println!("changed B2's window: {refused}");
    // Judging a file that exists, with no key: the chain as written passes; a budget of 1
    // does not, in a probe or in a step, nor one raised by 1; and neither does a file with a
    // move left out.
    judge_file(&path).unwrap_or_else(|error| panic!("the self-test chain: {error}"));
    for (position, name) in [(2, "B1 before the cliff"), (6, "R1 first release")] {
        let budget = chain["moves"][position]["inputs"][0]["compute_budget"]
            .as_u64()
            .expect("a budget");
        for (other, words) in [(1, "script units"), (budget + 1, "other masses")] {
            let mut changed_budget = chain.clone();
            changed_budget["moves"][position]["inputs"][0]["compute_budget"] = json!(other);
            fs::write(&changed, chain_text(&changed_budget)).expect("a changed chain");
            let refused = judge_file(&changed).expect_err("another compute budget");
            println!("judged with a budget of {other} at {name}: {refused}");
            assert!(
                refused.contains(name) && refused.contains(words),
                "{refused}"
            );
        }
    }
    let mut shorter = chain.clone();
    shorter["moves"].as_array_mut().expect("moves").remove(4);
    fs::write(&changed, chain_text(&shorter)).expect("a changed chain");
    let refused = judge_file(&changed).expect_err("a move left out");
    assert!(refused.contains("the moves are"), "{refused}");
    let _ = fs::remove_file(&changed);
    println!("refusals: as intended");
}

fn main() {
    assert!(
        std::env::var_os("VESTING_DUMP_DIR").is_none_or(|value| value.is_empty()),
        "VESTING_DUMP_DIR is set: the builder's transactions would go into the input of the \
         replay in engine-2.1.0/; run the builder without it"
    );
    if the_judged_chain() {
        return;
    }
    the_self_test();
    the_real_chain();
}
