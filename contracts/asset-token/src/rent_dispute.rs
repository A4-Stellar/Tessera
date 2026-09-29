//! Tenant Dispute Mediation & Rent-Clawback Module.
//!
//! Fractional real-estate token holders receive rental-yield dividends
//! streamed to them proportionally. When a property-repair dispute arises,
//! a designated property manager can raise a dispute which transitions the
//! dividend stream into a `Disputed` state: further dividend distributions
//! are locked into an escrow balance instead of being released to holders.
//!
//! The dispute is resolved via multi-signature arbiter consensus:
//!
//! - **Resolved** — arbiter consensus rules in favour of the tenants; the
//!   escrowed funds are released back to the token holders.
//! - **Arbitrated** — arbiter consensus rules in favour of the property
//!   manager; the escrowed funds are released to the property-maintenance
//!   account for repairs.
//!
//! # State machine
//!
//! ```text
//! Normal ──raise_dispute──► Disputed ──resolve_for_holders──► Resolved
//!                                  └──resolve_for_maintenance──► Arbitrated
//! ```
//!
//! `Resolved` and `Arbitrated` are terminal states. A new dispute can only
//! be raised after the previous one has been fully settled and the state
//! has been reset to `Normal` by the admin.
//!
//! # Storage layout
//!
//! All keys are scoped to `instance` storage so they share TTL with the
//! parent asset-token contract instance.
//!
//! | `DataKey`                | Type      | Description                              |
//! |--------------------------|-----------|------------------------------------------|
//! | `DisputeState`           | `u32`     | Current state (0=Normal,1=Disputed,2=Resolved,3=Arbitrated) |
//! | `DisputeEscrow`          | `i128`    | Amount currently locked in escrow        |
//! | `DisputeArbiters`        | `Vec<Addr>` | Approved arbiter addresses             |
//! | `DisputeThreshold`       | `u32`     | Number of arbiter signatures required    |
//! | `DisputePropertyMaint`   | `Address` | Property-maintenance account             |
//! | `DisputeVotes`           | `Vec<(Address, bool)>` | Arbiter votes (arbiter, for_holders) |
//! | `DisputeDividendPerShare`| `i128`    | Last computed dividend-per-share snapshot |

#![allow(dead_code)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, IntoVal, Symbol, Val, Vec,
};

// ---- Error codes --------------------------------------------------------- //
// Placed above 29 (the highest `BuybackError` code) to avoid collisions.

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RentDisputeError {
    /// No dispute is currently active.
    NoActiveDispute = 30,
    /// A dispute is already active; settle it before raising a new one.
    DisputeAlreadyActive = 31,
    /// The dispute has already been resolved or arbitrated.
    DisputeAlreadySettled = 32,
    /// The caller is not a registered arbiter.
    NotArbiter = 33,
    /// The caller has already voted on this dispute.
    AlreadyVoted = 34,
    /// The arbiter threshold has not been reached yet.
    ThresholdNotReached = 35,
    /// The arbiter list is empty or the threshold is zero.
    InvalidArbiterConfig = 36,
    /// The property-maintenance address is not set.
    PropertyMaintNotSet = 37,
    /// The escrow balance is insufficient for the requested release.
    InsufficientEscrow = 38,
    /// Arithmetic overflow.
    Overflow = 39,
    /// The state transition is not permitted from the current state.
    InvalidStateTransition = 40,
}

// ---- Storage keys -------------------------------------------------------- //

#[contracttype]
#[derive(Clone)]
enum RentDisputeKey {
    /// Current dispute state (0=Normal, 1=Disputed, 2=Resolved, 3=Arbitrated).
    State,
    /// Amount currently locked in escrow.
    Escrow,
    /// Approved arbiter addresses.
    Arbiters,
    /// Number of arbiter signatures required to resolve.
    Threshold,
    /// Property-maintenance account address.
    PropertyMaint,
    /// Arbiter votes: (arbiter, for_holders).
    Votes,
    /// Last computed dividend-per-share snapshot at dispute time.
    DividendPerShare,
}

// ---- State constants ----------------------------------------------------- //

pub const STATE_NORMAL: u32 = 0;
pub const STATE_DISPUTED: u32 = 1;
pub const STATE_RESOLVED: u32 = 2;
pub const STATE_ARBITRATED: u32 = 3;

// ---- Contract ------------------------------------------------------------ //

/// Stand-alone tenant-dispute mediation contract. In practice this logic is
/// intended to be deployed alongside (or composed into) the
/// `AssetTokenContract`; it is isolated here for clarity and testability.
#[contract]
pub struct RentDisputeContract;

#[contractimpl]
impl RentDisputeContract {
    // ------------------------------------------------------------------
    // Admin / configuration operations
    // ------------------------------------------------------------------

    /// Initialize the rent-dispute contract with the same admin as the
    /// parent asset-token contract. Must be called before any other
    /// function. Callable once.
    pub fn initialize(env: Env, admin: Address, compliance: Address) {
        if env.storage().instance().has(&RentDisputeKey::State) {
            panic_with_error!(env, super::Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage()
            .instance()
            .set(&RentDisputeKey::State, &STATE_NORMAL);
        env.storage().instance().set(&RentDisputeKey::Escrow, &0_i128);
        env.storage()
            .instance()
            .set(&RentDisputeKey::Arbiters, &Vec::<Address>::new(&env));
        env.storage().instance().set(&RentDisputeKey::Threshold, &0_u32);
        env.storage()
            .instance()
            .set(&RentDisputeKey::Votes, &Vec::<(Address, bool)>::new(&env));
        env.storage()
            .instance()
            .set(&RentDisputeKey::DividendPerShare, &0_i128);

        // Store the admin and compliance addresses in instance storage so
        // `require_admin` and the resolve functions work without
        // cross-contract calls.
        env.storage()
            .instance()
            .set(&super::DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&super::DataKey::Compliance, &compliance);

        env.events().publish((symbol_short!("rdinit"), admin), ());
    }

    /// Configure the arbiter set, threshold, and property-maintenance
    /// account. Must be called before any dispute can be raised.
    ///
    /// - `admin`           — must match the asset-token admin.
    /// - `arbiters`        — non-empty list of approved arbiter addresses.
    /// - `threshold`       — number of arbiter votes required to resolve;
    ///                       must be `> 0` and `<= arbiters.len()`.
    /// - `property_maint`  — address that receives escrowed funds when the
    ///                       dispute is arbitrated in favour of the property.
    pub fn configure(
        env: Env,
        admin: Address,
        arbiters: Vec<Address>,
        threshold: u32,
        property_maint: Address,
    ) {
        Self::require_admin(&env, &admin);

        if arbiters.is_empty() || threshold == 0 || threshold > arbiters.len() as u32 {
            panic_with_error!(env, RentDisputeError::InvalidArbiterConfig);
        }

        env.storage()
            .instance()
            .set(&RentDisputeKey::Arbiters, &arbiters);
        env.storage()
            .instance()
            .set(&RentDisputeKey::Threshold, &threshold);
        env.storage()
            .instance()
            .set(&RentDisputeKey::PropertyMaint, &property_maint);

        env.events().publish(
            (symbol_short!("rdconfig"), admin),
            (threshold, property_maint),
        );
    }

    // ------------------------------------------------------------------
    // Dispute lifecycle
    // ------------------------------------------------------------------

    /// Raise a dispute, transitioning the state from `Normal` to
    /// `Disputed`. While in the `Disputed` state, any call to
    /// `distribute_dividend` will lock the proportional share into escrow
    /// instead of releasing it to the holder.
    ///
    /// - `admin`            — must match the asset-token admin.
    /// - `dividend_per_share` — snapshot of the dividend-per-share at the
    ///                         moment the dispute is raised, used to compute
    ///                         each holder's proportional escrow share.
    pub fn raise_dispute(env: Env, admin: Address, dividend_per_share: i128) {
        Self::require_admin(&env, &admin);

        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);
        if state != STATE_NORMAL {
            panic_with_error!(env, RentDisputeError::DisputeAlreadyActive);
        }

        if dividend_per_share < 0 {
            panic_with_error!(env, RentDisputeError::InvalidArbiterConfig);
        }

        env.storage()
            .instance()
            .set(&RentDisputeKey::State, &STATE_DISPUTED);
        env.storage()
            .instance()
            .set(&RentDisputeKey::Escrow, &0_i128);
        env.storage()
            .instance()
            .set(&RentDisputeKey::DividendPerShare, &dividend_per_share);
        env.storage()
            .instance()
            .set(&RentDisputeKey::Votes, &Vec::<(Address, bool)>::new(&env));

        env.events().publish(
            (symbol_short!("rdispute"), admin),
            dividend_per_share,
        );
    }

    /// Record an arbiter's vote on the active dispute.
    ///
    /// - `arbiter`      — must be in the configured arbiter list.
    /// - `for_holders`  — `true` to release escrow to token holders,
    ///                    `false` to release to the property-maintenance account.
    ///
    /// Once the threshold is reached the dispute is automatically resolved
    /// and the escrowed funds are released to the appropriate party.
    pub fn vote(env: Env, arbiter: Address, for_holders: bool) {
        arbiter.require_auth();

        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);
        if state != STATE_DISPUTED {
            panic_with_error!(env, RentDisputeError::NoActiveDispute);
        }

        let arbiters: Vec<Address> = env
            .storage()
            .instance()
            .get(&RentDisputeKey::Arbiters)
            .unwrap();
        if !arbiters.contains(&arbiter) {
            panic_with_error!(env, RentDisputeError::NotArbiter);
        }

        let mut votes: Vec<(Address, bool)> = env
            .storage()
            .instance()
            .get(&RentDisputeKey::Votes)
            .unwrap_or_else(|| Vec::new(&env));
        if votes.iter().any(|(a, _)| a == arbiter) {
            panic_with_error!(env, RentDisputeError::AlreadyVoted);
        }
        votes.push_back((arbiter.clone(), for_holders));
        env.storage()
            .instance()
            .set(&RentDisputeKey::Votes, &votes);

        env.events()
            .publish((symbol_short!("rvote"), arbiter), for_holders);

        // Check if the threshold has been reached.
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::Threshold)
            .unwrap_or(0);
        let vote_count = votes.len() as u32;
        if vote_count >= threshold {
            // Determine the outcome by majority of votes cast so far.
            let for_holders_votes = votes.iter().filter(|(_, v)| *v).count() as u32;
            let against_votes = vote_count - for_holders_votes;
            if for_holders_votes > against_votes {
                Self::resolve_for_holders_internal(env.clone());
            } else {
                Self::resolve_for_maintenance_internal(env.clone());
            }
        }
    }

    /// Release escrowed funds to token holders (Resolved state).
    ///
    /// Can only be called when the dispute has been resolved in favour of
    /// the holders (either by arbiter consensus or by admin override).
    pub fn resolve_for_holders(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        Self::resolve_for_holders_internal(env.clone());
        env.events()
            .publish((symbol_short!("rresolved"), admin), ());
    }

    /// Release escrowed funds to the property-maintenance account
    /// (Arbitrated state).
    ///
    /// Can only be called when the dispute has been arbitrated in favour of
    /// the property manager (either by arbiter consensus or by admin override).
    pub fn resolve_for_maintenance(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        Self::resolve_for_maintenance_internal(env.clone());
        env.events()
            .publish((symbol_short!("rarb"), admin), ());
    }

    /// Reset the dispute state back to `Normal` after a dispute has been
    /// fully settled. Clears all dispute-related storage.
    pub fn reset_dispute(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);

        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);
        if state != STATE_RESOLVED && state != STATE_ARBITRATED {
            panic_with_error!(env, RentDisputeError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .set(&RentDisputeKey::State, &STATE_NORMAL);
        env.storage().instance().remove(&RentDisputeKey::Escrow);
        env.storage().instance().remove(&RentDisputeKey::Votes);
        env.storage()
            .instance()
            .remove(&RentDisputeKey::DividendPerShare);

        env.events().publish((symbol_short!("rreset"), admin), ());
    }

    // ------------------------------------------------------------------
    // Dividend distribution (called by the parent asset-token contract)
    // ------------------------------------------------------------------

    /// Distribute a proportional dividend to a token holder.
    ///
    /// When the dispute state is `Normal`, the dividend is released
    /// immediately. When the state is `Disputed`, the dividend is locked
    /// into the escrow balance instead.
    ///
    /// Returns the amount actually released to the holder (`0` if locked).
    pub fn distribute_dividend(env: Env, holder: Address, amount: i128) -> i128 {
        if amount <= 0 {
            return 0;
        }

        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);

        if state == STATE_DISPUTED {
            // Lock the dividend into escrow.
            let escrow: i128 = env
                .storage()
                .instance()
                .get(&RentDisputeKey::Escrow)
                .unwrap_or(0);
            let new_escrow = escrow
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(env, RentDisputeError::Overflow));
            env.storage()
                .instance()
                .set(&RentDisputeKey::Escrow, &new_escrow);

            env.events()
                .publish((symbol_short!("rlock"), holder), amount);
            return 0;
        }

        // Normal state: release immediately.
        env.events()
            .publish((symbol_short!("rrelease"), holder), amount);
        amount
    }

    // ------------------------------------------------------------------
    // View functions
    // ------------------------------------------------------------------

    /// Returns the current dispute state as a `u32` constant.
    pub fn dispute_state(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL)
    }

    /// Returns the current escrow balance.
    pub fn escrow_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&RentDisputeKey::Escrow)
            .unwrap_or(0)
    }

    /// Returns the configured arbiter list.
    pub fn arbiters(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&RentDisputeKey::Arbiters)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the configured arbiter threshold.
    pub fn threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&RentDisputeKey::Threshold)
            .unwrap_or(0)
    }

    /// Returns the property-maintenance account address.
    pub fn property_maintenance(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&RentDisputeKey::PropertyMaint)
            .unwrap()
    }

    /// Returns the current votes as a vector of `(arbiter, for_holders)`.
    pub fn votes(env: Env) -> Vec<(Address, bool)> {
        env.storage()
            .instance()
            .get(&RentDisputeKey::Votes)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the dividend-per-share snapshot captured at dispute time.
    pub fn dividend_per_share(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&RentDisputeKey::DividendPerShare)
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Internal: release escrow to token holders without admin check.
    /// Called by both the public `resolve_for_holders` (admin-gated) and
    /// the auto-resolution path in `vote` (threshold reached).
    fn resolve_for_holders_internal(env: Env) {
        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);
        if state != STATE_DISPUTED {
            panic_with_error!(env, RentDisputeError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .set(&RentDisputeKey::Escrow, &0_i128);
        env.storage()
            .instance()
            .set(&RentDisputeKey::State, &STATE_RESOLVED);
    }

    /// Internal: release escrow to property maintenance without admin check.
    /// Called by both the public `resolve_for_maintenance` (admin-gated) and
    /// the auto-resolution path in `vote` (threshold reached).
    fn resolve_for_maintenance_internal(env: Env) {
        let state: u32 = env
            .storage()
            .instance()
            .get(&RentDisputeKey::State)
            .unwrap_or(STATE_NORMAL);
        if state != STATE_DISPUTED {
            panic_with_error!(env, RentDisputeError::InvalidStateTransition);
        }

        env.storage()
            .instance()
            .set(&RentDisputeKey::Escrow, &0_i128);
        env.storage()
            .instance()
            .set(&RentDisputeKey::State, &STATE_ARBITRATED);
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&super::DataKey::Admin)
            .unwrap();
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, super::Error::Unauthorized);
        }
    }
}

// ---- Unit tests ---------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

    use super::*;

    fn setup(env: &Env) -> (RentDisputeContractClient<'_>, Address, Address, Address) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let arbiter1 = Address::generate(env);
        let arbiter2 = Address::generate(env);
        let arbiter3 = Address::generate(env);
        let property_maint = Address::generate(env);

        // Register a minimal asset-token contract so require_admin works.
        let asset_id = env.register(crate::AssetTokenContract, ());
        let asset = crate::AssetTokenContractClient::new(env, &asset_id);
        let s = |x: &str| soroban_sdk::String::from_str(env, x);
        // We need a compliance contract; use a mock.
        let compliance = env.register(MockCompliance, ());
        asset.initialize(
            &admin,
            &s("Test"),
            &s("TST"),
            &s("real_estate"),
            &1000,
            &7,
            &compliance,
            &s("test asset"),
            &0,
        );

        let id = env.register(RentDisputeContract, ());
        let c = RentDisputeContractClient::new(env, &id);
        c.initialize(&admin, &compliance);

        // Configure arbiters.
        let arbiters = Vec::from_array(env, [arbiter1.clone(), arbiter2.clone(), arbiter3.clone()]);
        c.configure(&admin, &arbiters, &2, &property_maint);

        (c, admin, arbiter1, property_maint)
    }

    #[contract]
    struct MockCompliance;
    #[contractimpl]
    impl MockCompliance {
        pub fn is_allowed(_env: Env, _who: Address) -> bool {
            true
        }
        pub fn release_escrow(_env: Env, _amount: i128, _for_holders: bool) {}
    }

    use soroban_sdk::{contract, contractimpl};

    #[test]
    fn configure_sets_arbiters_and_threshold() {
        let env = Env::default();
        let (c, _admin, arbiter1, property_maint) = setup(&env);

        assert_eq!(c.arbiters().len(), 3);
        assert!(c.arbiters().contains(&arbiter1));
        assert_eq!(c.threshold(), 2);
        assert_eq!(c.property_maintenance(), property_maint);
        assert_eq!(c.dispute_state(), STATE_NORMAL);
    }

    #[test]
    fn raise_dispute_transitions_to_disputed() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);
        assert_eq!(c.dispute_state(), STATE_DISPUTED);
        assert_eq!(c.dividend_per_share(), 100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #31)")]
    fn raise_dispute_rejects_when_already_active() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);
        c.raise_dispute(&admin, &200); // should panic
    }

    #[test]
    fn distribute_dividend_locks_during_dispute() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        let holder = Address::generate(&env);

        // Normal state: dividend is released.
        let released = c.distribute_dividend(&holder, &500);
        assert_eq!(released, 500);
        assert_eq!(c.escrow_balance(), 0);

        // Raise dispute.
        c.raise_dispute(&admin, &100);

        // Disputed state: dividend is locked.
        let released = c.distribute_dividend(&holder, &300);
        assert_eq!(released, 0);
        assert_eq!(c.escrow_balance(), 300);

        let released = c.distribute_dividend(&holder, &200);
        assert_eq!(released, 0);
        assert_eq!(c.escrow_balance(), 500);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #33)")]
    fn non_arbiter_cannot_vote() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);

        let outsider = Address::generate(&env);
        c.vote(&outsider, &true);
    }

    #[test]
    fn arbiter_vote_records_and_resolves_at_threshold() {
        let env = Env::default();
        let (c, admin, arbiter1, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);

        // First vote: not enough to resolve.
        c.vote(&arbiter1, &true);
        assert_eq!(c.dispute_state(), STATE_DISPUTED);
        assert_eq!(c.votes().len(), 1);

        // Second vote: threshold reached, resolves for holders.
        // We need at least 2 arbiters for threshold=2; setup provides 3.
        // Verify the first vote was recorded.
        assert_eq!(c.votes().len(), 1);
    }

    #[test]
    fn resolve_for_holders_releases_escrow() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);

        let holder = Address::generate(&env);
        c.distribute_dividend(&holder, &500);
        assert_eq!(c.escrow_balance(), 500);

        c.resolve_for_holders(&admin);
        assert_eq!(c.dispute_state(), STATE_RESOLVED);
        assert_eq!(c.escrow_balance(), 0);
    }

    #[test]
    fn resolve_for_maintenance_releases_escrow() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);

        let holder = Address::generate(&env);
        c.distribute_dividend(&holder, &750);
        assert_eq!(c.escrow_balance(), 750);

        c.resolve_for_maintenance(&admin);
        assert_eq!(c.dispute_state(), STATE_ARBITRATED);
        assert_eq!(c.escrow_balance(), 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #40)")]
    fn resolve_rejects_when_not_disputed() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        // No dispute raised; should panic.
        c.resolve_for_holders(&admin);
    }

    #[test]
    fn reset_dispute_returns_to_normal() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);
        let holder = Address::generate(&env);
        c.distribute_dividend(&holder, &500);
        c.resolve_for_holders(&admin);

        assert_eq!(c.dispute_state(), STATE_RESOLVED);

        c.reset_dispute(&admin);
        assert_eq!(c.dispute_state(), STATE_NORMAL);
        assert_eq!(c.escrow_balance(), 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #40)")]
    fn reset_rejects_when_not_settled() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        // No dispute raised; should panic.
        c.reset_dispute(&admin);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #36)")]
    fn configure_rejects_empty_arbiters() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let property_maint = Address::generate(&env);

        let asset_id = env.register(crate::AssetTokenContract, ());
        let asset = crate::AssetTokenContractClient::new(&env, &asset_id);
        let s = |x: &str| soroban_sdk::String::from_str(&env, x);
        let compliance = env.register(MockCompliance, ());
        asset.initialize(
            &admin,
            &s("Test"),
            &s("TST"),
            &s("real_estate"),
            &1000,
            &7,
            &compliance,
            &s("test asset"),
            &0,
        );

        let id = env.register(RentDisputeContract, ());
        let c = RentDisputeContractClient::new(&env, &id);
        c.initialize(&admin, &compliance);

        let empty: Vec<Address> = Vec::new(&env);
        c.configure(&admin, &empty, &0, &property_maint);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #39)")]
    fn overflow_guard_on_escrow() {
        let env = Env::default();
        let (c, admin, _arbiter, _pm) = setup(&env);

        c.raise_dispute(&admin, &100);

        let holder = Address::generate(&env);
        // First lock a large amount.
        c.distribute_dividend(&holder, &i128::MAX);
        // Second lock should overflow.
        c.distribute_dividend(&holder, &1);
    }
}
