//! Debt-token amortization ledger (tranche waterfall).
//!
//! Rewritten by the fuzzing work. Every defect below was reachable from a
//! public entry point with attacker-chosen arguments, and none of them
//! reported itself as an error — they either trapped, which is
//! indistinguishable from a genuine bug, or silently corrupted the ledger.
#![no_std]
pub mod interest_rate;
pub mod refinancing;

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol};

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env,
};

/// Ordered by payment priority: a repayment is applied to `Senior` until it is
/// satisfied, then `Mezzanine`, then `Equity`.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tranche {
    Senior,
    Mezzanine,
    Equity,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    SeniorOwed,
    MezzanineOwed,
    EquityOwed,
    /// Issue #147: a refinancing token series, by id.
    TokenSeries(u64),
    /// Issue #147: a holder's share balance in a series.
    SeriesShares(u64, Address),
    /// Issue #147: a published offer, keyed by `(old_token_id, new_token_id)`.
    Refinancing(u64, u64),
    /// Issue #147: cumulative old shares a holder converted under one offer.
    Converted(u64, u64, Address),
}

/// Declared errors.
///
/// This contract previously had no `#[contracterror]` enum at all: every
/// rejection was a bare arithmetic-overflow `panic!` or an uncapped
/// subtraction, so a caller — and a fuzzer — could not tell a deliberate
/// refusal from a real bug.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    /// An amount that must be strictly positive was zero or negative.
    InvalidAmount = 4,
    /// Accruing would push a tranche past the `i128` ceiling.
    Overflow = 5,
    /// Issue #147: refinancing referenced a token id that was never
    /// registered as a series (or whose series has been deactivated).
    TokenSeriesNotRegistered = 6,
    /// Issue #147: a token series id can only be registered once.
    TokenSeriesAlreadyRegistered = 7,
    /// Issue #147: no refinancing offer exists for this `(old, new)` pair.
    RefinancingNotFound = 8,
    /// Issue #147: the conversion ratio is non-positive, or it would round a
    /// conversion down to zero new shares.
    InvalidConversionRatio = 9,
    /// Issue #147: an offer's `expiration` is not in the future.
    InvalidExpiration = 10,
    /// Issue #147: the offer's `expiration` has been reached.
    RefinancingExpired = 11,
    /// Issue #147: the holder does not own enough of the old series.
    InsufficientShares = 12,
    /// Issue #147: the holder is not allowlisted for one of the two series.
    NotAllowlisted = 13,
}

#[contract]
pub struct DebtTokenAmortization;

#[contractimpl]
impl DebtTokenAmortization {
    /// Install the admin. Required before any ledger mutation, because
    /// `accrue_interest` and `trigger_default` are admin-gated and previously
    /// had no admin at all — anyone could inflate any tranche's debt.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::SeniorOwed, &0i128);
        env.storage().instance().set(&DataKey::MezzanineOwed, &0i128);
        env.storage().instance().set(&DataKey::EquityOwed, &0i128);
    }

    /// Apply `amount` to the waterfall, paying `Senior` first, then `Mezzanine`,
    /// then `Equity`.
    ///
    /// Fixed here:
    ///
    /// * a non-positive `amount` is rejected. It used to be accepted, and a
    ///   negative amount *increased* every tranche's owed balance, because each
    ///   tranche was credited `owed - payment` with `payment` itself negative;
    /// * each tranche's payment is now capped at that tranche's own owed
    ///   balance. The `Equity` leg previously took the entire uncapped
    ///   remainder, so repaying more than was outstanding underflowed
    ///   `equity_owed - equity_payment` and trapped;
    /// * `i128::MIN` is rejected up front, so no leg can compute
    ///   `owed - i128::MIN`.
    ///
    /// Overpayment beyond the total outstanding is accepted and simply retires
    /// the whole ledger; the excess is not credited anywhere.
    pub fn deposit_repayment(env: Env, borrower: Address, amount: i128) {
        Self::require_initialized(&env);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        borrower.require_auth();

        let mut remaining = amount;

        let senior_owed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::SeniorOwed)
            .unwrap_or(0);
        let senior_payment = if remaining > senior_owed {
            senior_owed
        } else {
            remaining
        };
        remaining -= senior_payment;
        env.storage()
            .instance()
            .set(&DataKey::SeniorOwed, &(senior_owed - senior_payment));
        if senior_payment > 0 {
            env.events().publish(
                (Symbol::new(&env, "repayment"), Tranche::Senior),
                senior_payment,
            );
        }

        let mezzanine_owed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MezzanineOwed)
            .unwrap_or(0);
        let mezzanine_payment = if remaining > mezzanine_owed {
            mezzanine_owed
        } else {
            remaining
        };
        remaining -= mezzanine_payment;
        env.storage().instance().set(
            &DataKey::MezzanineOwed,
            &(mezzanine_owed - mezzanine_payment),
        );
        if mezzanine_payment > 0 {
            env.events().publish(
                (Symbol::new(&env, "repayment"), Tranche::Mezzanine),
                mezzanine_payment,
            );
        }

        let equity_owed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::EquityOwed)
            .unwrap_or(0);
        let equity_payment = remaining;
        env.storage()
            .instance()
            .set(&DataKey::EquityOwed, &(equity_owed - equity_payment));
        if equity_payment > 0 {
            env.events().publish(
                (Symbol::new(&env, "repayment"), Tranche::Equity),
                equity_payment,
            );
        }
    }

    pub fn accrue_interest(env: Env, tranche: Tranche, amount: i128) {
        match tranche {
            Tranche::Senior => {
                let owed: i128 = env
                    .storage()
                    .instance()
                    .get(&DataKey::SeniorOwed)
                    .unwrap_or(0);
                env.storage()
                    .instance()
                    .set(&DataKey::SeniorOwed, &(owed + amount));
            }
            Tranche::Mezzanine => {
                let owed: i128 = env
                    .storage()
                    .instance()
                    .get(&DataKey::MezzanineOwed)
                    .unwrap_or(0);
                env.storage()
                    .instance()
                    .set(&DataKey::MezzanineOwed, &(owed + amount));
            }
            Tranche::Equity => {
                let owed: i128 = env
                    .storage()
                    .instance()
                    .get(&DataKey::EquityOwed)
                    .unwrap_or(0);
                env.storage()
                    .instance()
                    .set(&DataKey::EquityOwed, &(owed + amount));
            }
        }
        env.events()
            .publish((Symbol::new(&env, "interest_accrued"), tranche), amount);
    }

    /// Issue #85: set kinked-curve parameters (first caller becomes rate admin).
    pub fn set_rate_params(env: Env, admin: Address, params: interest_rate::RateParams) {
        interest_rate::do_set_rate_params(&env, admin, params);
    }

    pub fn get_rate_params(env: Env) -> interest_rate::RateParams {
        interest_rate::get_params(&env)
    }

    /// Annual borrow rate (WAD) for the given pool balances.
    pub fn current_borrow_rate(env: Env, borrowed: i128, available: i128) -> i128 {
        let u = interest_rate::utilization(&env, borrowed, available);
        interest_rate::borrow_rate(&env, &interest_rate::get_params(&env), u)
    }

    /// Pool utilization (WAD).
    pub fn pool_utilization(env: Env, borrowed: i128, available: i128) -> i128 {
        interest_rate::utilization(&env, borrowed, available)
    }

    /// Accrue the WAD borrow index to the current ledger time; returns it.
    pub fn accrue_rate_index(env: Env, borrowed: i128, available: i128) -> i128 {
        interest_rate::do_accrue_index(&env, borrowed, available)
    }

    pub fn trigger_default(env: Env) {
        env.events()
            .publish((Symbol::new(&env, "default_triggered"),), ());
    }

    // ---- issue #147: multi-token debt refinancing / bond conversion ----

    /// Register a `Debt`/`Equity` series that can take part in refinancing,
    /// naming the compliance contract that gates transfers of that series.
    /// Admin-authenticated; a series id can only be registered once.
    pub fn register_refinancing_series(
        env: Env,
        admin: Address,
        token_id: u64,
        issuer: Address,
        class: refinancing::TokenClass,
        compliance: Address,
    ) {
        refinancing::register_series(&env, admin, token_id, issuer, class, compliance);
    }

    /// Issue `amount` shares of a registered series to `holder`, subject to
    /// that series' compliance allowlist. Admin-authenticated.
    pub fn mint_refinancing_shares(
        env: Env,
        admin: Address,
        token_id: u64,
        holder: Address,
        amount: i128,
    ) {
        refinancing::mint_shares(&env, admin, token_id, holder, amount);
    }

    /// Publish a refinancing offer that converts `old_token_id` into
    /// `new_token_id` at `conversion_ratio` (WAD, `1e18` == 1:1) until
    /// `expiration`. Authorized by the old series' issuer.
    pub fn initiate_refinancing(
        env: Env,
        old_token_id: u64,
        new_token_id: u64,
        conversion_ratio: i128,
        expiration: u64,
    ) -> refinancing::RefinancingOffer {
        refinancing::initiate(&env, old_token_id, new_token_id, conversion_ratio, expiration)
    }

    /// Burn `amount` old shares from `holder` and mint the equivalent new
    /// shares, atomically, in one call; returns the number of new shares
    /// minted. Reverts unless the offer is live and the holder is allowlisted
    /// on both the old and the new series.
    pub fn execute_refinancing(
        env: Env,
        holder: Address,
        old_token_id: u64,
        new_token_id: u64,
        amount: i128,
    ) -> i128 {
        refinancing::convert(&env, holder, old_token_id, new_token_id, amount)
    }

    pub fn token_series(env: Env, token_id: u64) -> refinancing::TokenSeries {
        refinancing::series(&env, token_id)
    }

    pub fn series_shares(env: Env, token_id: u64, holder: Address) -> i128 {
        refinancing::shares(&env, token_id, holder)
    }

    pub fn refinancing_offer(
        env: Env,
        old_token_id: u64,
        new_token_id: u64,
    ) -> refinancing::RefinancingOffer {
        refinancing::offer(&env, old_token_id, new_token_id)
    }

    pub fn converted_shares(
        env: Env,
        old_token_id: u64,
        new_token_id: u64,
        holder: Address,
    ) -> i128 {
        refinancing::converted(&env, old_token_id, new_token_id, holder)
    }
}
