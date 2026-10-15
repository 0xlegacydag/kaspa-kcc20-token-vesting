//! One `release` transaction, described by its parts, so that a check can change any part.
//!
//! Layout of the honest transaction:
//!
//! | input | coin |
//! |---|---|
//! | 0 | the vesting coin (covenant [`VESTING_COVID`]), calling `release` |
//! | 1 | the vault: a KCC20 coin of the token family [`TOKEN_COVID`], owned by the vesting covenant (owner scheme 0x04), calling `transfer` on the normal path with an empty owner witness |
//! | 2 | the funder's pay-to-public-key coin: pays the fee and the KAS of the new token coin |
//!
//! | output | coin |
//! |---|---|
//! | 0 | the claim: a KCC20 coin owned by the beneficiary's key (owner scheme 0x00), bound to input 1 |
//! | 1 | the rest of the vault, owned by the vesting covenant, bound to input 1 (absent when nothing is left) |
//! | next | the vesting coin with the new released amount, bound to input 0 (absent after the last release) |
//! | last | the funder's change |

use kaspa_consensus_core::hashing::sighash_type::{SIG_HASH_ALL, SigHashType};
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutpoint, UtxoEntry};
use kaspa_hashes::Hash;
use kcc20_family::keys::{self, SigningContext};
use kcc20_family::scenario::{Call, OutputSpec};
use kcc20_family::shapes::{outpoint, pay_to_public_key_script};
use kcc20_family::states::{OWNER_COVENANT_ID, PATH_NORMAL};
use kcc20_family::{
    Arg, ContractInput, ContractOutput, Entry, Family, Kcc20State, LateBytes, OrdinaryInput,
    PlainOutput, Scenario,
};

use crate::contract::{Compiled, VestingState, active_source};
use crate::launch::VaultCoin;

/// Covenant id of the token family (the synthetic id of the shapes of `kcc20-family`).
pub const TOKEN_COVID: [u8; 32] = [0x11; 32];
/// Covenant id of the vesting contract.
pub const VESTING_COVID: [u8; 32] = [0x77; 32];
/// Covenant id of another token family, for the checks with a second token.
pub const OTHER_TOKEN_COVID: [u8; 32] = [0x22; 32];

/// Key seed of the beneficiary.
pub const BENEFICIARY: u8 = 0xbe;
/// Key seed of somebody else.
pub const STRANGER: u8 = 0x5e;
/// Key seed of the funder, who pays the fee.
pub const FUNDER: u8 = keys::FUNDER;

/// Sompi per KAS.
pub const SOMPI_PER_KAS: u64 = 100_000_000;
/// KAS held by every covenant coin: 1 KAS. The storage mass of a transaction grows as the
/// KAS of the coins it creates shrinks (KIP-9), and a release creates up to three of them.
pub const COIN_VALUE: u64 = SOMPI_PER_KAS;
/// The funder's coin.
pub const FUNDING_VALUE: u64 = 100 * SOMPI_PER_KAS;
/// Fee of every check: generous on purpose, so that a refusal is never about the fee. The
/// honest release at its smallest budgets is measured separately.
pub const DEFAULT_FEE: u64 = SOMPI_PER_KAS / 10;

/// The schedule of the checks, in DAA units. `start` is set from the gate's DAA score.
pub const CLIFF: i64 = 1_000;
/// Duration of the schedule.
pub const DURATION: i64 = 10_000;
/// Tokens of the schedule.
pub const TOTAL: i64 = 1_000_000;

/// The covenant id of the token family as a hash.
pub fn token_covid() -> Hash {
    Hash::from_bytes(TOKEN_COVID)
}

/// The covenant id of the vesting contract as a hash.
pub fn vesting_covid() -> Hash {
    Hash::from_bytes(VESTING_COVID)
}

/// The vesting coin's state at the start of the schedule.
pub fn initial_state(start: i64) -> VestingState {
    VestingState {
        beneficiary: keys::x_only(BENEFICIARY),
        token_covid: TOKEN_COVID,
        token_template: Family::pinned().template_hash(kcc20_family::Contract::Kcc20),
        start,
        cliff: CLIFF,
        duration: DURATION,
        total: TOTAL,
        released: 0,
    }
}

/// A token coin owned by the vesting covenant, borrowing disabled, zero extension.
pub fn vault_state(amount: i64) -> Kcc20State {
    Kcc20State::new([0; 32], amount).with_owner(OWNER_COVENANT_ID, VESTING_COVID)
}

/// A coin of the token family [`TOKEN_COVID`] with this state, as the assembler of
/// `kcc20-family` makes it: the pinned KCC20 script, [`COIN_VALUE`] KAS.
pub fn token_coin(state: Kcc20State) -> VaultCoin {
    let script_public_key = Family::pinned()
        .script_public_key(&state.clone().into())
        .expect("a token state has a script");
    VaultCoin {
        entry: UtxoEntry::new(COIN_VALUE, script_public_key, 0, false, Some(token_covid())),
        state,
    }
}

/// A token coin owned by the beneficiary's key.
pub fn claim_state(amount: i64) -> Kcc20State {
    Kcc20State::new(keys::x_only(BENEFICIARY), amount)
}

/// How the vesting input signs.
#[derive(Clone, Debug)]
pub enum Signer {
    /// A Schnorr signature by the key with this seed, of this type.
    Key(u8, SigHashType),
    /// These 65 bytes.
    Fixed(Vec<u8>),
}

/// The parts of one `release` transaction.
#[derive(Clone, Debug)]
pub struct Release {
    /// Name for reports.
    pub name: String,
    /// The contract source the vesting coin runs.
    pub source: String,
    /// State of the vesting coin being spent.
    pub vesting: VestingState,
    /// KAS of the vesting coin.
    pub vesting_value: u64,
    /// Outpoint of the vesting coin.
    pub vesting_outpoint: TransactionOutpoint,
    /// Covenant id the vesting coin carries.
    pub vesting_coin_covid: Hash,
    /// State of the token coin being spent.
    pub vault: Kcc20State,
    /// KAS of the token coin.
    pub vault_value: u64,
    /// Outpoint of the token coin.
    pub vault_outpoint: TransactionOutpoint,
    /// Covenant id the token coin carries.
    pub vault_covid: Hash,
    /// Template the token coin runs: the pinned family, or one with a replaced KCC20.
    pub token_family: Family,
    /// The `claim` argument of the call.
    pub claim: i64,
    /// Who signs the call.
    pub signer: Signer,
    /// Lock time of the transaction.
    pub lock_time: u64,
    /// Sequence of the vesting input.
    pub vesting_sequence: u64,
    /// The token outputs: states locked into them, in order. The token's own call declares
    /// the same states unless [`Release::declared`] says otherwise.
    pub token_outputs: Vec<Kcc20State>,
    /// States declared to the token's `transfer`, when they differ from the locked ones.
    pub declared: Option<Vec<Kcc20State>>,
    /// KAS of each token output.
    pub token_output_values: Vec<u64>,
    /// The vesting coin's next state, when it continues.
    pub next_vesting: Option<VestingState>,
    /// The source of the next vesting coin, when it differs from [`Release::source`].
    pub next_source: Option<String>,
    /// KAS of the next vesting coin.
    pub next_vesting_value: u64,
    /// Outpoint of the funder's coin.
    pub funding_outpoint: TransactionOutpoint,
    /// Fee left to the miner.
    pub fee: u64,
    /// The covenant owns coins of the token family beside the one this release spends:
    /// set by [`Release::with_other_coins_on_the_covenant`].
    pub other_coins_on_the_covenant: bool,
}

/// The vesting coin's next state after a release of `claim` that leaves `remaining` tokens
/// in the token coin, or none when the release ends it. The released amount stops at total,
/// as in step 8 of the contract. Once it reaches total with nothing left in the token coin,
/// the contract lets the beneficiary keep the vesting coin or end it: this keeps it while
/// the covenant owns other coins of the family, which an ended coin would lock for good.
fn next_vesting_state(
    vesting: &VestingState,
    claim: i64,
    remaining: i64,
    other_coins_on_the_covenant: bool,
) -> Option<VestingState> {
    let new_released = if claim < vesting.total.saturating_sub(vesting.released) {
        vesting.released + claim
    } else {
        vesting.total
    };
    (new_released < vesting.total || remaining > 0 || other_coins_on_the_covenant)
        .then(|| vesting.clone().with_released(new_released))
}

impl Release {
    /// The honest release of `claim` tokens at lock time `lock_time`, from a vesting coin
    /// in `vesting` and a vault holding `vault_amount` tokens.
    pub fn honest(
        name: impl Into<String>,
        vesting: VestingState,
        vault_amount: i64,
        claim: i64,
        lock_time: u64,
    ) -> Self {
        let remaining = vault_amount - claim;
        let mut token_outputs = vec![claim_state(claim)];
        if remaining > 0 {
            token_outputs.push(vault_state(remaining));
        }
        // Ends the vesting coin once released reaches total with nothing left in the token
        // coin: the covenant owns no other coin here, unless the check says so
        // (`with_other_coins_on_the_covenant`).
        let next_vesting = next_vesting_state(&vesting, claim, remaining, false);
        Self {
            name: name.into(),
            source: active_source(),
            vesting,
            vesting_value: COIN_VALUE,
            vesting_outpoint: outpoint(0x31, 0),
            vesting_coin_covid: vesting_covid(),
            vault: vault_state(vault_amount),
            vault_value: COIN_VALUE,
            vault_outpoint: outpoint(0x32, 0),
            vault_covid: token_covid(),
            token_family: Family::pinned().clone(),
            claim,
            signer: Signer::Key(BENEFICIARY, SIG_HASH_ALL),
            lock_time,
            vesting_sequence: 0,
            token_output_values: vec![COIN_VALUE; token_outputs.len()],
            token_outputs,
            declared: None,
            next_vesting,
            next_source: None,
            next_vesting_value: COIN_VALUE,
            funding_outpoint: outpoint(0x33, 0),
            fee: DEFAULT_FEE,
            other_coins_on_the_covenant: false,
        }
    }

    /// The same honest release when the covenant owns other coins of the token family than
    /// the one spent (a vault that still holds tokens while a coin sent later is taken): the
    /// vesting coin continues, with released at most total, so that those coins can still be
    /// released. Called right after [`Release::honest`], before any other change.
    pub fn with_other_coins_on_the_covenant(mut self) -> Self {
        self.other_coins_on_the_covenant = true;
        let remaining = self.vault.amount - self.claim;
        self.next_vesting = next_vesting_state(&self.vesting, self.claim, remaining, true);
        self
    }

    /// The token coin this release spends (input 1), whole: its UTXO entry as the
    /// assembler makes it, and the state it carries.
    pub fn vault_coin(&self) -> VaultCoin {
        let assembled = self
            .scenario()
            .assemble(Family::pinned())
            .expect("the scenario assembles");
        VaultCoin {
            entry: assembled.utxos[1].clone(),
            state: self.vault.clone(),
        }
    }

    /// The compiled script of the vesting coin being spent.
    pub fn compiled(&self) -> Compiled {
        Compiled::from_source(&self.source, &self.vesting).expect("the vesting source compiles")
    }

    /// The compiled script of the next vesting coin.
    pub fn next_compiled(&self) -> Option<Compiled> {
        let source = self.next_source.as_deref().unwrap_or(&self.source);
        self.next_vesting.as_ref().map(|state| {
            Compiled::from_source(source, state).expect("the next vesting source compiles")
        })
    }

    /// The transaction as a scenario of the assembler of `kcc20-family`. Inputs and outputs
    /// are in the order of the module's table; a check may still change the scenario before
    /// assembly.
    pub fn scenario(&self) -> Scenario {
        let mut scenario = Scenario::new(self.name.clone());
        scenario.lock_time = self.lock_time;

        // Input 0: the vesting coin.
        let compiled = self.compiled();
        let signer = self.signer.clone();
        let claim = self.claim;
        let call_script = compiled.clone();
        let signature_script = LateBytes::from_tx(move |ctx: &SigningContext<'_>| {
            let signature = match &signer {
                Signer::Key(seed, hash_type) => {
                    keys::schnorr_signature(ctx, &keys::keypair(*seed), *hash_type)
                }
                Signer::Fixed(bytes) => bytes.clone(),
            };
            call_script.release_signature_script(&signature, claim)
        });
        let mut vesting_input = OrdinaryInput::new(
            compiled.script_public_key(),
            self.vesting_value,
            self.vesting_outpoint,
            signature_script,
        )
        .with_covenant_id(self.vesting_coin_covid)
        .labelled("vesting coin: release");
        vesting_input.sequence = self.vesting_sequence;
        scenario.push_ordinary_input(vesting_input);

        // Input 1: the vault, the token's leader on the normal path; owner scheme 0x04
        // takes an empty owner witness.
        let declared = self
            .declared
            .clone()
            .unwrap_or_else(|| self.token_outputs.clone());
        scenario.push_contract_input(
            ContractInput::new(
                self.vault.clone(),
                self.vault_value,
                Some(self.vault_covid),
                self.vault_outpoint,
                Call::typed(
                    Entry::Transfer,
                    vec![
                        Arg::States(declared),
                        Arg::Bytes(LateBytes::fixed(vec![PATH_NORMAL])),
                    ],
                ),
            )
            .labelled("vault: KCC20 transfer"),
        );

        // Input 2: the funder.
        scenario.push_ordinary_input(funding_input(self.funding_outpoint));

        // Outputs.
        for (state, value) in self.token_outputs.iter().zip(&self.token_output_values) {
            scenario.push_contract_output(ContractOutput::new(
                state.clone(),
                *value,
                Some(CovenantBinding::new(1, self.vault_covid)),
            ));
        }
        if let Some(next) = self.next_compiled() {
            let mut output = PlainOutput::new(next.script_public_key(), self.next_vesting_value);
            output.binding = Some(CovenantBinding::new(0, self.vesting_coin_covid));
            scenario.push_plain_output(output);
        }
        let spent = self.vesting_value + self.vault_value + FUNDING_VALUE;
        let created: u64 = scenario.outputs.iter().map(OutputSpec::value).sum();
        let change = spent.saturating_sub(created).saturating_sub(self.fee);
        scenario.push_plain_output(PlainOutput::new(
            pay_to_public_key_script(&keys::x_only(FUNDER)),
            change,
        ));
        scenario
    }
}

/// The funder's coin, spent with a Schnorr signature of the default type.
pub fn funding_input(outpoint: TransactionOutpoint) -> OrdinaryInput {
    OrdinaryInput::new(
        pay_to_public_key_script(&keys::x_only(FUNDER)),
        FUNDING_VALUE,
        outpoint,
        LateBytes::from_tx(|ctx: &SigningContext<'_>| {
            kcc20_family::raw::push(&keys::schnorr_signature(
                ctx,
                &keys::keypair(FUNDER),
                SIG_HASH_ALL,
            ))
            .expect("a 65-byte push fits a script")
        }),
    )
    .labelled("funder")
}
