//! Issue #90 — forward / reverse share splits via a global multiplier.
//!
//! Executing a split is O(1): it appends `(num, den)` to a split history,
//! updates the cumulative multiplier and rescales total supply. Holder
//! balances are *not* iterated; each holder records the number of splits
//! already applied to their stored balance (`HolderEpoch`) and is settled
//! lazily by replaying the missed splits the next time the balance is read
//! or written. Each step is `floor(balance * num / den)`.
//!
//! Precision note: forward splits (`den == 1`) are exact. Reverse splits
//! floor fractional shares per holder, so the sum of settled balances can be
//! slightly below the (also floored) total supply. Fractional-share cash-out
//! is out of scope.
use soroban_sdk::{panic_with_error, Address, Env, Symbol};

use crate::{DataKey, Error};

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

pub fn split_count(env: &Env) -> u32 {
    env.storage().instance().get(&DataKey::SplitCount).unwrap_or(0)
}

pub fn multiplier(env: &Env) -> (u128, u128) {
    env.storage()
        .instance()
        .get(&DataKey::SplitMultiplier)
        .unwrap_or((1, 1))
}

fn scale(env: &Env, value: i128, num: u32, den: u32) -> i128 {
    value
        .checked_mul(num as i128)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow))
        / den as i128
}

pub fn execute(env: &Env, num: u32, den: u32) {
    if num == 0 || den == 0 {
        panic_with_error!(env, Error::InvalidSplitRatio);
    }
    let count = split_count(env);
    let (mn, md) = multiplier(env);
    let n = mn
        .checked_mul(num as u128)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidSplitRatio));
    let d = md
        .checked_mul(den as u128)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidSplitRatio));
    let g = gcd(n, d);
    let (n, d) = (n / g, d / g);

    let supply: i128 = env.storage().instance().get(&DataKey::TotalSupply).unwrap_or(0);
    let new_supply = scale(env, supply, num, den);

    env.storage().persistent().set(&DataKey::Split(count), &(num, den));
    env.storage().instance().set(&DataKey::SplitCount, &(count + 1));
    env.storage().instance().set(&DataKey::SplitMultiplier, &(n, d));
    env.storage().instance().set(&DataKey::TotalSupply, &new_supply);

    env.events().publish(
        (Symbol::new(env, "ShareSplit"),),
        (n, d, new_supply, num, den),
    );
}

fn settle(env: &Env, holder: &Address) -> i128 {
    let raw: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::Balance(holder.clone()))
        .unwrap_or(0);
    let count = split_count(env);
    let epoch: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::HolderEpoch(holder.clone()))
        .unwrap_or(0);
    let mut value = raw;
    for i in epoch..count {
        let (num, den): (u32, u32) = env.storage().persistent().get(&DataKey::Split(i)).unwrap();
        value = scale(env, value, num, den);
    }
    value
}

/// Balance after applying all splits not yet applied to the stored value.
pub fn effective_balance(env: &Env, holder: &Address) -> i128 {
    settle(env, holder)
}

/// Store an already-effective balance and mark the holder as fully settled.
pub fn set_balance(env: &Env, holder: &Address, value: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::Balance(holder.clone()), &value);
    env.storage()
        .persistent()
        .set(&DataKey::HolderEpoch(holder.clone()), &split_count(env));
}

#[cfg(test)]
mod test {
    use crate::{AssetTokenContract, AssetTokenContractClient};
    use soroban_sdk::{
        contract, contractimpl, testutils::Address as _, Address, Env, String,
    };

    #[contract]
    struct AllowAll;
    #[contractimpl]
    impl AllowAll {
        pub fn is_allowed(_env: Env, _who: Address) -> bool {
            true
        }
    }

    fn setup(env: &Env) -> (AssetTokenContractClient<'_>, Address, Address, Address) {
        env.mock_all_auths();
        let compliance = env.register(AllowAll, ());
        let id = env.register(AssetTokenContract, ());
        let c = AssetTokenContractClient::new(env, &id);
        let admin = Address::generate(env);
        let alice = Address::generate(env);
        let bob = Address::generate(env);
        let s = |x: &str| String::from_str(env, x);
        c.initialize(&admin, &s("A"), &s("A"), &s("re"), &1000, &7, &compliance, &s("d"), &0);
        c.transfer(&admin, &alice, &300);
        c.transfer(&admin, &bob, &101);
        (c, admin, alice, bob)
    }

    #[test]
    fn forward_split_scales_everyone_exactly() {
        let env = Env::default();
        let (c, admin, alice, bob) = setup(&env);
        c.execute_share_split(&admin, &2, &1);
        assert_eq!(c.total_supply(), 2000);
        assert_eq!(c.balance(&alice), 600);
        assert_eq!(c.balance(&bob), 202);
        assert_eq!(c.balance(&admin), 1198);
        assert_eq!(c.split_multiplier(), (2, 1));
        assert_eq!(c.split_count(), 1);
    }

    #[test]
    fn reverse_split_floors() {
        let env = Env::default();
        let (c, admin, alice, bob) = setup(&env);
        c.execute_share_split(&admin, &1, &5);
        assert_eq!(c.total_supply(), 200);
        assert_eq!(c.balance(&alice), 60);
        assert_eq!(c.balance(&bob), 20); // 101/5 floored
    }

    #[test]
    fn transfers_after_split_and_lazy_settle_across_multiple_splits() {
        let env = Env::default();
        let (c, admin, alice, bob) = setup(&env);
        c.execute_share_split(&admin, &3, &1);
        c.execute_share_split(&admin, &1, &2);
        assert_eq!(c.split_multiplier(), (3, 2));
        assert_eq!(c.balance(&alice), 450);
        c.transfer(&alice, &bob, &50);
        assert_eq!(c.balance(&alice), 400);
        assert_eq!(c.balance(&bob), 101 * 3 / 1 / 2 + 50);
        c.execute_share_split(&admin, &2, &1);
        assert_eq!(c.balance(&alice), 800);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #11)")]
    fn zero_ratio_rejected() {
        let env = Env::default();
        let (c, admin, _, _) = setup(&env);
        c.execute_share_split(&admin, &0, &1);
    }

    #[test]
    #[should_panic]
    fn non_admin_rejected() {
        let env = Env::default();
        let (c, _, alice, _) = setup(&env);
        env.set_auths(&[]);
        c.execute_share_split(&alice, &2, &1);
    }

    #[test]
    fn emits_share_split_event() {
        use soroban_sdk::testutils::Events;
        let env = Env::default();
        let (c, admin, _, _) = setup(&env);
        c.execute_share_split(&admin, &2, &1);
        assert!(!env.events().all().events().is_empty());
    }
}
