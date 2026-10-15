//! Checks of a vesting launch that no script can make: they are for whoever funds a vesting
//! coin or relies on one, and run before any token goes to the vesting covenant.
//!
//! The vault is owned by the vesting covenant's id (owner scheme 0x04), and the token
//! program lets a vault coin move whenever ANY coin with that id is spent beside it. The
//! contract governs only the spends of its own coin. So a launch is sound only if the
//! vesting covenant has exactly one coin: the genesis made one output with the id, and
//! that output is the TokenVesting script with the intended state. Consensus allows a
//! genesis to give the same id to several outputs (rusty-kaspa a41a333
//! crypto/txscript/src/covenants.rs:120-160); see `tests/genesis.rs`.

use kaspa_consensus_core::constants::LOCK_TIME_THRESHOLD;
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kaspa_hashes::Hash;
use kcc20_family::states::{BORROW_DISABLED, OWNER_COVENANT_ID};
use kcc20_family::{Contract, Family, Kcc20State};

use crate::contract::{Compiled, VestingState, int_constant};

/// Why a launch is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// No output of the genesis carries the expected id.
    NoVestingOutput,
    /// The id written on the vesting output is not the one its genesis group hashes to.
    WrongCovenantId { written: Hash, recomputed: Hash },
    /// More than one output carries the id: every one of them opens the vault.
    MoreThanOneCoin { outputs: Vec<usize> },
    /// The coin carrying the id is not the TokenVesting script with the intended state.
    NotTheVestingScript,
    /// The vault coin is not what the schedule needs.
    Vault(String),
    /// The parameters would make a dead coin.
    Parameters(String),
}

/// Checks the genesis transaction of a vesting covenant: the output at `vesting_output`
/// carries the id its genesis group hashes to, it is the only output with that id, and its
/// script is TokenVesting compiled for `intended`. Returns the covenant id.
pub fn check_vesting_genesis(
    genesis: &Transaction,
    vesting_output: usize,
    intended: &VestingState,
) -> Result<Hash, LaunchError> {
    let output = genesis
        .outputs
        .get(vesting_output)
        .ok_or(LaunchError::NoVestingOutput)?;
    let binding = output.covenant.ok_or(LaunchError::NoVestingOutput)?;
    let group: Vec<usize> = genesis
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.covenant.is_some_and(|other| {
                other.covenant_id == binding.covenant_id
                    && other.authorizing_input == binding.authorizing_input
            })
        })
        .map(|(index, _)| index)
        .collect();
    let authorizing = genesis
        .inputs
        .get(binding.authorizing_input as usize)
        .ok_or(LaunchError::NoVestingOutput)?;
    let recomputed = covenant_id(
        authorizing.previous_outpoint,
        group
            .iter()
            .map(|&index| (index as u32, &genesis.outputs[index])),
    );
    if recomputed != binding.covenant_id {
        return Err(LaunchError::WrongCovenantId {
            written: binding.covenant_id,
            recomputed,
        });
    }
    let carrying: Vec<usize> = genesis
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate
                .covenant
                .is_some_and(|other| other.covenant_id == binding.covenant_id)
        })
        .map(|(index, _)| index)
        .collect();
    if carrying.len() != 1 {
        return Err(LaunchError::MoreThanOneCoin { outputs: carrying });
    }
    if output.script_public_key != Compiled::new(intended).script_public_key() {
        return Err(LaunchError::NotTheVestingScript);
    }
    Ok(binding.covenant_id)
}

/// Checks the parameters of a new vesting coin: values the contract refuses at every release
/// (so the coin and its vault would be dead), or that it would read as another schedule.
/// The bounds are the contract's own constants, read from its source (`MAX_DAA`,
/// `MAX_DURATION`), and the node's lock-time threshold; each test below is one `require` of
/// step 2 or 3 of the contract, except the key check and `released == 0`, which is stricter
/// on purpose.
pub fn check_parameters(state: &VestingState) -> Result<(), LaunchError> {
    let fail = |what: &str| Err(LaunchError::Parameters(what.to_string()));
    let max_daa = int_constant("MAX_DAA");
    let max_duration = int_constant("MAX_DURATION");
    if secp256k1::XOnlyPublicKey::from_slice(&state.beneficiary).is_err() {
        return fail("the beneficiary is not a valid x-only public key");
    }
    if !(0..=max_daa).contains(&state.start) {
        return fail("start must be between 0 and MAX_DAA (step 2 of the contract)");
    }
    if !(0..=max_daa).contains(&state.cliff) {
        return fail("cliff must be between 0 and MAX_DAA (step 2 of the contract)");
    }
    // Both are at most MAX_DAA here: the sum cannot overflow.
    if state.start + state.cliff >= LOCK_TIME_THRESHOLD as i64 {
        return fail("start + cliff is outside the DAA-score domain of lock times (step 3)");
    }
    if !(1..=max_duration).contains(&state.duration) {
        return fail("duration must be between 1 and MAX_DURATION (step 2 of the contract)");
    }
    if state.total <= 0 || state.released != 0 {
        return fail("a new coin has a positive total and nothing released");
    }
    Ok(())
}

/// A vault coin as the chain holds it: its UTXO entry (covenant id, locking script, KAS) and
/// the token state its script is said to carry. [`check_vault`] checks the one against the
/// other.
#[derive(Clone, Debug)]
pub struct VaultCoin {
    /// The coin's UTXO entry.
    pub entry: UtxoEntry,
    /// The token state the coin's script is said to carry.
    pub state: Kcc20State,
}

/// Checks the vault coin before the first release, the whole coin and not only its state:
/// it is a coin of the token family the vesting coin names (`token_covid`); the vesting
/// coin's `token_template` is the KCC20 program the contract can read (the pinned one: the
/// contract's TOKEN_PREFIX_LEN and TOKEN_SUFFIX_LEN are its lengths), and the coin's script
/// is that program with the state given for it. Then the state: owned by the vesting
/// covenant through owner scheme 0x04, closed to borrowing (otherwise anyone can add
/// tokens and move its outpoint), holding exactly what is left to release. The coin's KAS
/// are not bounded: each release keeps them in the rest of the vault.
///
/// The check is made once, at launch. The contract releases from any coin of the family
/// that its covenant owns, and nobody checks a coin sent to the covenant later (README.md,
/// "What a launch must check").
pub fn check_vault(
    vesting_covid: Hash,
    state: &VestingState,
    vault: &VaultCoin,
) -> Result<(), LaunchError> {
    let token_covid = Hash::from_bytes(state.token_covid);
    if vault.entry.covenant_id != Some(token_covid) {
        return Err(LaunchError::Vault(format!(
            "not a coin of the token family {token_covid}: its covenant id is {:?}",
            vault.entry.covenant_id
        )));
    }
    let family = Family::pinned();
    if family.template_hash(Contract::Kcc20) != state.token_template {
        return Err(LaunchError::Vault(
            "the vesting coin names a token program other than the KCC20 program it reads".into(),
        ));
    }
    let script = family
        .script_public_key(&vault.state.clone().into())
        .map_err(|error| LaunchError::Vault(error.to_string()))?;
    if vault.entry.script_public_key != script {
        return Err(LaunchError::Vault(
            "its script is not the KCC20 program with the state given for it".into(),
        ));
    }
    let vault = &vault.state;
    if vault.owner_scheme != OWNER_COVENANT_ID || vault.owner != vesting_covid.as_bytes() {
        return Err(LaunchError::Vault(
            "not owned by the vesting covenant through scheme 0x04".into(),
        ));
    }
    if vault.borrow_scheme != BORROW_DISABLED {
        return Err(LaunchError::Vault("open to borrowing".into()));
    }
    if vault.amount != state.total - state.released {
        return Err(LaunchError::Vault(format!(
            "holds {} tokens, the schedule has {} left to release",
            vault.amount,
            state.total - state.released
        )));
    }
    Ok(())
}
