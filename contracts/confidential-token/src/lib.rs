//! Confidential asset-token contract for institutional investors.
//!
//! Balances and transfer amounts are Pedersen commitments `C = v*G + r*H` on
//! BLS12-381 G1 (see [`pedersen`]); only the holder knows the openings
//! `(v, r)`. A transfer publishes a commitment to the hidden amount plus an
//! aggregated Bulletproofs range proof (see [`bulletproofs`]) showing that
//! both the amount and the sender's remaining balance are in `[0, 2^64)`.
//! Because commitments are additively homomorphic the contract updates
//! balances without learning any value:
//!
//! ```text
//! available[from] <- available[from] - C_amount     (proven >= 0)
//! pending[to]     <- pending[to]     + C_amount
//! ```
//!
//! Incoming funds land in a separate `pending` commitment and are merged with
//! `apply_pending`. Otherwise an incoming transfer would change the sender's
//! balance commitment between proof generation and submission and invalidate
//! the proof (front-running).
//!
//! Verifying a 2x64-bit aggregated proof is one ~280-term multi-scalar
//! multiplication. On the current Soroban cost model (the host subgroup-checks
//! every MSM input) that is several times the per-transaction CPU limit, so
//! verification can run as a resumable session: `begin_transfer` checks the
//! proof and fixes every Fiat-Shamir scalar, permissionless `verify_step`
//! calls evaluate bounded chunks of the MSM into an accumulator, and `finish`
//! checks the sum is the identity and settles. The one-shot
//! `confidential_transfer` / `burn` run the identical checks in one call for
//! networks whose limits allow it.
//!
//! Compliance is unchanged from the public asset token: both parties must
//! pass the compliance contract's `is_allowed` identity allowlist check, and
//! issuance (`mint`) and redemption (`burn`) amounts stay public, so total
//! supply is always auditable. The recipient learns the transfer opening from
//! the encrypted `memo`, which the contract passes through as opaque bytes.
#![no_std]

pub mod bulletproofs;
pub mod pedersen;
#[cfg(any(test, feature = "testutils"))]
pub mod prover;
#[cfg(test)]
mod test;

use soroban_sdk::crypto::bls12_381::{Fr, G1Affine};
use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Bytes, Env, IntoVal, String, Symbol, Val, Vec, U256,
};

use bulletproofs::Generators;
pub use bulletproofs::RangeProof;
use pedersen::Point;

/// Bit width of every range proof: amounts and balances are in `[0, 2^64)`.
pub const RANGE_BITS: u32 = 64;
/// Commitments proven together in one transfer (amount + remaining balance).
pub const TRANSFER_AGGREGATION: u32 = 2;
/// Vector generators needed for the largest proof the contract accepts.
pub const GENERATOR_COUNT: u32 = RANGE_BITS * TRANSFER_AGGREGATION;
/// Upper bound on the opaque encrypted memo carried by a transfer.
pub const MAX_MEMO_BYTES: u32 = 1024;
/// Lifetime (ledgers, ~1 day) of an unfinished verification session.
pub const SESSION_TTL: u32 = 17_280;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    Paused = 5,
    SenderNotCompliant = 6,
    RecipientNotCompliant = 7,
    /// The range proof did not verify for the given commitments and context.
    InvalidProof = 8,
    /// A supplied commitment is not a valid G1 subgroup point.
    InvalidCommitment = 9,
    /// `extend_generators` has not yet derived all `GENERATOR_COUNT` pairs.
    GeneratorsNotReady = 10,
    /// Total supply would leave `[0, 2^64)`, which the range proofs rely on.
    SupplyCapExceeded = 11,
    SelfTransfer = 12,
    MemoTooLarge = 13,
    SessionNotFound = 14,
    /// The owner's balance changed after the session's proof was bound.
    StaleSession = 15,
    /// `finish` was called before `verify_step` evaluated every term.
    VerificationIncomplete = 16,
}

/// State change a verification session applies once its proof checks out.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)] // contracttype enums cannot box variants
pub enum SessionOp {
    /// `(to, amount_commitment, memo)`
    Transfer(Address, Point, Bytes),
    /// Public redemption amount.
    Burn(i128),
}

/// A range-proof verification in progress. The MSM terms are
/// `G_0..G_{size-1}`, `H_0..H_{size-1}` (stored generators) followed by
/// `points`, each weighted by the matching entry of `scalars`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationSession {
    pub owner: Address,
    pub op: SessionOp,
    /// Owner's nonce when the proof was bound; must be unchanged at `finish`.
    pub nonce: u64,
    /// Owner's new spendable commitment if the session settles.
    pub remaining: Point,
    pub size: u32,
    pub points: Vec<Point>,
    pub scalars: Vec<U256>,
    /// Number of terms already added to `accumulator`.
    pub cursor: u32,
    pub accumulator: Point,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    Compliance,
    Name,
    Symbol,
    Decimals,
    TotalSupply,
    Paused,
    /// Bulletproofs vector generators `G_i` / `H_i`, derived on-chain.
    GensG,
    GensH,
    /// Spendable balance commitment.
    Available(Address),
    /// Received-but-not-yet-applied balance commitment.
    Pending(Address),
    /// Bumped on every change to `Available(holder)`; bound into proofs.
    Nonce(Address),
    NextSession,
    /// Temporary storage: an unfinished `VerificationSession`.
    Session(u64),
}

#[contract]
pub struct ConfidentialTokenContract;

#[contractimpl]
impl ConfidentialTokenContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        name: String,
        symbol: String,
        decimals: u32,
        compliance_contract: Address,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        let store = env.storage().instance();
        store.set(&DataKey::Admin, &admin);
        store.set(&DataKey::Name, &name);
        store.set(&DataKey::Symbol, &symbol);
        store.set(&DataKey::Decimals, &decimals);
        store.set(&DataKey::Compliance, &compliance_contract);
        store.set(&DataKey::TotalSupply, &0u64);
        store.set(&DataKey::Paused, &false);
        env.storage()
            .persistent()
            .set(&DataKey::GensG, &Vec::<Point>::new(&env));
        env.storage()
            .persistent()
            .set(&DataKey::GensH, &Vec::<Point>::new(&env));
    }

    /// Derive the next `count` Bulletproofs generator pairs with
    /// `hash_to_g1`. Permissionless and deterministic, so anyone can finish
    /// the setup, split across as many transactions as the budget needs.
    /// Returns how many pairs exist.
    pub fn extend_generators(env: Env, count: u32) -> u32 {
        Self::require_initialized(&env);
        let mut g: Vec<Point> = env.storage().persistent().get(&DataKey::GensG).unwrap();
        let mut h: Vec<Point> = env.storage().persistent().get(&DataKey::GensH).unwrap();
        let end = g.len().saturating_add(count).min(GENERATOR_COUNT);
        for i in g.len()..end {
            g.push_back(pedersen::vector_generator(&env, b'G', i).to_bytes());
            h.push_back(pedersen::vector_generator(&env, b'H', i).to_bytes());
        }
        env.storage().persistent().set(&DataKey::GensG, &g);
        env.storage().persistent().set(&DataKey::GensH, &h);
        end
    }

    pub fn generators_ready(env: Env) -> bool {
        let g: Vec<Point> = env
            .storage()
            .persistent()
            .get(&DataKey::GensG)
            .unwrap_or(Vec::new(&env));
        g.len() >= GENERATOR_COUNT
    }

    /// Issue `amount` publicly to `to`. The amount is committed with zero
    /// blinding (`amount*G`) into the recipient's pending balance; issuance
    /// stays visible so total supply is auditable.
    #[allow(deprecated)] // Match the event style used across the contract suite.
    pub fn mint(env: Env, admin: Address, to: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        Self::require_not_paused(&env);
        let amount = Self::public_amount(&env, amount);
        if !Self::compliance_allows(&env, &to) {
            panic_with_error!(env, Error::RecipientNotCompliant);
        }
        let supply: u64 = env.storage().instance().get(&DataKey::TotalSupply).unwrap();
        let supply = supply
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::SupplyCapExceeded));
        env.storage().instance().set(&DataKey::TotalSupply, &supply);

        let pending = pedersen::add(
            &env,
            &Self::pending_balance(env.clone(), to.clone()),
            &pedersen::public_amount(&env, amount),
        );
        env.storage()
            .persistent()
            .set(&DataKey::Pending(to.clone()), &pending);
        env.events()
            .publish((symbol_short!("mint"), to), amount as i128);
    }

    /// Move a hidden amount from `from` to `to`, verifying in one call.
    ///
    /// `proof` must be an aggregated range proof, bound to
    /// `transfer_context(from, to)`, over the two commitments
    /// `[amount_commitment, available(from) - amount_commitment]`. See the
    /// crate docs for when to use `begin_transfer` instead.
    pub fn confidential_transfer(
        env: Env,
        from: Address,
        to: Address,
        amount_commitment: Point,
        proof: RangeProof,
        memo: Bytes,
    ) {
        let mut session = Self::open_transfer(&env, from, to, amount_commitment, &proof, memo);
        let total = session.scalars.len();
        Self::accumulate(&env, &mut session, total);
        Self::settle(&env, session);
    }

    /// Start a transfer whose proof is verified across several transactions.
    /// Performs every check of `confidential_transfer` except the final MSM
    /// and returns the session id for `verify_step` / `finish`.
    pub fn begin_transfer(
        env: Env,
        from: Address,
        to: Address,
        amount_commitment: Point,
        proof: RangeProof,
        memo: Bytes,
    ) -> u64 {
        let session = Self::open_transfer(&env, from, to, amount_commitment, &proof, memo);
        Self::store_new_session(&env, &session)
    }

    /// Redeem a public `amount` from the spendable balance, verifying in one
    /// call. `proof` is a single range proof, bound to
    /// `burn_context(from, amount)`, over `available(from) - amount*G`,
    /// proving the balance stays non-negative.
    pub fn burn(env: Env, from: Address, amount: i128, proof: RangeProof) {
        let mut session = Self::open_burn(&env, from, amount, &proof);
        let total = session.scalars.len();
        Self::accumulate(&env, &mut session, total);
        Self::settle(&env, session);
    }

    /// Multi-transaction variant of `burn`.
    pub fn begin_burn(env: Env, from: Address, amount: i128, proof: RangeProof) -> u64 {
        let session = Self::open_burn(&env, from, amount, &proof);
        Self::store_new_session(&env, &session)
    }

    /// Evaluate up to `max_points` more MSM terms of a session. Permissionless:
    /// the result is fully determined by the stored session, so anyone may
    /// pay to advance it. Returns the number of terms still outstanding.
    pub fn verify_step(env: Env, session_id: u64, max_points: u32) -> u32 {
        let mut session = Self::load_session(&env, session_id);
        let total = session.scalars.len();
        let end = session.cursor.saturating_add(max_points).min(total);
        Self::accumulate(&env, &mut session, end);
        let key = DataKey::Session(session_id);
        env.storage().temporary().set(&key, &session);
        env.storage()
            .temporary()
            .extend_ttl(&key, SESSION_TTL, SESSION_TTL);
        total - session.cursor
    }

    /// Settle a fully evaluated session. Permissionless: the owner authorized
    /// the operation in `begin_*`, and settlement re-checks compliance, the
    /// pause flag and that the owner's balance has not moved since.
    pub fn finish(env: Env, session_id: u64) {
        let session = Self::load_session(&env, session_id);
        if session.cursor < session.scalars.len() {
            panic_with_error!(env, Error::VerificationIncomplete);
        }
        env.storage()
            .temporary()
            .remove(&DataKey::Session(session_id));
        Self::settle(&env, session);
    }

    pub fn get_session(env: Env, session_id: u64) -> Option<VerificationSession> {
        env.storage().temporary().get(&DataKey::Session(session_id))
    }

    /// Merge the pending balance into the spendable balance.
    #[allow(deprecated)] // Match the event style used across the contract suite.
    pub fn apply_pending(env: Env, holder: Address) {
        holder.require_auth();
        Self::require_not_paused(&env);
        let pending = Self::pending_balance(env.clone(), holder.clone());
        let available = Self::available_balance(env.clone(), holder.clone());
        Self::set_available(&env, &holder, &pedersen::add(&env, &available, &pending));
        env.storage()
            .persistent()
            .set(&DataKey::Pending(holder.clone()), &pedersen::identity(&env));
        env.events()
            .publish((Symbol::new(&env, "apply_pending"), holder), pending);
    }

    /// Fiat-Shamir context a transfer proof must be bound to: this contract,
    /// both parties, the sender's nonce and current spendable commitment.
    pub fn transfer_context(env: Env, from: Address, to: Address) -> Bytes {
        (
            symbol_short!("transfer"),
            env.current_contract_address(),
            from.clone(),
            to,
            Self::nonce(env.clone(), from.clone()),
            Self::available_balance(env.clone(), from),
        )
            .to_xdr(&env)
    }

    /// Fiat-Shamir context a burn proof must be bound to.
    pub fn burn_context(env: Env, from: Address, amount: i128) -> Bytes {
        (
            symbol_short!("burn"),
            env.current_contract_address(),
            from.clone(),
            amount,
            Self::nonce(env.clone(), from.clone()),
            Self::available_balance(env.clone(), from),
        )
            .to_xdr(&env)
    }

    pub fn available_balance(env: Env, holder: Address) -> Point {
        env.storage()
            .persistent()
            .get(&DataKey::Available(holder))
            .unwrap_or_else(|| pedersen::identity(&env))
    }

    pub fn pending_balance(env: Env, holder: Address) -> Point {
        env.storage()
            .persistent()
            .get(&DataKey::Pending(holder))
            .unwrap_or_else(|| pedersen::identity(&env))
    }

    pub fn nonce(env: Env, holder: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::Nonce(holder))
            .unwrap_or(0)
    }

    pub fn total_supply(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    pub fn name(env: Env) -> String {
        Self::require_initialized(&env);
        env.storage().instance().get(&DataKey::Name).unwrap()
    }

    pub fn symbol(env: Env) -> String {
        Self::require_initialized(&env);
        env.storage().instance().get(&DataKey::Symbol).unwrap()
    }

    pub fn decimals(env: Env) -> u32 {
        Self::require_initialized(&env);
        env.storage().instance().get(&DataKey::Decimals).unwrap()
    }

    pub fn compliance_contract(env: Env) -> Address {
        Self::require_initialized(&env);
        env.storage().instance().get(&DataKey::Compliance).unwrap()
    }

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Paused, &true);
    }

    pub fn unpause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
    }

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::NotInitialized);
        }
    }

    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        if stored != *admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_not_paused(env: &Env) {
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            panic_with_error!(env, Error::Paused);
        }
    }

    fn public_amount(env: &Env, amount: i128) -> u64 {
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        u64::try_from(amount).unwrap_or_else(|_| panic_with_error!(env, Error::SupplyCapExceeded))
    }

    fn set_available(env: &Env, holder: &Address, commitment: &Point) {
        env.storage()
            .persistent()
            .set(&DataKey::Available(holder.clone()), commitment);
        let nonce = Self::nonce(env.clone(), holder.clone()) + 1;
        env.storage()
            .persistent()
            .set(&DataKey::Nonce(holder.clone()), &nonce);
    }

    fn generators(env: &Env) -> Generators {
        let g: Vec<Point> = env
            .storage()
            .persistent()
            .get(&DataKey::GensG)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let h: Vec<Point> = env.storage().persistent().get(&DataKey::GensH).unwrap();
        if g.len() < GENERATOR_COUNT {
            panic_with_error!(env, Error::GeneratorsNotReady);
        }
        Generators { g, h }
    }

    fn open_transfer(
        env: &Env,
        from: Address,
        to: Address,
        amount_commitment: Point,
        proof: &RangeProof,
        memo: Bytes,
    ) -> VerificationSession {
        from.require_auth();
        Self::require_not_paused(env);
        if from == to {
            panic_with_error!(env, Error::SelfTransfer);
        }
        if memo.len() > MAX_MEMO_BYTES {
            panic_with_error!(env, Error::MemoTooLarge);
        }
        Self::require_compliant(env, &from, &to);
        if !pedersen::is_valid_point(env, &amount_commitment) {
            panic_with_error!(env, Error::InvalidCommitment);
        }

        let available = Self::available_balance(env.clone(), from.clone());
        let remaining = pedersen::sub(env, &available, &amount_commitment);
        let mut commitments = Vec::new(env);
        commitments.push_back(amount_commitment.clone());
        commitments.push_back(remaining.clone());
        let context = Self::transfer_context(env.clone(), from.clone(), to.clone());
        Self::open_session(
            env,
            from,
            SessionOp::Transfer(to, amount_commitment, memo),
            remaining,
            &commitments,
            proof,
            &context,
        )
    }

    fn open_burn(
        env: &Env,
        from: Address,
        amount: i128,
        proof: &RangeProof,
    ) -> VerificationSession {
        from.require_auth();
        Self::require_not_paused(env);
        let amount_u64 = Self::public_amount(env, amount);
        if !Self::compliance_allows(env, &from) {
            panic_with_error!(env, Error::SenderNotCompliant);
        }

        let available = Self::available_balance(env.clone(), from.clone());
        let remaining = pedersen::sub(env, &available, &pedersen::public_amount(env, amount_u64));
        let mut commitments = Vec::new(env);
        commitments.push_back(remaining.clone());
        let context = Self::burn_context(env.clone(), from.clone(), amount);
        Self::open_session(
            env,
            from,
            SessionOp::Burn(amount),
            remaining,
            &commitments,
            proof,
            &context,
        )
    }

    fn open_session(
        env: &Env,
        owner: Address,
        op: SessionOp,
        remaining: Point,
        commitments: &Vec<Point>,
        proof: &RangeProof,
        context: &Bytes,
    ) -> VerificationSession {
        let generator_count = Self::generators(env).g.len();
        let check = bulletproofs::prepare(
            env,
            generator_count,
            commitments,
            proof,
            RANGE_BITS,
            context,
        )
        .unwrap_or_else(|_| panic_with_error!(env, Error::InvalidProof));
        let mut scalars = Vec::new(env);
        for k in check.scalars.iter() {
            scalars.push_back(k.to_u256());
        }
        VerificationSession {
            nonce: Self::nonce(env.clone(), owner.clone()),
            owner,
            op,
            remaining,
            size: check.size,
            points: check.points,
            scalars,
            cursor: 0,
            accumulator: pedersen::identity(env),
        }
    }

    fn store_new_session(env: &Env, session: &VerificationSession) -> u64 {
        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextSession)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextSession, &(id + 1));
        let key = DataKey::Session(id);
        env.storage().temporary().set(&key, session);
        env.storage()
            .temporary()
            .extend_ttl(&key, SESSION_TTL, SESSION_TTL);
        id
    }

    fn load_session(env: &Env, session_id: u64) -> VerificationSession {
        env.storage()
            .temporary()
            .get(&DataKey::Session(session_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::SessionNotFound))
    }

    /// Add MSM terms `cursor..end` into the session accumulator.
    fn accumulate(env: &Env, session: &mut VerificationSession, end: u32) {
        if end <= session.cursor {
            return;
        }
        let gens = Self::generators(env);
        let mut scalars: Vec<Fr> = Vec::new(env);
        for k in session.scalars.slice(session.cursor..end).iter() {
            scalars.push_back(Fr::from_u256(k));
        }
        let partial = bulletproofs::msm_chunk(
            env,
            &gens,
            session.size,
            &session.points,
            session.cursor,
            scalars,
        );
        session.accumulator = env
            .crypto()
            .bls12_381()
            .g1_add(&G1Affine::from_bytes(session.accumulator.clone()), &partial)
            .to_bytes();
        session.cursor = end;
    }

    /// Final checks and the state change of a fully evaluated session.
    #[allow(deprecated)] // Match the event style used across the contract suite.
    fn settle(env: &Env, session: VerificationSession) {
        if session.accumulator != pedersen::identity(env) {
            panic_with_error!(env, Error::InvalidProof);
        }
        Self::require_not_paused(env);
        if Self::nonce(env.clone(), session.owner.clone()) != session.nonce {
            panic_with_error!(env, Error::StaleSession);
        }
        let from = session.owner;
        match session.op {
            SessionOp::Transfer(to, amount_commitment, memo) => {
                Self::require_compliant(env, &from, &to);
                Self::set_available(env, &from, &session.remaining);
                let pending = pedersen::add(
                    env,
                    &Self::pending_balance(env.clone(), to.clone()),
                    &amount_commitment,
                );
                env.storage()
                    .persistent()
                    .set(&DataKey::Pending(to.clone()), &pending);
                env.events().publish(
                    (Symbol::new(env, "conf_transfer"), from, to),
                    (amount_commitment, memo),
                );
            }
            SessionOp::Burn(amount) => {
                if !Self::compliance_allows(env, &from) {
                    panic_with_error!(env, Error::SenderNotCompliant);
                }
                Self::set_available(env, &from, &session.remaining);
                let supply: u64 = env.storage().instance().get(&DataKey::TotalSupply).unwrap();
                // Cannot underflow: every unit of a proven non-negative
                // balance was minted, and `amount` fit in u64 at `begin`.
                env.storage()
                    .instance()
                    .set(&DataKey::TotalSupply, &(supply - amount as u64));
                env.events().publish((symbol_short!("burn"), from), amount);
            }
        }
    }

    fn require_compliant(env: &Env, from: &Address, to: &Address) {
        if !Self::compliance_allows(env, from) {
            panic_with_error!(env, Error::SenderNotCompliant);
        }
        if !Self::compliance_allows(env, to) {
            panic_with_error!(env, Error::RecipientNotCompliant);
        }
    }

    /// Same identity allowlist gate as the public asset token.
    fn compliance_allows(env: &Env, who: &Address) -> bool {
        let compliance: Address = env
            .storage()
            .instance()
            .get(&DataKey::Compliance)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        let args: Vec<Val> = (who.clone(),).into_val(env);
        env.invoke_contract(&compliance, &Symbol::new(env, "is_allowed"), args)
    }
}
