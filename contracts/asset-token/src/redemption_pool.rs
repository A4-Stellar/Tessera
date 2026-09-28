//! Automated Asset Token Redemption and Liquidation Pool (issue #153).
//!
//! When a tokenized real-estate asset reaches maturity and the underlying
//! property is sold, the issuer must be able to wind the asset down: the sale
//! proceeds (a SEP-0041 stablecoin) are deposited into the token contract and
//! each token holder burns their tokens in exchange for their pro-rata share
//! of those proceeds. From the moment the asset is marked `Liquidated` the
//! secondary market is permanently closed — [`ensure_transferable`] makes
//! every future `transfer` fail — so holder balances are frozen at the
//! liquidation snapshot and the pro-rata maths cannot be gamed by moving
//! tokens between accounts after the sale price is known.
//!
//! # Lifecycle
//!
//! 1. `start_liquidation` (admin): capture the total supply as the redemption
//!    snapshot, record the stablecoin contract that will pay out, flip the
//!    asset status to [`AssetStatus::Liquidated`] and freeze transfers.
//! 2. `deposit_liquidation_proceeds` (admin, repeatable): transfer sale
//!    proceeds from the issuer into this contract and add them to the pool.
//! 3. `redeem_liquidation_proceeds` (holder): burn `amount` tokens, reduce
//!    total supply and transfer `amount * deposited / snapshot_supply` of the
//!    stablecoin to the holder.
//!
//! A holder may redeem repeatedly while they still hold tokens; each call is
//! priced against the *current* pool balance and the fixed snapshot supply.
//! Rounding is always in favour of the pool (floor division), so the contract
//! can never pay out more than it has taken in.
//!
//! # Storage layout
//!
//! Module-local keys (the asset-token's own `DataKey` is used only for the
//! shared `TotalSupply`) so nothing in the existing layout is renumbered.
//!
//! | Key                          | Storage    | Type        | Description                          |
//! |------------------------------|------------|-------------|--------------------------------------|
//! | `RedemptionKey::Status`      | instance   | `AssetStatus` | `Active` (default) or `Liquidated` |
//! | `RedemptionKey::Stablecoin`  | instance   | `Address`   | SEP-0041 token used to disburse      |
//! | `RedemptionKey::SnapshotSupply` | instance | `i128`     | Total supply captured at liquidation |
//! | `RedemptionKey::TotalProceeds`  | instance | `i128`     | Stablecoin proceeds deposited        |
//! | `RedemptionKey::TotalClaimed`   | instance | `i128`     | Stablecoin proceeds paid out         |
//! | `RedemptionKey::Redeemed(holder)` | persistent | `i128`  | Tokens a holder has burned           |

use soroban_sdk::{
    contracterror, contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, Symbol,
    Val, Vec,
};

use crate::stock_split::{effective_balance, set_balance};
use crate::{DataKey, Error};

// ---- Errors -------------------------------------------------------------- //
// Numbered from 30 so they stay clear of the asset-token `Error` codes
// (1..=12) and the buyback module's `BuybackError` codes (20..=29).

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RedemptionError {
    /// The asset has not been marked `Liquidated` yet.
    NotLiquidated = 30,
    /// The asset is already liquidated; the status cannot be re-entered.
    AlreadyLiquidated = 31,
    /// `stablecoin_amount` (or a redemption amount) must be strictly positive.
    InvalidAmount = 32,
    /// Arithmetic overflow while accumulating proceeds or payouts.
    Overflow = 33,
}

// ---- Status -------------------------------------------------------------- //

/// Lifecycle status of a tokenized asset (issue #153).
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetStatus {
    /// Normal operation: transfers, mint, burn and splits behave as before.
    Active = 0,
    /// The underlying asset has been sold. Secondary-market transfers are
    /// permanently disabled; the remaining holder action is redemption of
    /// tokens for a pro-rata share of the deposited cash proceeds.
    Liquidated = 1,
}

// ---- Storage keys -------------------------------------------------------- //

#[contracttype]
#[derive(Clone)]
enum RedemptionKey {
    /// `AssetStatus`. Absent means [`AssetStatus::Active`].
    Status,
    /// SEP-0041 stablecoin contract that funds the redemption pool.
    Stablecoin,
    /// Total supply captured the moment liquidation started. Fixed forever:
    /// it is the denominator of every pro-rata payout.
    SnapshotSupply,
    /// Stablecoin proceeds deposited into the pool so far.
    TotalProceeds,
    /// Stablecoin proceeds already paid out to redeeming holders.
    TotalClaimed,
    /// Tokens `holder` has already burned through redemption.
    Redeemed(Address),
}

// ---- Views --------------------------------------------------------------- //

/// Current asset status. Defaults to [`AssetStatus::Active`].
pub fn status(env: &Env) -> AssetStatus {
    env.storage()
        .instance()
        .get(&RedemptionKey::Status)
        .unwrap_or(AssetStatus::Active)
}

/// Whether the asset has been marked `Liquidated`.
pub fn is_liquidated(env: &Env) -> bool {
    status(env) == AssetStatus::Liquidated
}

/// Transfer gate checked by `AssetTokenContract::transfer`. Once the asset is
/// liquidated this panics with [`Error::AssetLiquidated`], permanently closing
/// the secondary market.
pub fn ensure_transferable(env: &Env) {
    if is_liquidated(env) {
        panic_with_error!(env, Error::AssetLiquidated);
    }
}

/// Total supply snapshotted at liquidation (`0` before liquidation).
pub fn snapshot_supply(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&RedemptionKey::SnapshotSupply)
        .unwrap_or(0)
}

/// `(total proceeds deposited, total proceeds claimed)`.
pub fn proceeds(env: &Env) -> (i128, i128) {
    let deposited = env
        .storage()
        .instance()
        .get(&RedemptionKey::TotalProceeds)
        .unwrap_or(0);
    let claimed = env
        .storage()
        .instance()
        .get(&RedemptionKey::TotalClaimed)
        .unwrap_or(0);
    (deposited, claimed)
}

/// Stablecoin proceeds `holder` would receive by redeeming their whole
/// remaining balance right now. `0` before liquidation.
pub fn claimable(env: &Env, holder: &Address) -> i128 {
    if !is_liquidated(env) {
        return 0;
    }
    let supply = snapshot_supply(env);
    if supply <= 0 {
        return 0;
    }
    let (deposited, _) = proceeds(env);
    effective_balance(env, holder)
        .checked_mul(deposited)
        .map(|scaled| scaled / supply)
        .unwrap_or(0)
}

// ---- Lifecycle ----------------------------------------------------------- //

/// Mark the asset `Liquidated` and freeze transfers forever.
///
/// The caller (`AssetTokenContract::start_liquidation`) is responsible for the
/// admin check; this only enforces the state transition.
pub fn start_liquidation(env: &Env, stablecoin_contract: Address) {
    if is_liquidated(env) {
        panic_with_error!(env, RedemptionError::AlreadyLiquidated);
    }

    let supply: i128 = env
        .storage()
        .instance()
        .get(&DataKey::TotalSupply)
        .unwrap_or(0);

    env.storage()
        .instance()
        .set(&RedemptionKey::Status, &AssetStatus::Liquidated);
    env.storage()
        .instance()
        .set(&RedemptionKey::Stablecoin, &stablecoin_contract);
    env.storage()
        .instance()
        .set(&RedemptionKey::SnapshotSupply, &supply);
    env.storage()
        .instance()
        .set(&RedemptionKey::TotalProceeds, &0_i128);
    env.storage()
        .instance()
        .set(&RedemptionKey::TotalClaimed, &0_i128);

    env.events().publish(
        (Symbol::new(env, "Liquidated"),),
        (supply, stablecoin_contract),
    );
}

/// Deposit `amount` stablecoins of sale proceeds from `from` into the pool.
/// Repeatable: an issuer may fund the pool in several tranches. Returns the
/// new total deposited.
pub fn deposit_proceeds(env: &Env, from: &Address, amount: i128) -> i128 {
    if !is_liquidated(env) {
        panic_with_error!(env, RedemptionError::NotLiquidated);
    }
    if amount <= 0 {
        panic_with_error!(env, RedemptionError::InvalidAmount);
    }

    let token: Address = env
        .storage()
        .instance()
        .get(&RedemptionKey::Stablecoin)
        .unwrap();
    let pool = env.current_contract_address();
    transfer_stablecoin(env, &token, from, &pool, amount);

    let deposited: i128 = env
        .storage()
        .instance()
        .get(&RedemptionKey::TotalProceeds)
        .unwrap_or(0);
    let new_total = deposited
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, RedemptionError::Overflow));
    env.storage()
        .instance()
        .set(&RedemptionKey::TotalProceeds, &new_total);

    env.events()
        .publish((symbol_short!("liqdep"), from.clone()), amount);

    new_total
}

/// Burn `amount` of `holder`'s tokens and pay out their pro-rata share of the
/// pool. Returns the stablecoin payout.
///
/// The payout is `amount * total_deposited / snapshot_supply` (floor), so a
/// holder who redeems in several calls receives the same total as one who
/// redeems everything at once *given the same pool balance*; proceeds
/// deposited later simply raise the price of the tokens still outstanding.
pub fn redeem(env: &Env, holder: &Address, amount: i128) -> i128 {
    holder.require_auth();

    if !is_liquidated(env) {
        panic_with_error!(env, RedemptionError::NotLiquidated);
    }
    if amount <= 0 {
        panic_with_error!(env, RedemptionError::InvalidAmount);
    }

    let balance = effective_balance(env, holder);
    if balance < amount {
        panic_with_error!(env, Error::InsufficientBalance);
    }

    let supply = snapshot_supply(env);
    let (deposited, claimed) = proceeds(env);
    let payout = if supply <= 0 {
        0
    } else {
        amount
            .checked_mul(deposited)
            .unwrap_or_else(|| panic_with_error!(env, RedemptionError::Overflow))
            / supply
    };

    // Burn: settle any pending splits, debit the holder and the total supply.
    set_balance(env, holder, balance - amount);
    let total: i128 = env
        .storage()
        .instance()
        .get(&DataKey::TotalSupply)
        .unwrap_or(0);
    let new_total = total
        .checked_sub(amount)
        .unwrap_or_else(|| panic_with_error!(env, RedemptionError::Overflow));
    env.storage()
        .instance()
        .set(&DataKey::TotalSupply, &new_total);

    let mut redeemed: i128 = env
        .storage()
        .persistent()
        .get(&RedemptionKey::Redeemed(holder.clone()))
        .unwrap_or(0);
    redeemed = redeemed
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, RedemptionError::Overflow));
    env.storage()
        .persistent()
        .set(&RedemptionKey::Redeemed(holder.clone()), &redeemed);

    if payout > 0 {
        let new_claimed = claimed
            .checked_add(payout)
            .unwrap_or_else(|| panic_with_error!(env, RedemptionError::Overflow));
        // Defensive: the pool can never pay out more than it holds.
        if new_claimed > deposited {
            panic_with_error!(env, RedemptionError::Overflow);
        }
        env.storage()
            .instance()
            .set(&RedemptionKey::TotalClaimed, &new_claimed);

        let token: Address = env
            .storage()
            .instance()
            .get(&RedemptionKey::Stablecoin)
            .unwrap();
        let pool = env.current_contract_address();
        transfer_stablecoin(env, &token, &pool, holder, payout);
    }

    env.events()
        .publish((symbol_short!("liqrdm"), holder.clone()), (amount, payout));

    payout
}

/// Tokens `holder` has burned through redemption so far.
pub fn redeemed_tokens(env: &Env, holder: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&RedemptionKey::Redeemed(holder.clone()))
        .unwrap_or(0)
}

// ---- Internal ------------------------------------------------------------ //

/// Call `transfer(from, to, amount)` on the SEP-0041 stablecoin contract.
fn transfer_stablecoin(env: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
    let args: Vec<Val> = (from.clone(), to.clone(), amount).into_val(env);
    env.invoke_contract::<()>(token, &Symbol::new(env, "transfer"), args);
}

// ---- Unit tests ---------------------------------------------------------- //

#[cfg(test)]
mod test {
    use super::*;
    use crate::{AssetTokenContract, AssetTokenContractClient};
    use soroban_sdk::{contract, contractimpl, testutils::Address as _, String};

    #[contract]
    struct AllowAll;
    #[contractimpl]
    impl AllowAll {
        pub fn is_allowed(_env: Env, _who: Address) -> bool {
            true
        }
    }

    /// Minimal SEP-0041-style stablecoin: the pool only ever calls `transfer`.
    #[contract]
    struct MockStable;
    #[contractimpl]
    impl MockStable {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let key = StableKey::Balance(to);
            let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
            env.storage().persistent().set(&key, &(balance + amount));
        }

        pub fn balance(env: Env, of: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&StableKey::Balance(of))
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            from.require_auth();
            let from_key = StableKey::Balance(from);
            let to_key = StableKey::Balance(to);
            let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
            if from_balance < amount {
                panic!("insufficient stablecoin balance");
            }
            let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
            env.storage()
                .persistent()
                .set(&from_key, &(from_balance - amount));
            env.storage()
                .persistent()
                .set(&to_key, &(to_balance + amount));
        }
    }

    #[contracttype]
    enum StableKey {
        Balance(Address),
    }

    struct Fixture<'a> {
        asset: AssetTokenContractClient<'a>,
        stable: MockStableClient<'a>,
        stable_id: Address,
        admin: Address,
        alice: Address,
        bob: Address,
    }

    /// Asset token with supply 1000: admin 0, alice 600, bob 400.
    fn setup(env: &Env) -> Fixture<'_> {
        env.mock_all_auths();
        let compliance = env.register(AllowAll, ());
        let asset_id = env.register(AssetTokenContract, ());
        let stable_id = env.register(MockStable, ());
        let asset = AssetTokenContractClient::new(env, &asset_id);
        let stable = MockStableClient::new(env, &stable_id);
        let admin = Address::generate(env);
        let alice = Address::generate(env);
        let bob = Address::generate(env);
        let s = |x: &str| String::from_str(env, x);
        asset.initialize(
            &admin,
            &s("A"),
            &s("A"),
            &s("re"),
            &1000,
            &7,
            &compliance,
            &s("d"),
            &0,
        );
        asset.transfer(&admin, &alice, &600);
        asset.transfer(&admin, &bob, &400);
        Fixture {
            asset,
            stable,
            stable_id,
            admin,
            alice,
            bob,
        }
    }

    fn liquidate(f: &Fixture<'_>, proceeds: i128) {
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        if proceeds > 0 {
            f.stable.mint(&f.admin, &proceeds);
            f.asset.deposit_liquidation_proceeds(&f.admin, &proceeds);
        }
    }

    #[test]
    fn start_liquidation_sets_status_and_snapshot() {
        let env = Env::default();
        let f = setup(&env);
        assert!(!f.asset.is_liquidated());
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        assert!(f.asset.is_liquidated());
        assert_eq!(f.asset.liquidation_snapshot_supply(), 1000);
        assert_eq!(f.asset.liquidation_proceeds(), (0, 0));
        assert_eq!(f.asset.liquidation_claimable(&f.alice), 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #12)")]
    fn transfers_are_frozen_permanently_after_liquidation() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        f.asset.transfer(&f.alice, &f.bob, &1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #31)")]
    fn liquidation_cannot_be_started_twice() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #30)")]
    fn deposit_before_liquidation_is_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.deposit_liquidation_proceeds(&f.admin, &100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #30)")]
    fn redeem_before_liquidation_is_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.redeem_liquidation_proceeds(&f.alice, &100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #32)")]
    fn zero_deposit_is_rejected() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        f.asset.deposit_liquidation_proceeds(&f.admin, &0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #32)")]
    fn zero_redemption_is_rejected() {
        let env = Env::default();
        let f = setup(&env);
        liquidate(&f, 1000);
        f.asset.redeem_liquidation_proceeds(&f.alice, &0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn redeem_more_than_balance_is_rejected() {
        let env = Env::default();
        let f = setup(&env);
        liquidate(&f, 1000);
        f.asset.redeem_liquidation_proceeds(&f.alice, &601);
    }

    #[test]
    fn pro_rata_redemption_pays_every_holder_their_share() {
        let env = Env::default();
        let f = setup(&env);
        liquidate(&f, 1000);

        assert_eq!(f.asset.liquidation_claimable(&f.alice), 600);
        assert_eq!(f.asset.liquidation_claimable(&f.bob), 400);

        let alice_payout = f.asset.redeem_liquidation_proceeds(&f.alice, &600);
        assert_eq!(alice_payout, 600);
        let bob_payout = f.asset.redeem_liquidation_proceeds(&f.bob, &400);
        assert_eq!(bob_payout, 400);

        assert_eq!(f.stable.balance(&f.alice), 600);
        assert_eq!(f.stable.balance(&f.bob), 400);
        // Tokens burned, supply drained, nothing left in the pool.
        assert_eq!(f.asset.balance(&f.alice), 0);
        assert_eq!(f.asset.balance(&f.bob), 0);
        assert_eq!(f.asset.total_supply(), 0);
        assert_eq!(f.asset.liquidation_proceeds(), (1000, 1000));
    }

    #[test]
    fn partial_redemption_is_pro_rata_and_leaves_the_remainder_claimable() {
        let env = Env::default();
        let f = setup(&env);
        liquidate(&f, 1000);

        let payout = f.asset.redeem_liquidation_proceeds(&f.alice, &300);
        assert_eq!(payout, 300);
        assert_eq!(f.asset.balance(&f.alice), 300);
        assert_eq!(f.asset.liquidation_claimable(&f.alice), 300);
        assert_eq!(f.asset.total_supply(), 700);

        // The rest is still redeemable at the same price.
        let rest = f.asset.redeem_liquidation_proceeds(&f.alice, &300);
        assert_eq!(rest, 300);
        assert_eq!(f.asset.balance(&f.alice), 0);
        assert_eq!(f.stable.balance(&f.alice), 600);
    }

    #[test]
    fn proceeds_deposited_in_tranches_accumulate() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        f.stable.mint(&f.admin, &1000);

        f.asset.deposit_liquidation_proceeds(&f.admin, &250);
        f.asset.deposit_liquidation_proceeds(&f.admin, &750);
        assert_eq!(f.asset.liquidation_proceeds(), (1000, 0));

        assert_eq!(f.asset.redeem_liquidation_proceeds(&f.alice, &600), 600);
        assert_eq!(f.asset.redeem_liquidation_proceeds(&f.bob, &400), 400);
        assert_eq!(f.asset.liquidation_proceeds(), (1000, 1000));
    }

    #[test]
    fn redeeming_full_balance_after_partial_proceeds_leaves_dust_in_pool() {
        let env = Env::default();
        let f = setup(&env);
        f.asset.start_liquidation(&f.admin, &f.stable_id);
        f.stable.mint(&f.admin, &1000);
        // Only 3 units are in the pool for 1000 tokens: 600 -> floor(1.8) = 1.
        f.asset.deposit_liquidation_proceeds(&f.admin, &3);
        let payout = f.asset.redeem_liquidation_proceeds(&f.alice, &600);
        assert_eq!(payout, 1);
        assert_eq!(f.asset.liquidation_proceeds(), (3, 1));
    }
}
