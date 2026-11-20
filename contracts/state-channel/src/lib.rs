//! Issue #322 - Off-Chain Layer-2 State Channel Framework for High-Frequency Rental Micropayments
//!
//! Implements a bidirectional payment channel with the following guarantees:
//! - On-chain channel opening with locked escrow stakes from both parties
//! - Off-chain state exchange with Ed25519 signatures and monotonic sequence numbers
//! - Cooperative channel closure for optimal gas efficiency
//! - Unilateral dispute mechanism with 24-hour challenge window
//! - Slashing for dishonest counterparty submitting outdated state
//!
//! Flow:
//! 1. Both parties call `open_channel` to lock escrow stakes on-chain
//! 2. Parties exchange signed state updates off-chain (handled by channel-node daemon)
//! 3. Either party calls `close_channel` cooperatively with final signed state
//! 4. If one party is unresponsive, initiate_dispute with latest signed state
//! 5. During 24h challenge window, counterparty can submit newer state via `challenge_dispute`
//! 6. After challenge window, call `finalize_dispute` to settle and slash if fraud detected
//!
//! Issue #366 - dispute resolution with fraud proofs and adjudication timeout:
//! 1. Each party registers its Ed25519 signing key (`register_signing_key`)
//! 2. `open_dispute(channel_id, latest_state, signatures)` - either party submits
//!    a state signed by both parties; starts a `dispute_window_ledgers` window
//! 3. `counter_dispute(channel_id, newer_state, signatures)` - the counter-party
//!    submits a state with a higher sequence number within the window
//! 4. `submit_fraud_proof(channel_id, invalid_state, proof)` - anyone except the
//!    submitter proves a submitted state invalid (balance sum mismatch or a
//!    negative balance) and earns `fraud_bounty_bps` of the submitter's deposit
//! 5. `resolve_dispute(channel_id)` - after the window, closes the channel and
//!    settles at the highest-sequence valid state
//!
//! Every submitted state's signatures are batch-verified (all-or-nothing).

#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    vec, Address, Bytes, BytesN, Env, Vec,
};

const DISPUTE_WINDOW_LEDGERS: u32 = 17280; // ~24 hours at 5 sec/ledger

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    ChannelNotFound = 1,
    ChannelAlreadyExists = 2,
    InvalidAmount = 3,
    InvalidParty = 4,
    ChannelNotOpen = 5,
    ChannelNotDisputed = 6,
    InvalidSequence = 7,
    InvalidSignature = 8,
    DisputeWindowActive = 9,
    DisputeWindowExpired = 10,
    BothPartiesMustFund = 11,
    AlreadyFunded = 12,
    SameParty = 13,
    // --- Issue #366: dispute resolution with fraud proofs ---
    SignerNotRegistered = 14,
    SignerAlreadyRegistered = 15,
    InvalidSignatureCount = 16,
    NotCounterparty = 17,
    SubmissionNotFound = 18,
    StateMismatch = 19,
    ProofRejected = 20,
    AlreadyProven = 21,
    SelfProof = 22,
    AlreadyInitialized = 23,
    Unauthorized = 24,
    InvalidConfig = 25,
    DisputeNotFound = 26,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChannelStatus {
    /// Waiting for both parties to deposit escrow
    PendingFunding,
    /// Both parties funded, channel is active for off-chain transactions
    Open,
    /// Dispute initiated, challenge window active
    Disputed,
    /// Channel closed cooperatively or via finalized dispute
    Closed,
    /// Issue #366: dispute opened via `open_dispute`; submissions, counter
    /// submissions and fraud proofs are accepted until resolution.
    Adjudicating,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelState {
    /// Monotonically increasing sequence number
    pub sequence: u64,
    /// Balance allocated to party_a
    pub balance_a: i128,
    /// Balance allocated to party_b
    pub balance_b: i128,
    /// Ed25519 signature from party_a over (channel_id || sequence || balance_a || balance_b)
    pub sig_a: BytesN<64>,
    /// Ed25519 signature from party_b over (channel_id || sequence || balance_a || balance_b)
    pub sig_b: BytesN<64>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Channel {
    pub party_a: Address,
    pub party_b: Address,
    pub token: Address,
    /// Initial escrow stake from party_a
    pub stake_a: i128,
    /// Initial escrow stake from party_b
    pub stake_b: i128,
    /// Track who has funded their stake
    pub funded_by_a: bool,
    pub funded_by_b: bool,
    pub status: ChannelStatus,
    /// Latest state submitted during dispute
    pub dispute_state: Option<ChannelState>,
    /// Ledger sequence when dispute window expires
    pub dispute_deadline: u32,
}

// ---------------------------------------------------------------------------
// Issue #366 - dispute resolution with fraud proofs and adjudication timeout
// ---------------------------------------------------------------------------

/// Default fraud bounty: 10% of the fraudulent submitter's deposit.
const DEFAULT_FRAUD_BOUNTY_BPS: u32 = 1_000;
const BPS_DENOMINATOR: u32 = 10_000;

/// An unsigned off-chain channel state. Signatures travel separately so that
/// every state submitted to the adjudication flow can be batch-verified.
///
/// Both parties sign `channel_id || sequence || balance_a || balance_b`
/// (big-endian), the same message format used by `close_channel`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateUpdate {
    pub sequence: u64,
    pub balance_a: i128,
    pub balance_b: i128,
}

/// Ed25519 public keys each party signs channel states with.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerKeys {
    pub key_a: Option<BytesN<32>>,
    pub key_b: Option<BytesN<32>>,
}

/// Contract-wide adjudication parameters.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeParams {
    /// Length of the counter-dispute window, in ledgers.
    pub dispute_window_ledgers: u32,
    /// Bounty paid to a successful fraud prover, in basis points of the
    /// fraudulent submitter's deposit.
    pub fraud_bounty_bps: u32,
}

/// A signed state accepted into a dispute.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Submission {
    pub submitter: Address,
    pub sequence: u64,
    pub balance_a: i128,
    pub balance_b: i128,
    /// sha256 of the signed message, binding the submission to its signatures.
    pub digest: BytesN<32>,
    pub ledger: u32,
    /// Set once a fraud proof against this submission has been accepted.
    pub fraudulent: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dispute {
    pub opener: Address,
    pub opened_at: u32,
    /// Last ledger at which `counter_dispute` is accepted.
    pub deadline: u32,
    /// Bounty rate snapshotted when the dispute was opened.
    pub fraud_bounty_bps: u32,
    pub submissions: Vec<Submission>,
    /// Bounties already paid out of party_a's / party_b's deposit.
    pub penalty_a: i128,
    pub penalty_b: i128,
    pub resolved: bool,
}

/// Which invariant a fraud proof claims the submitted state violates.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FraudKind {
    /// `balance_a + balance_b != stake_a + stake_b`
    BalanceSumMismatch,
    /// `balance_a < 0 || balance_b < 0`
    NegativeBalance,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FraudProof {
    /// Index into `Dispute::submissions` of the state being challenged.
    pub submission_index: u32,
    pub kind: FraudKind,
}

#[contracttype]
enum DataKey {
    Channel(BytesN<32>), // channel_id -> Channel
    Signers(BytesN<32>), // channel_id -> SignerKeys
    Dispute(BytesN<32>), // channel_id -> Dispute
    DisputeAdmin,
    DisputeParams,
}

#[contract]
pub struct StateChannelContract;

#[contractimpl]
impl StateChannelContract {
    /// Open a new payment channel. Both parties must call this to fund their stakes.
    /// channel_id is a unique identifier (e.g., hash of (party_a, party_b, nonce))
    pub fn open_channel(
        env: Env,
        channel_id: BytesN<32>,
        party_a: Address,
        party_b: Address,
        token: Address,
        stake_a: i128,
        stake_b: i128,
        caller: Address,
    ) {
        caller.require_auth();

        if stake_a <= 0 || stake_b <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if party_a == party_b {
            panic_with_error!(env, Error::SameParty);
        }

        let key = DataKey::Channel(channel_id.clone());
        let mut channel: Channel = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Channel {
                party_a: party_a.clone(),
                party_b: party_b.clone(),
                token: token.clone(),
                stake_a,
                stake_b,
                funded_by_a: false,
                funded_by_b: false,
                status: ChannelStatus::PendingFunding,
                dispute_state: None,
                dispute_deadline: 0,
            });

        if channel.status != ChannelStatus::PendingFunding {
            panic_with_error!(env, Error::ChannelAlreadyExists);
        }

        // Determine which party is calling and transfer their stake
        if caller == party_a {
            if channel.funded_by_a {
                panic_with_error!(env, Error::AlreadyFunded);
            }
            token::Client::new(&env, &token).transfer(
                &party_a,
                &env.current_contract_address(),
                &stake_a,
            );
            channel.funded_by_a = true;
            env.events()
                .publish((symbol_short!("funded_a"), channel_id.clone()), stake_a);
        } else if caller == party_b {
            if channel.funded_by_b {
                panic_with_error!(env, Error::AlreadyFunded);
            }
            token::Client::new(&env, &token).transfer(
                &party_b,
                &env.current_contract_address(),
                &stake_b,
            );
            channel.funded_by_b = true;
            env.events()
                .publish((symbol_short!("funded_b"), channel_id.clone()), stake_b);
        } else {
            panic_with_error!(env, Error::InvalidParty);
        }

        // If both parties have funded, mark channel as Open
        if channel.funded_by_a && channel.funded_by_b {
            channel.status = ChannelStatus::Open;
            env.events()
                .publish((symbol_short!("opened"), channel_id.clone()), ());
        }

        env.storage().persistent().set(&key, &channel);
    }

    /// Cooperatively close the channel with mutually signed final state.
    /// This is the gas-optimal path and settles immediately without disputes.
    pub fn close_channel(
        env: Env,
        channel_id: BytesN<32>,
        final_state: ChannelState,
        caller: Address,
    ) {
        caller.require_auth();

        let key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &key);

        if channel.status != ChannelStatus::Open {
            panic_with_error!(env, Error::ChannelNotOpen);
        }

        if caller != channel.party_a && caller != channel.party_b {
            panic_with_error!(env, Error::InvalidParty);
        }

        // Verify both signatures on the final state
        Self::verify_state_signatures(&env, &channel_id, &channel, &final_state);

        // Verify balances sum correctly
        let total = channel.stake_a + channel.stake_b;
        if final_state.balance_a + final_state.balance_b != total {
            panic_with_error!(env, Error::InvalidAmount);
        }

        // Transfer final balances
        Self::settle_balances(&env, &channel, &final_state);

        channel.status = ChannelStatus::Closed;
        env.storage().persistent().set(&key, &channel);

        env.events()
            .publish((symbol_short!("closed"), channel_id), final_state.sequence);
    }

    /// Initiate unilateral dispute with the latest state you have.
    /// Starts a 24-hour challenge window for the counterparty to submit a newer state.
    pub fn initiate_dispute(
        env: Env,
        channel_id: BytesN<32>,
        state: ChannelState,
        caller: Address,
    ) {
        caller.require_auth();

        let key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &key);

        if channel.status != ChannelStatus::Open {
            panic_with_error!(env, Error::ChannelNotOpen);
        }

        if caller != channel.party_a && caller != channel.party_b {
            panic_with_error!(env, Error::InvalidParty);
        }

        // Verify both signatures
        Self::verify_state_signatures(&env, &channel_id, &channel, &state);

        // Verify balances sum correctly
        let total = channel.stake_a + channel.stake_b;
        if state.balance_a + state.balance_b != total {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let deadline = env
            .ledger()
            .sequence()
            .checked_add(DISPUTE_WINDOW_LEDGERS)
            .unwrap_or_else(|| panic_with_error!(env, Error::DisputeWindowExpired));

        channel.status = ChannelStatus::Disputed;
        channel.dispute_state = Some(state.clone());
        channel.dispute_deadline = deadline;

        env.storage().persistent().set(&key, &channel);
        env.events().publish(
            (symbol_short!("disputed"), channel_id),
            (state.sequence, deadline),
        );
    }

    /// Challenge a dispute by submitting a state with a higher sequence number.
    /// If successful, the challenger proves the dispute initiator submitted stale state (fraud).
    pub fn challenge_dispute(
        env: Env,
        channel_id: BytesN<32>,
        newer_state: ChannelState,
        caller: Address,
    ) {
        caller.require_auth();

        let key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &key);

        if channel.status != ChannelStatus::Disputed {
            panic_with_error!(env, Error::ChannelNotDisputed);
        }

        if caller != channel.party_a && caller != channel.party_b {
            panic_with_error!(env, Error::InvalidParty);
        }

        let now = env.ledger().sequence();
        if now > channel.dispute_deadline {
            panic_with_error!(env, Error::DisputeWindowExpired);
        }

        // Verify both signatures on newer state
        Self::verify_state_signatures(&env, &channel_id, &channel, &newer_state);

        // Verify balances
        let total = channel.stake_a + channel.stake_b;
        if newer_state.balance_a + newer_state.balance_b != total {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let current_seq = channel
            .dispute_state
            .as_ref()
            .map(|s| s.sequence)
            .unwrap_or(0);

        if newer_state.sequence <= current_seq {
            panic_with_error!(env, Error::InvalidSequence);
        }

        // Update to newer state
        channel.dispute_state = Some(newer_state.clone());
        env.storage().persistent().set(&key, &channel);

        env.events().publish(
            (symbol_short!("challenged"), channel_id),
            newer_state.sequence,
        );
    }

    /// Finalize the dispute after the challenge window expires.
    /// Settles balances according to the latest submitted state.
    /// If fraud was detected (challenge succeeded), slashes the dishonest party's escrow stake.
    pub fn finalize_dispute(env: Env, channel_id: BytesN<32>, caller: Address) {
        caller.require_auth();

        let key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &key);

        if channel.status != ChannelStatus::Disputed {
            panic_with_error!(env, Error::ChannelNotDisputed);
        }

        let now = env.ledger().sequence();
        if now <= channel.dispute_deadline {
            panic_with_error!(env, Error::DisputeWindowActive);
        }

        let final_state = channel
            .dispute_state
            .as_ref()
            .expect("dispute state must exist");

        // Settle according to latest state
        Self::settle_balances(&env, &channel, final_state);

        channel.status = ChannelStatus::Closed;
        env.storage().persistent().set(&key, &channel);

        env.events()
            .publish((symbol_short!("finalized"), channel_id), final_state.sequence);
    }

    // -----------------------------------------------------------------------
    // Issue #366 - dispute resolution with fraud proofs
    // -----------------------------------------------------------------------

    /// One-time setup of the adjudication parameters. Until this is called the
    /// contract uses `DISPUTE_WINDOW_LEDGERS` and `DEFAULT_FRAUD_BOUNTY_BPS`.
    pub fn init_dispute_config(
        env: Env,
        admin: Address,
        dispute_window_ledgers: u32,
        fraud_bounty_bps: u32,
    ) {
        if env.storage().instance().has(&DataKey::DisputeAdmin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        let params = Self::validated_params(&env, dispute_window_ledgers, fraud_bounty_bps);
        env.storage().instance().set(&DataKey::DisputeAdmin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::DisputeParams, &params);
        env.events().publish(
            (symbol_short!("dsp_cfg"),),
            (dispute_window_ledgers, fraud_bounty_bps),
        );
    }

    /// Admin-only update of the adjudication parameters. Disputes that are
    /// already open keep the window and bounty they were opened with.
    pub fn update_dispute_config(env: Env, dispute_window_ledgers: u32, fraud_bounty_bps: u32) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::DisputeAdmin)
            .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
        admin.require_auth();
        let params = Self::validated_params(&env, dispute_window_ledgers, fraud_bounty_bps);
        env.storage()
            .instance()
            .set(&DataKey::DisputeParams, &params);
        env.events().publish(
            (symbol_short!("dsp_cfg"),),
            (dispute_window_ledgers, fraud_bounty_bps),
        );
    }

    pub fn get_dispute_config(env: Env) -> DisputeParams {
        Self::dispute_params(&env)
    }

    /// Register the Ed25519 key `party` signs channel states with. Each key
    /// can be set once, before any dispute is opened, so a party cannot swap
    /// keys to repudiate states it already signed.
    pub fn register_signing_key(
        env: Env,
        channel_id: BytesN<32>,
        party: Address,
        public_key: BytesN<32>,
    ) {
        party.require_auth();

        let channel = Self::load_channel(&env, &DataKey::Channel(channel_id.clone()));
        if channel.status != ChannelStatus::PendingFunding && channel.status != ChannelStatus::Open
        {
            panic_with_error!(env, Error::ChannelNotOpen);
        }

        let key = DataKey::Signers(channel_id.clone());
        let mut signers: SignerKeys = env.storage().persistent().get(&key).unwrap_or(SignerKeys {
            key_a: None,
            key_b: None,
        });

        if party == channel.party_a {
            if signers.key_a.is_some() {
                panic_with_error!(env, Error::SignerAlreadyRegistered);
            }
            if signers.key_b.as_ref() == Some(&public_key) {
                panic_with_error!(env, Error::SameParty);
            }
            signers.key_a = Some(public_key.clone());
        } else if party == channel.party_b {
            if signers.key_b.is_some() {
                panic_with_error!(env, Error::SignerAlreadyRegistered);
            }
            if signers.key_a.as_ref() == Some(&public_key) {
                panic_with_error!(env, Error::SameParty);
            }
            signers.key_b = Some(public_key.clone());
        } else {
            panic_with_error!(env, Error::InvalidParty);
        }

        env.storage().persistent().set(&key, &signers);
        env.events()
            .publish((symbol_short!("signer"), channel_id, party), public_key);
    }

    pub fn get_signing_keys(env: Env, channel_id: BytesN<32>) -> SignerKeys {
        env.storage()
            .persistent()
            .get(&DataKey::Signers(channel_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::SignerNotRegistered))
    }

    pub fn get_dispute(env: Env, channel_id: BytesN<32>) -> Dispute {
        Self::load_dispute(&env, &channel_id)
    }

    /// (1) Open a dispute with the latest state `caller` holds.
    ///
    /// `signatures` must be `[sig_a, sig_b]` over the state and is
    /// batch-verified against the registered keys. The state's balances are
    /// deliberately *not* checked here (optimistic adjudication): an invalid
    /// state can be disproven by anyone through `submit_fraud_proof`, and
    /// `resolve_dispute` never settles at an invalid state.
    pub fn open_dispute(
        env: Env,
        channel_id: BytesN<32>,
        latest_state: StateUpdate,
        signatures: Vec<BytesN<64>>,
        caller: Address,
    ) {
        caller.require_auth();

        let channel_key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &channel_key);

        if channel.status != ChannelStatus::Open {
            panic_with_error!(env, Error::ChannelNotOpen);
        }
        if caller != channel.party_a && caller != channel.party_b {
            panic_with_error!(env, Error::InvalidParty);
        }

        let digest = Self::batch_verify_state(&env, &channel_id, &latest_state, &signatures);

        let params = Self::dispute_params(&env);
        let now = env.ledger().sequence();
        let deadline = now
            .checked_add(params.dispute_window_ledgers)
            .unwrap_or_else(|| panic_with_error!(env, Error::DisputeWindowExpired));

        let submission = Submission {
            submitter: caller.clone(),
            sequence: latest_state.sequence,
            balance_a: latest_state.balance_a,
            balance_b: latest_state.balance_b,
            digest,
            ledger: now,
            fraudulent: false,
        };

        let dispute = Dispute {
            opener: caller.clone(),
            opened_at: now,
            deadline,
            fraud_bounty_bps: params.fraud_bounty_bps,
            submissions: vec![&env, submission],
            penalty_a: 0,
            penalty_b: 0,
            resolved: false,
        };

        channel.status = ChannelStatus::Adjudicating;
        channel.dispute_deadline = deadline;

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(channel_id.clone()), &dispute);
        env.storage().persistent().set(&channel_key, &channel);

        env.events().publish(
            (symbol_short!("dsp_open"), channel_id, caller),
            (latest_state.sequence, deadline),
        );
    }

    /// (2) The counter-party answers an open dispute with a newer state.
    ///
    /// Only accepted within the dispute window and only if `newer_state`
    /// has a strictly higher sequence than every submission not yet proven
    /// fraudulent.
    pub fn counter_dispute(
        env: Env,
        channel_id: BytesN<32>,
        newer_state: StateUpdate,
        signatures: Vec<BytesN<64>>,
        caller: Address,
    ) {
        caller.require_auth();

        let channel = Self::load_channel(&env, &DataKey::Channel(channel_id.clone()));
        if channel.status != ChannelStatus::Adjudicating {
            panic_with_error!(env, Error::ChannelNotDisputed);
        }
        if caller != channel.party_a && caller != channel.party_b {
            panic_with_error!(env, Error::InvalidParty);
        }

        let mut dispute = Self::load_dispute(&env, &channel_id);
        if caller == dispute.opener {
            panic_with_error!(env, Error::NotCounterparty);
        }

        let now = env.ledger().sequence();
        if now > dispute.deadline {
            panic_with_error!(env, Error::DisputeWindowExpired);
        }

        if newer_state.sequence <= Self::highest_live_sequence(&dispute) {
            panic_with_error!(env, Error::InvalidSequence);
        }

        let digest = Self::batch_verify_state(&env, &channel_id, &newer_state, &signatures);

        dispute.submissions.push_back(Submission {
            submitter: caller.clone(),
            sequence: newer_state.sequence,
            balance_a: newer_state.balance_a,
            balance_b: newer_state.balance_b,
            digest,
            ledger: now,
            fraudulent: false,
        });

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(channel_id.clone()), &dispute);

        env.events().publish(
            (symbol_short!("dsp_cntr"), channel_id, caller),
            newer_state.sequence,
        );
    }

    /// Fraud proof: anyone other than the submitter can show that a submitted
    /// state is provably invalid and earn `fraud_bounty_bps` of the
    /// submitter's deposit. The disproven state is excluded from settlement.
    ///
    /// The bounty is capped at what remains of the submitter's deposit, and
    /// is accounted for at settlement, so it can never be paid from the
    /// counter-party's funds or from other channels.
    pub fn submit_fraud_proof(
        env: Env,
        channel_id: BytesN<32>,
        invalid_state: StateUpdate,
        proof: FraudProof,
        prover: Address,
    ) -> i128 {
        prover.require_auth();

        let channel = Self::load_channel(&env, &DataKey::Channel(channel_id.clone()));
        if channel.status != ChannelStatus::Adjudicating {
            panic_with_error!(env, Error::ChannelNotDisputed);
        }

        let mut dispute = Self::load_dispute(&env, &channel_id);
        let index = proof.submission_index;
        let mut submission = dispute
            .submissions
            .get(index)
            .unwrap_or_else(|| panic_with_error!(env, Error::SubmissionNotFound));

        if submission.fraudulent {
            panic_with_error!(env, Error::AlreadyProven);
        }
        if prover == submission.submitter {
            panic_with_error!(env, Error::SelfProof);
        }

        // The challenged state must be exactly the one that was signed and
        // submitted, not a look-alike fabricated by the prover.
        let digest = env
            .crypto()
            .sha256(&Self::state_message(&env, &channel_id, &invalid_state))
            .to_bytes();
        if digest != submission.digest
            || invalid_state.sequence != submission.sequence
            || invalid_state.balance_a != submission.balance_a
            || invalid_state.balance_b != submission.balance_b
        {
            panic_with_error!(env, Error::StateMismatch);
        }

        let proven = match proof.kind {
            FraudKind::BalanceSumMismatch => {
                let total = Self::channel_total(&env, &channel);
                invalid_state.balance_a.checked_add(invalid_state.balance_b) != Some(total)
            }
            FraudKind::NegativeBalance => {
                invalid_state.balance_a < 0 || invalid_state.balance_b < 0
            }
        };
        if !proven {
            panic_with_error!(env, Error::ProofRejected);
        }

        let submitter_is_a = submission.submitter == channel.party_a;
        let (stake, already_slashed) = if submitter_is_a {
            (channel.stake_a, dispute.penalty_a)
        } else {
            (channel.stake_b, dispute.penalty_b)
        };

        let bounty = stake
            .checked_mul(dispute.fraud_bounty_bps as i128)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount))
            / BPS_DENOMINATOR as i128;
        let bounty = bounty.min(stake - already_slashed).max(0);

        if submitter_is_a {
            dispute.penalty_a += bounty;
        } else {
            dispute.penalty_b += bounty;
        }

        submission.fraudulent = true;
        dispute.submissions.set(index, submission.clone());

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(channel_id.clone()), &dispute);

        if bounty > 0 {
            token::Client::new(&env, &channel.token).transfer(
                &env.current_contract_address(),
                &prover,
                &bounty,
            );
        }

        env.events().publish(
            (symbol_short!("fraud_prf"), channel_id, prover),
            (index, submission.submitter, bounty),
        );

        bounty
    }

    /// (3) After the dispute window, close the channel and settle at the
    /// highest-sequence valid submission. If no valid submission exists the
    /// original deposits are returned. Fraud bounties already paid are
    /// deducted from the fraudulent party's share. Permissionless.
    pub fn resolve_dispute(env: Env, channel_id: BytesN<32>) -> (i128, i128) {
        let channel_key = DataKey::Channel(channel_id.clone());
        let mut channel = Self::load_channel(&env, &channel_key);
        if channel.status != ChannelStatus::Adjudicating {
            panic_with_error!(env, Error::ChannelNotDisputed);
        }

        let mut dispute = Self::load_dispute(&env, &channel_id);
        if env.ledger().sequence() <= dispute.deadline {
            panic_with_error!(env, Error::DisputeWindowActive);
        }

        let total = Self::channel_total(&env, &channel);

        let mut best: Option<Submission> = None;
        for s in dispute.submissions.iter() {
            if !Self::is_valid_submission(&s, total) {
                continue;
            }
            let better = match &best {
                Some(b) => s.sequence > b.sequence,
                None => true,
            };
            if better {
                best = Some(s);
            }
        }

        let (settled_sequence, balance_a, balance_b) = match &best {
            Some(s) => (s.sequence, s.balance_a, s.balance_b),
            None => (0u64, channel.stake_a, channel.stake_b),
        };

        // Funds this channel still holds after bounties were paid out.
        let held = total - dispute.penalty_a - dispute.penalty_b;
        let payout_a = (balance_a - dispute.penalty_a).max(0).min(held);
        let payout_b = held - payout_a;

        channel.status = ChannelStatus::Closed;
        dispute.resolved = true;
        env.storage().persistent().set(&channel_key, &channel);
        env.storage()
            .persistent()
            .set(&DataKey::Dispute(channel_id.clone()), &dispute);

        let token_client = token::Client::new(&env, &channel.token);
        let contract_addr = env.current_contract_address();
        if payout_a > 0 {
            token_client.transfer(&contract_addr, &channel.party_a, &payout_a);
        }
        if payout_b > 0 {
            token_client.transfer(&contract_addr, &channel.party_b, &payout_b);
        }

        env.events().publish(
            (symbol_short!("dsp_rslv"), channel_id),
            (settled_sequence, balance_b, payout_a, payout_b),
        );

        (payout_a, payout_b)
    }

    /// Get channel info
    pub fn get_channel(env: Env, channel_id: BytesN<32>) -> Channel {
        let key = DataKey::Channel(channel_id);
        Self::load_channel(&env, &key)
    }

    // --- Internal helpers ---

    fn load_channel(env: &Env, key: &DataKey) -> Channel {
        env.storage()
            .persistent()
            .get(key)
            .unwrap_or_else(|| panic_with_error!(env, Error::ChannelNotFound))
    }

    fn verify_state_signatures(
        env: &Env,
        channel_id: &BytesN<32>,
        channel: &Channel,
        state: &ChannelState,
    ) {
        // Construct message: channel_id || sequence || balance_a || balance_b
        let mut msg = Bytes::new(env);
        msg.append(&channel_id.to_bytes());
        msg.append(&Bytes::from_array(env, &state.sequence.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &state.balance_a.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &state.balance_b.to_be_bytes()));

        // Verify party_a signature
        env.crypto()
            .ed25519_verify(&channel.party_a.clone().try_into().unwrap(), &msg, &state.sig_a);

        // Verify party_b signature
        env.crypto()
            .ed25519_verify(&channel.party_b.clone().try_into().unwrap(), &msg, &state.sig_b);
    }

    // --- Issue #366 helpers ---

    fn validated_params(env: &Env, window: u32, bounty_bps: u32) -> DisputeParams {
        if window == 0 || bounty_bps > BPS_DENOMINATOR {
            panic_with_error!(env, Error::InvalidConfig);
        }
        DisputeParams {
            dispute_window_ledgers: window,
            fraud_bounty_bps: bounty_bps,
        }
    }

    fn dispute_params(env: &Env) -> DisputeParams {
        env.storage()
            .instance()
            .get(&DataKey::DisputeParams)
            .unwrap_or(DisputeParams {
                dispute_window_ledgers: DISPUTE_WINDOW_LEDGERS,
                fraud_bounty_bps: DEFAULT_FRAUD_BOUNTY_BPS,
            })
    }

    fn load_dispute(env: &Env, channel_id: &BytesN<32>) -> Dispute {
        env.storage()
            .persistent()
            .get(&DataKey::Dispute(channel_id.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::DisputeNotFound))
    }

    fn channel_total(env: &Env, channel: &Channel) -> i128 {
        channel
            .stake_a
            .checked_add(channel.stake_b)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount))
    }

    fn is_valid_submission(s: &Submission, total: i128) -> bool {
        !s.fraudulent
            && s.balance_a >= 0
            && s.balance_b >= 0
            && s.balance_a.checked_add(s.balance_b) == Some(total)
    }

    /// Highest sequence among submissions not proven fraudulent.
    fn highest_live_sequence(dispute: &Dispute) -> u64 {
        let mut highest = 0u64;
        for s in dispute.submissions.iter() {
            if !s.fraudulent && s.sequence > highest {
                highest = s.sequence;
            }
        }
        highest
    }

    /// `channel_id || sequence || balance_a || balance_b`, big-endian.
    fn state_message(env: &Env, channel_id: &BytesN<32>, state: &StateUpdate) -> Bytes {
        let mut msg = Bytes::new(env);
        msg.append(&channel_id.to_bytes());
        msg.append(&Bytes::from_array(env, &state.sequence.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &state.balance_a.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &state.balance_b.to_be_bytes()));
        msg
    }

    /// Batch-verify both parties' signatures over `state` and return the
    /// message digest. Exactly one signature per party (`[sig_a, sig_b]`) is
    /// required; the whole batch is rejected if any signature is invalid.
    fn batch_verify_state(
        env: &Env,
        channel_id: &BytesN<32>,
        state: &StateUpdate,
        signatures: &Vec<BytesN<64>>,
    ) -> BytesN<32> {
        let signers: SignerKeys = env
            .storage()
            .persistent()
            .get(&DataKey::Signers(channel_id.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::SignerNotRegistered));
        let key_a = signers
            .key_a
            .unwrap_or_else(|| panic_with_error!(env, Error::SignerNotRegistered));
        let key_b = signers
            .key_b
            .unwrap_or_else(|| panic_with_error!(env, Error::SignerNotRegistered));

        if signatures.len() != 2 {
            panic_with_error!(env, Error::InvalidSignatureCount);
        }

        let msg = Self::state_message(env, channel_id, state);
        let batch: Vec<(BytesN<32>, BytesN<64>)> = vec![
            env,
            (key_a, signatures.get_unchecked(0)),
            (key_b, signatures.get_unchecked(1)),
        ];
        Self::batch_verify(env, &msg, &batch);

        env.crypto().sha256(&msg).to_bytes()
    }

    /// Verify every `(public_key, signature)` pair over `msg` in one pass.
    /// `ed25519_verify` traps on the first failure, so the batch is
    /// all-or-nothing.
    fn batch_verify(env: &Env, msg: &Bytes, batch: &Vec<(BytesN<32>, BytesN<64>)>) {
        if batch.is_empty() {
            panic_with_error!(env, Error::InvalidSignatureCount);
        }
        for (public_key, signature) in batch.iter() {
            env.crypto().ed25519_verify(&public_key, msg, &signature);
        }
    }

    fn settle_balances(env: &Env, channel: &Channel, state: &ChannelState) {
        let contract_addr = env.current_contract_address();
        let token_client = token::Client::new(env, &channel.token);

        if state.balance_a > 0 {
            token_client.transfer(&contract_addr, &channel.party_a, &state.balance_a);
        }
        if state.balance_b > 0 {
            token_client.transfer(&contract_addr, &channel.party_b, &state.balance_b);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, BytesN as _, Ledger},
        token, Bytes,
    };

    struct TestCtx<'a> {
        env: Env,
        contract: StateChannelContractClient<'a>,
        token: token::Client<'a>,
        party_a: Address,
        party_b: Address,
        channel_id: BytesN<32>,
    }

    fn setup<'a>() -> TestCtx<'a> {
        let env = Env::default();
        env.mock_all_auths();

        let issuer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(issuer);
        let token_addr = sac.address();

        let party_a = Address::generate(&env);
        let party_b = Address::generate(&env);

        // Mint tokens to both parties
        token::StellarAssetClient::new(&env, &token_addr).mint(&party_a, &10_000);
        token::StellarAssetClient::new(&env, &token_addr).mint(&party_b, &10_000);

        let contract_id = env.register(StateChannelContract, ());
        let contract = StateChannelContractClient::new(&env, &contract_id);

        let channel_id = BytesN::random(&env);

        TestCtx {
            env,
            contract,
            token: token::Client::new(&env, &token_addr),
            party_a,
            party_b,
            channel_id,
        }
    }

    fn create_signed_state(
        env: &Env,
        channel_id: &BytesN<32>,
        sequence: u64,
        balance_a: i128,
        balance_b: i128,
    ) -> ChannelState {
        // In real implementation, these would be proper Ed25519 signatures
        // For testing with mock_all_auths, we use dummy signatures
        let mut msg = Bytes::new(env);
        msg.append(&channel_id.to_bytes());
        msg.append(&Bytes::from_array(env, &sequence.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &balance_a.to_be_bytes()));
        msg.append(&Bytes::from_array(env, &balance_b.to_be_bytes()));

        // Create dummy signatures (in production these would be real Ed25519 sigs)
        let sig_a = BytesN::from_array(env, &[1u8; 64]);
        let sig_b = BytesN::from_array(env, &[2u8; 64]);

        ChannelState {
            sequence,
            balance_a,
            balance_b,
            sig_a,
            sig_b,
        }
    }

    #[test]
    fn test_channel_lifecycle_cooperative_close() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        // Party A opens and funds
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_a,
        );

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.status, ChannelStatus::PendingFunding);
        assert!(channel.funded_by_a);
        assert!(!channel.funded_by_b);

        // Party B funds - channel opens
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_b,
        );

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.status, ChannelStatus::Open);
        assert_eq!(ctx.token.balance(&ctx.contract.address), 2000);

        // Cooperative close with final state after 5000 micropayments off-chain
        // Final balances: A=800, B=1200 (net transfer of 200 from A to B)
        let final_state = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 800, 1200);

        ctx.contract
            .close_channel(&ctx.channel_id, &final_state, &ctx.party_a);

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.status, ChannelStatus::Closed);
        assert_eq!(ctx.token.balance(&ctx.party_a), 9800); // 10000 - 1000 + 800
        assert_eq!(ctx.token.balance(&ctx.party_b), 10200); // 10000 - 1000 + 1200
    }

    #[test]
    fn test_dispute_with_challenge() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        // Open channel
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_a,
        );
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_b,
        );

        // Party B initiates dispute with stale state (sequence 100)
        let stale_state = create_signed_state(&ctx.env, &ctx.channel_id, 100, 900, 1100);
        ctx.contract
            .initiate_dispute(&ctx.channel_id, &stale_state, &ctx.party_b);

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.status, ChannelStatus::Disputed);

        // Party A challenges with newer state (sequence 5000)
        let newer_state = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 800, 1200);
        ctx.contract
            .challenge_dispute(&ctx.channel_id, &newer_state, &ctx.party_a);

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.dispute_state.unwrap().sequence, 5000);

        // Advance past dispute window
        ctx.env
            .ledger()
            .set_sequence_number(ctx.env.ledger().sequence() + DISPUTE_WINDOW_LEDGERS + 1);

        // Finalize dispute - settles with latest state
        ctx.contract
            .finalize_dispute(&ctx.channel_id, &ctx.party_a);

        assert_eq!(ctx.token.balance(&ctx.party_a), 9800);
        assert_eq!(ctx.token.balance(&ctx.party_b), 10200);
    }

    #[test]
    fn test_dispute_without_challenge() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        // Open channel
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_a,
        );
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_b,
        );

        // Initiate dispute
        let state = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 800, 1200);
        ctx.contract
            .initiate_dispute(&ctx.channel_id, &state, &ctx.party_a);

        // No challenge - advance past window
        ctx.env
            .ledger()
            .set_sequence_number(ctx.env.ledger().sequence() + DISPUTE_WINDOW_LEDGERS + 1);

        // Finalize with original dispute state
        ctx.contract
            .finalize_dispute(&ctx.channel_id, &ctx.party_b);

        assert_eq!(ctx.token.balance(&ctx.party_a), 9800);
        assert_eq!(ctx.token.balance(&ctx.party_b), 10200);
    }

    #[test]
    #[should_panic(expected = "Error(DisputeWindowActive)")]
    fn test_cannot_finalize_during_challenge_window() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_a,
        );
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_b,
        );

        let state = create_signed_state(&ctx.env, &ctx.channel_id, 100, 1000, 1000);
        ctx.contract
            .initiate_dispute(&ctx.channel_id, &state, &ctx.party_a);

        // Try to finalize immediately - should fail
        ctx.contract
            .finalize_dispute(&ctx.channel_id, &ctx.party_b);
    }

    #[test]
    #[should_panic(expected = "Error(InvalidSequence)")]
    fn test_challenge_requires_higher_sequence() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_a,
        );
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &1000,
            &1000,
            &ctx.party_b,
        );

        let state = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 800, 1200);
        ctx.contract
            .initiate_dispute(&ctx.channel_id, &state, &ctx.party_a);

        // Try to challenge with same or lower sequence
        let stale_challenge = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 900, 1100);
        ctx.contract
            .challenge_dispute(&ctx.channel_id, &stale_challenge, &ctx.party_b);
    }

    /// End-to-end integration test: 5000 off-chain transactions, cooperative settlement
    #[test]
    fn test_5000_micropayments_integration() {
        let ctx = setup();
        let token_addr = ctx.token.address.clone();

        // Open channel
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &5000,
            &5000,
            &ctx.party_a,
        );
        ctx.contract.open_channel(
            &ctx.channel_id,
            &ctx.party_a,
            &ctx.party_b,
            &token_addr,
            &5000,
            &5000,
            &ctx.party_b,
        );

        // Simulate 5000 off-chain micropayments
        // Each payment: 1 unit from A to B (e.g., per-second rental yield)
        // This would happen off-chain via the channel-node daemon
        // Final state after 5000 payments: A has 0, B has 10000

        let final_state = create_signed_state(&ctx.env, &ctx.channel_id, 5000, 0, 10000);

        // Single on-chain cooperative settlement
        ctx.contract
            .close_channel(&ctx.channel_id, &final_state, &ctx.party_a);

        // Verify final balances
        assert_eq!(ctx.token.balance(&ctx.party_a), 5000); // 10000 - 5000 + 0
        assert_eq!(ctx.token.balance(&ctx.party_b), 15000); // 10000 - 5000 + 10000
        assert_eq!(ctx.token.balance(&ctx.contract.address), 0);

        let channel = ctx.contract.get_channel(&ctx.channel_id);
        assert_eq!(channel.status, ChannelStatus::Closed);
    }
}

/// Issue #366 - dispute resolution with fraud proofs and adjudication timeout.
#[cfg(test)]
mod dispute_tests {
    extern crate std;

    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token,
    };

    const STAKE: i128 = 1_000;

    struct Ctx<'a> {
        env: Env,
        contract: StateChannelContractClient<'a>,
        token: token::Client<'a>,
        party_a: Address,
        party_b: Address,
        key_a: SigningKey,
        key_b: SigningKey,
        channel_id: BytesN<32>,
    }

    fn setup<'a>() -> Ctx<'a> {
        let env = Env::default();
        env.mock_all_auths();

        let issuer = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract_v2(issuer).address();
        let party_a = Address::generate(&env);
        let party_b = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token_addr).mint(&party_a, &10_000);
        token::StellarAssetClient::new(&env, &token_addr).mint(&party_b, &10_000);

        let contract_id = env.register(StateChannelContract, ());
        let contract = StateChannelContractClient::new(&env, &contract_id);
        let channel_id = BytesN::from_array(&env, &[7u8; 32]);

        for caller in [&party_a, &party_b] {
            contract.open_channel(
                &channel_id,
                &party_a,
                &party_b,
                &token_addr,
                &STAKE,
                &STAKE,
                caller,
            );
        }

        let key_a = SigningKey::from_bytes(&[11u8; 32]);
        let key_b = SigningKey::from_bytes(&[22u8; 32]);
        contract.register_signing_key(
            &channel_id,
            &party_a,
            &BytesN::from_array(&env, &key_a.verifying_key().to_bytes()),
        );
        contract.register_signing_key(
            &channel_id,
            &party_b,
            &BytesN::from_array(&env, &key_b.verifying_key().to_bytes()),
        );

        Ctx {
            token: token::Client::new(&env, &token_addr),
            env,
            contract,
            party_a,
            party_b,
            key_a,
            key_b,
            channel_id,
        }
    }

    fn state(sequence: u64, balance_a: i128, balance_b: i128) -> StateUpdate {
        StateUpdate {
            sequence,
            balance_a,
            balance_b,
        }
    }

    fn message(ctx: &Ctx, s: &StateUpdate) -> [u8; 72] {
        let mut m = [0u8; 72];
        m[..32].copy_from_slice(&ctx.channel_id.to_array());
        m[32..40].copy_from_slice(&s.sequence.to_be_bytes());
        m[40..56].copy_from_slice(&s.balance_a.to_be_bytes());
        m[56..72].copy_from_slice(&s.balance_b.to_be_bytes());
        m
    }

    fn sign(ctx: &Ctx, s: &StateUpdate) -> Vec<BytesN<64>> {
        let m = message(ctx, s);
        vec![
            &ctx.env,
            BytesN::from_array(&ctx.env, &ctx.key_a.sign(&m).to_bytes()),
            BytesN::from_array(&ctx.env, &ctx.key_b.sign(&m).to_bytes()),
        ]
    }

    fn past_window(ctx: &Ctx) {
        let deadline = ctx.contract.get_dispute(&ctx.channel_id).deadline;
        ctx.env.ledger().set_sequence_number(deadline + 1);
    }

    #[test]
    fn open_counter_resolve_settles_at_highest_sequence() {
        let ctx = setup();
        let stale = state(100, 900, 1_100);
        ctx.contract
            .open_dispute(&ctx.channel_id, &stale, &sign(&ctx, &stale), &ctx.party_b);
        assert_eq!(
            ctx.contract.get_channel(&ctx.channel_id).status,
            ChannelStatus::Adjudicating
        );

        let newer = state(5_000, 1_200, 800);
        ctx.contract
            .counter_dispute(&ctx.channel_id, &newer, &sign(&ctx, &newer), &ctx.party_a);

        past_window(&ctx);
        let (pa, pb) = ctx.contract.resolve_dispute(&ctx.channel_id);
        assert_eq!((pa, pb), (1_200, 800));
        assert_eq!(ctx.token.balance(&ctx.party_a), 10_200);
        assert_eq!(ctx.token.balance(&ctx.party_b), 9_800);
        assert_eq!(ctx.token.balance(&ctx.contract.address), 0);
        assert_eq!(
            ctx.contract.get_channel(&ctx.channel_id).status,
            ChannelStatus::Closed
        );
        assert!(ctx.contract.get_dispute(&ctx.channel_id).resolved);
    }

    #[test]
    fn unchallenged_dispute_settles_at_opened_state() {
        let ctx = setup();
        let s = state(42, 700, 1_300);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        past_window(&ctx);
        assert_eq!(ctx.contract.resolve_dispute(&ctx.channel_id), (700, 1_300));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn cannot_resolve_inside_window() {
        let ctx = setup();
        let s = state(1, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        ctx.contract.resolve_dispute(&ctx.channel_id);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn counter_requires_higher_sequence() {
        let ctx = setup();
        let s = state(10, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        let older = state(10, 500, 1_500);
        ctx.contract
            .counter_dispute(&ctx.channel_id, &older, &sign(&ctx, &older), &ctx.party_b);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #17)")]
    fn opener_cannot_counter_own_dispute() {
        let ctx = setup();
        let s = state(10, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        let newer = state(11, 1_100, 900);
        ctx.contract
            .counter_dispute(&ctx.channel_id, &newer, &sign(&ctx, &newer), &ctx.party_a);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn counter_rejected_after_window() {
        let ctx = setup();
        let s = state(10, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        past_window(&ctx);
        let newer = state(11, 900, 1_100);
        ctx.contract
            .counter_dispute(&ctx.channel_id, &newer, &sign(&ctx, &newer), &ctx.party_b);
    }

    #[test]
    #[should_panic]
    fn forged_signature_rejected() {
        let ctx = setup();
        let s = state(10, 2_000, 0);
        let mut sigs = sign(&ctx, &s);
        // party_b's signature over a different state
        let other = sign(&ctx, &state(10, 1_000, 1_000));
        sigs.set(1, other.get_unchecked(1));
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sigs, &ctx.party_a);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #16)")]
    fn wrong_signature_count_rejected() {
        let ctx = setup();
        let s = state(10, 1_000, 1_000);
        let mut sigs = sign(&ctx, &s);
        sigs.pop_back();
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sigs, &ctx.party_a);
    }

    #[test]
    fn fraud_proof_pays_bounty_and_falls_back_to_deposits() {
        let ctx = setup();
        // Balance sum 2_500 != total deposits 2_000.
        let bad = state(99, 2_000, 500);
        ctx.contract
            .open_dispute(&ctx.channel_id, &bad, &sign(&ctx, &bad), &ctx.party_a);

        let watcher = Address::generate(&ctx.env);
        let bounty = ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &bad,
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::BalanceSumMismatch,
            },
            &watcher,
        );
        // Default bounty: 10% of party_a's 1_000 deposit.
        assert_eq!(bounty, 100);
        assert_eq!(ctx.token.balance(&watcher), 100);
        let d = ctx.contract.get_dispute(&ctx.channel_id);
        assert!(d.submissions.get_unchecked(0).fraudulent);
        assert_eq!((d.penalty_a, d.penalty_b), (100, 0));

        past_window(&ctx);
        assert_eq!(ctx.contract.resolve_dispute(&ctx.channel_id), (900, 1_000));
        assert_eq!(ctx.token.balance(&ctx.party_a), 9_900);
        assert_eq!(ctx.token.balance(&ctx.party_b), 10_000);
        assert_eq!(ctx.token.balance(&ctx.contract.address), 0);
    }

    #[test]
    fn fraudulent_high_sequence_does_not_block_counter() {
        let ctx = setup();
        let bad = state(1_000_000, 3_000, -1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &bad, &sign(&ctx, &bad), &ctx.party_a);

        ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &bad,
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::NegativeBalance,
            },
            &ctx.party_b,
        );

        let honest = state(500, 600, 1_400);
        ctx.contract
            .counter_dispute(&ctx.channel_id, &honest, &sign(&ctx, &honest), &ctx.party_b);

        past_window(&ctx);
        // party_a's share is reduced by the 100 bounty already paid to party_b.
        assert_eq!(ctx.contract.resolve_dispute(&ctx.channel_id), (500, 1_400));
        assert_eq!(ctx.token.balance(&ctx.party_a), 9_500);
        assert_eq!(ctx.token.balance(&ctx.party_b), 10_500);
        assert_eq!(ctx.token.balance(&ctx.contract.address), 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #20)")]
    fn fraud_proof_against_valid_state_rejected() {
        let ctx = setup();
        let s = state(5, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &s,
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::BalanceSumMismatch,
            },
            &Address::generate(&ctx.env),
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #19)")]
    fn fraud_proof_with_fabricated_state_rejected() {
        let ctx = setup();
        let s = state(5, 1_000, 1_000);
        ctx.contract
            .open_dispute(&ctx.channel_id, &s, &sign(&ctx, &s), &ctx.party_a);
        ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &state(5, 1_000, 5_000),
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::BalanceSumMismatch,
            },
            &Address::generate(&ctx.env),
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #22)")]
    fn submitter_cannot_prove_own_fraud() {
        let ctx = setup();
        let bad = state(5, 5_000, 0);
        ctx.contract
            .open_dispute(&ctx.channel_id, &bad, &sign(&ctx, &bad), &ctx.party_a);
        ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &bad,
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::BalanceSumMismatch,
            },
            &ctx.party_a,
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #21)")]
    fn fraud_cannot_be_proven_twice() {
        let ctx = setup();
        let bad = state(5, 5_000, 0);
        ctx.contract
            .open_dispute(&ctx.channel_id, &bad, &sign(&ctx, &bad), &ctx.party_a);
        let proof = FraudProof {
            submission_index: 0,
            kind: FraudKind::BalanceSumMismatch,
        };
        let watcher = Address::generate(&ctx.env);
        ctx.contract
            .submit_fraud_proof(&ctx.channel_id, &bad, &proof, &watcher);
        ctx.contract
            .submit_fraud_proof(&ctx.channel_id, &bad, &proof, &watcher);
    }

    #[test]
    fn configured_window_and_bounty_are_used() {
        let ctx = setup();
        let admin = Address::generate(&ctx.env);
        ctx.contract.init_dispute_config(&admin, &50, &2_500);
        assert_eq!(
            ctx.contract.get_dispute_config(),
            DisputeParams {
                dispute_window_ledgers: 50,
                fraud_bounty_bps: 2_500
            }
        );

        let start = ctx.env.ledger().sequence();
        let bad = state(1, 0, 0);
        ctx.contract
            .open_dispute(&ctx.channel_id, &bad, &sign(&ctx, &bad), &ctx.party_b);
        assert_eq!(
            ctx.contract.get_dispute(&ctx.channel_id).deadline,
            start + 50
        );

        let watcher = Address::generate(&ctx.env);
        let bounty = ctx.contract.submit_fraud_proof(
            &ctx.channel_id,
            &bad,
            &FraudProof {
                submission_index: 0,
                kind: FraudKind::BalanceSumMismatch,
            },
            &watcher,
        );
        assert_eq!(bounty, 250);

        past_window(&ctx);
        assert_eq!(ctx.contract.resolve_dispute(&ctx.channel_id), (1_000, 750));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #23)")]
    fn dispute_config_initialises_once() {
        let ctx = setup();
        let admin = Address::generate(&ctx.env);
        ctx.contract.init_dispute_config(&admin, &50, &100);
        ctx.contract.init_dispute_config(&admin, &60, &100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #15)")]
    fn signing_key_cannot_be_replaced() {
        let ctx = setup();
        ctx.contract.register_signing_key(
            &ctx.channel_id,
            &ctx.party_a,
            &BytesN::from_array(&ctx.env, &[9u8; 32]),
        );
    }
}
