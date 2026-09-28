//! Issue #147 — multi-token debt refinancing / bond conversion engine.
//!
//! An issuer registers each token series it wants to refinance as either
//! `Debt` or `Equity`, naming the compliance contract that gates transfers of
//! that series. It then publishes a [`RefinancingOffer`] that converts an
//! *old* (high-interest) series into a *new* (lower-interest debt or equity)
//! series at a fixed `conversion_ratio` until `expiration`.
//!
//! A holder executes the conversion in a single contract call: the old shares
//! are burned and the new shares minted together, so the swap is
//! all-or-nothing — if any step fails the host reverts the whole call, both
//! ledger writes and the event included. Before either balance is touched the
//! engine verifies that (a) the offer is live, (b) the holder is allowlisted
//! for **both** series' compliance contracts, and (c) the holder actually owns
//! the old shares. Because the converted shares are burned, the same shares can
//! never be converted twice.
//!
//! `conversion_ratio` is WAD-scaled (`1e18` == 1 new share per old share) and
//! the minted amount is `floor(old_shares * ratio / 1e18)`, reusing the
//! 256-bit `mul_div` the interest-rate model already uses. A conversion that
//! would mint zero shares is rejected rather than silently destroying the
//! holder's balance to rounding.
use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, Symbol, Val, Vec,
};

use crate::interest_rate::{mul_div, WAD};
use crate::{DataKey, Error};

/// Denominator for `conversion_ratio`: WAD, the same 18-decimal fixed point
/// the interest-rate model uses.
pub const RATIO_SCALE: i128 = WAD;

/// Whether a refinancing target is a (lower-interest) debt series or an equity
/// series. Both are legal conversion targets; the class is informational to
/// the contract, which only tracks shares.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenClass {
    Debt,
    Equity,
}

/// A token series that can take part in refinancing.
#[contracttype]
#[derive(Clone)]
pub struct TokenSeries {
    /// The only party allowed to publish refinancing offers out of this series.
    pub issuer: Address,
    pub class: TokenClass,
    /// Compliance contract consulted via `is_allowed(holder)` before any share
    /// of this series is minted or converted.
    pub compliance: Address,
    /// Inactive series can neither be the source nor the target of a
    /// conversion (and cannot be minted).
    pub active: bool,
}

/// A published refinancing: convert `old_token_id` into `new_token_id`.
#[contracttype]
#[derive(Clone)]
pub struct RefinancingOffer {
    pub old_token_id: u64,
    pub new_token_id: u64,
    /// WAD-scaled new shares minted per old share burned (`1e18` == 1:1).
    pub conversion_ratio: i128,
    /// Ledger timestamp at/after which the offer can no longer be executed.
    pub expiration: u64,
    pub active: bool,
}

fn require_admin(env: &Env, admin: &Address) {
    let stored: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
    admin.require_auth();
    if admin != &stored {
        panic_with_error!(env, Error::Unauthorized);
    }
}

fn series_or_panic(env: &Env, token_id: u64) -> TokenSeries {
    env.storage()
        .persistent()
        .get(&DataKey::TokenSeries(token_id))
        .unwrap_or_else(|| panic_with_error!(env, Error::TokenSeriesNotRegistered))
}

fn live_series(env: &Env, token_id: u64) -> TokenSeries {
    let series = series_or_panic(env, token_id);
    if !series.active {
        panic_with_error!(env, Error::TokenSeriesNotRegistered);
    }
    series
}

fn balance(env: &Env, token_id: u64, holder: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::SeriesShares(token_id, holder.clone()))
        .unwrap_or(0)
}

fn set_balance(env: &Env, token_id: u64, holder: &Address, value: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::SeriesShares(token_id, holder.clone()), &value);
}

/// `compliance.is_allowed(holder)` via a cross-contract call, the same gate
/// `asset-token` uses for its transfers.
fn is_allowed(env: &Env, compliance: &Address, holder: &Address) -> bool {
    let args: Vec<Val> = (holder.clone(),).into_val(env);
    env.invoke_contract(compliance, &Symbol::new(env, "is_allowed"), args)
}

/// Register a `Debt`/`Equity` series alongside the compliance contract that
/// gates it. Admin-authenticated; a series id can only be registered once.
pub fn register_series(
    env: &Env,
    admin: Address,
    token_id: u64,
    issuer: Address,
    class: TokenClass,
    compliance: Address,
) {
    require_admin(env, &admin);
    if env
        .storage()
        .persistent()
        .has(&DataKey::TokenSeries(token_id))
    {
        panic_with_error!(env, Error::TokenSeriesAlreadyRegistered);
    }
    env.storage().persistent().set(
        &DataKey::TokenSeries(token_id),
        &TokenSeries {
            issuer: issuer.clone(),
            class,
            compliance: compliance.clone(),
            active: true,
        },
    );
    env.events().publish(
        (symbol_short!("refinance"), symbol_short!("series")),
        (token_id, class, compliance),
    );
}

/// Issue `amount` shares of a registered series to `holder`, subject to that
/// series' compliance allowlist. Admin-authenticated.
pub fn mint_shares(env: &Env, admin: Address, token_id: u64, holder: Address, amount: i128) {
    require_admin(env, &admin);
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let series = live_series(env, token_id);
    if !is_allowed(env, &series.compliance, &holder) {
        panic_with_error!(env, Error::NotAllowlisted);
    }
    let new_balance = balance(env, token_id, &holder)
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
    set_balance(env, token_id, &holder, new_balance);
    env.events().publish(
        (symbol_short!("refinance"), symbol_short!("issue")),
        (token_id, holder, amount),
    );
}

/// Publish a refinancing offer. Authorized by the *old* series' issuer, so the
/// entry point needs no separate caller argument: the host checks the issuer's
/// signature via `require_auth`.
pub fn initiate(
    env: &Env,
    old_token_id: u64,
    new_token_id: u64,
    conversion_ratio: i128,
    expiration: u64,
) -> RefinancingOffer {
    let old = live_series(env, old_token_id);
    old.issuer.require_auth();
    // Ensure the target is a registered, active series as well.
    live_series(env, new_token_id);

    if conversion_ratio <= 0 {
        panic_with_error!(env, Error::InvalidConversionRatio);
    }
    if expiration <= env.ledger().timestamp() {
        panic_with_error!(env, Error::InvalidExpiration);
    }

    let offer = RefinancingOffer {
        old_token_id,
        new_token_id,
        conversion_ratio,
        expiration,
        active: true,
    };
    env.storage()
        .persistent()
        .set(&DataKey::Refinancing(old_token_id, new_token_id), &offer);
    env.events().publish(
        (symbol_short!("refinance"), symbol_short!("offer")),
        (old_token_id, new_token_id, conversion_ratio, expiration),
    );
    offer
}

/// Burn `amount` old shares held by `holder` and mint the equivalent new
/// shares in the same call. Returns the number of new shares minted.
///
/// Reverts (rolling back every write in this call) if the offer does not
/// exist, has expired, either series is inactive, the holder is not
/// allowlisted on **both** series, the holder does not own `amount` old
/// shares, or the ratio would round the minted amount down to zero.
pub fn convert(
    env: &Env,
    holder: Address,
    old_token_id: u64,
    new_token_id: u64,
    amount: i128,
) -> i128 {
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    holder.require_auth();

    let offer: RefinancingOffer = env
        .storage()
        .persistent()
        .get(&DataKey::Refinancing(old_token_id, new_token_id))
        .unwrap_or_else(|| panic_with_error!(env, Error::RefinancingNotFound));
    if !offer.active {
        panic_with_error!(env, Error::RefinancingNotFound);
    }
    if env.ledger().timestamp() >= offer.expiration {
        panic_with_error!(env, Error::RefinancingExpired);
    }

    let old = live_series(env, old_token_id);
    let new = live_series(env, new_token_id);

    // Dual gate: a party not allowlisted for *either* token must not convert.
    if !is_allowed(env, &old.compliance, &holder) || !is_allowed(env, &new.compliance, &holder) {
        panic_with_error!(env, Error::NotAllowlisted);
    }

    let old_balance = balance(env, old_token_id, &holder);
    if old_balance < amount {
        panic_with_error!(env, Error::InsufficientShares);
    }

    let new_shares = mul_div(env, amount, offer.conversion_ratio, RATIO_SCALE);
    if new_shares <= 0 {
        panic_with_error!(env, Error::InvalidConversionRatio);
    }

    // All validation passed: burn and mint together. Any later failure in this
    // same call reverts both writes.
    set_balance(env, old_token_id, &holder, old_balance - amount);
    let new_balance = balance(env, new_token_id, &holder)
        .checked_add(new_shares)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
    set_balance(env, new_token_id, &holder, new_balance);

    let converted: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::Converted(
            old_token_id,
            new_token_id,
            holder.clone(),
        ))
        .unwrap_or(0);
    let converted = converted
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
    env.storage().persistent().set(
        &DataKey::Converted(old_token_id, new_token_id, holder.clone()),
        &converted,
    );

    env.events().publish(
        (symbol_short!("refinance"), symbol_short!("convert")),
        (old_token_id, new_token_id, holder, amount, new_shares),
    );
    new_shares
}

pub fn series(env: &Env, token_id: u64) -> TokenSeries {
    series_or_panic(env, token_id)
}

pub fn shares(env: &Env, token_id: u64, holder: Address) -> i128 {
    balance(env, token_id, &holder)
}

pub fn offer(env: &Env, old_token_id: u64, new_token_id: u64) -> RefinancingOffer {
    env.storage()
        .persistent()
        .get(&DataKey::Refinancing(old_token_id, new_token_id))
        .unwrap_or_else(|| panic_with_error!(env, Error::RefinancingNotFound))
}

/// Cumulative old shares `holder` has converted under this offer. Purely an
/// audit trail: double-conversion is prevented by the burn, not this counter.
pub fn converted(env: &Env, old_token_id: u64, new_token_id: u64, holder: Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Converted(old_token_id, new_token_id, holder))
        .unwrap_or(0)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{DebtTokenAmortization, DebtTokenAmortizationClient};
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Ledger},
        Address, Env,
    };

    /// Minimal compliance contract: `is_allowed` is a toggle per address.
    #[contract]
    struct MockCompliance;

    #[contractimpl]
    impl MockCompliance {
        pub fn set_allowed(env: Env, who: Address, ok: bool) {
            env.storage().persistent().set(&who, &ok);
        }

        pub fn is_allowed(env: Env, who: Address) -> bool {
            env.storage().persistent().get(&who).unwrap_or(false)
        }
    }

    const ONE_POINT_TWO_FIVE: i128 = 1_250_000_000_000_000_000; // 1.25 WAD

    struct Fixture<'a> {
        c: DebtTokenAmortizationClient<'a>,
        admin: Address,
        holder: Address,
        comp_old: Address,
        comp_new: Address,
    }

    /// Old (Debt, id 1) and new (Equity, id 2) series, each gated by its own
    /// compliance contract, with 1_000 old shares minted to an allowlisted
    /// holder. Ledger time starts at 1_000.
    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        env.ledger().set_timestamp(1_000);

        let id = env.register(DebtTokenAmortization, ());
        let c = DebtTokenAmortizationClient::new(env, &id);
        let admin = Address::generate(env);
        let issuer = Address::generate(env);
        let holder = Address::generate(env);
        let comp_old = env.register(MockCompliance, ());
        let comp_new = env.register(MockCompliance, ());

        c.initialize(&admin);
        c.register_refinancing_series(&admin, &1u64, &issuer, &TokenClass::Debt, &comp_old);
        c.register_refinancing_series(&admin, &2u64, &issuer, &TokenClass::Equity, &comp_new);

        let allow = |compliance: &Address| {
            let client = MockComplianceClient::new(env, compliance);
            client.set_allowed(&issuer, &true);
            client.set_allowed(&holder, &true);
        };
        allow(&comp_old);
        allow(&comp_new);

        c.mint_refinancing_shares(&admin, &1u64, &holder, &1_000i128);

        Fixture {
            c,
            admin,
            holder,
            comp_old,
            comp_new,
        }
    }

    #[test]
    fn converts_old_debt_into_new_equity_at_ratio() {
        let env = Env::default();
        let f = setup(&env);
        let offer =
            f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        assert_eq!(offer.conversion_ratio, ONE_POINT_TWO_FIVE);

        let minted = f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);

        assert_eq!(minted, 1_250); // 1_000 * 1.25
        assert_eq!(f.c.series_shares(&1u64, &f.holder), 0); // burned
        assert_eq!(f.c.series_shares(&2u64, &f.holder), 1_250); // minted
        assert_eq!(f.c.converted_shares(&1u64, &2u64, &f.holder), 1_000);
        assert_eq!(f.c.token_series(&2u64).class, TokenClass::Equity);
    }

    #[test]
    fn partial_conversion_leaves_remainder_intact() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        assert_eq!(
            f.c.execute_refinancing(&f.holder, &1u64, &2u64, &400i128),
            500
        );
        assert_eq!(f.c.series_shares(&1u64, &f.holder), 600);
        assert_eq!(f.c.series_shares(&2u64, &f.holder), 500);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #13)")]
    fn holder_not_allowlisted_on_new_token_cannot_convert() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        MockComplianceClient::new(&env, &f.comp_new).set_allowed(&f.holder, &false);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #13)")]
    fn holder_not_allowlisted_on_old_token_cannot_convert() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        MockComplianceClient::new(&env, &f.comp_old).set_allowed(&f.holder, &false);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
    }

    #[test]
    fn conversion_succeeds_before_expiration() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &2_000u64);
        env.ledger().set_timestamp(1_999);
        assert_eq!(
            f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128),
            1_250
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #11)")]
    fn conversion_at_expiration_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &2_000u64);
        env.ledger().set_timestamp(2_000);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn initiating_with_expiration_in_the_past_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &1_000u64);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn non_positive_ratio_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &0i128, &5_000u64);
    }

    #[test]
    fn ratio_rounds_down() {
        let env = Env::default();
        let f = setup(&env);
        // 0.666... new per old: 3 old shares -> floor(1.999...) = 1 new share.
        let two_thirds: i128 = 666_666_666_666_666_666;
        f.c.initiate_refinancing(&1u64, &2u64, &two_thirds, &5_000u64);
        assert_eq!(f.c.execute_refinancing(&f.holder, &1u64, &2u64, &3i128), 1);
        assert_eq!(f.c.series_shares(&1u64, &f.holder), 997);
        assert_eq!(f.c.series_shares(&2u64, &f.holder), 1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn conversion_rounding_to_zero_rejected() {
        let env = Env::default();
        let f = setup(&env);
        let two_thirds: i128 = 666_666_666_666_666_666;
        f.c.initiate_refinancing(&1u64, &2u64, &two_thirds, &5_000u64);
        // floor(1 * 0.666...) == 0: the mint must not silently round to nothing.
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #12)")]
    fn already_converted_shares_cannot_be_converted_again() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
        // The 1_000 old shares were burned, so they cannot be converted twice.
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #12)")]
    fn converting_more_than_balance_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.c.initiate_refinancing(&1u64, &2u64, &ONE_POINT_TWO_FIVE, &5_000u64);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_001i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn conversion_without_an_offer_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.c.execute_refinancing(&f.holder, &1u64, &2u64, &1_000i128);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn duplicate_series_registration_rejected() {
        let env = Env::default();
        let f = setup(&env);
        let issuer = Address::generate(&env);
        let comp = Address::generate(&env);
        f.c.register_refinancing_series(&f.admin, &1u64, &issuer, &TokenClass::Debt, &comp);
    }
}
