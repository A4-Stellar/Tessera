//! Dual-class equity. Per-class balances partition the ordinary token balance;
//! per-class supplies sum to `total_supply`. Enable only on an empty token.
//! Ordinary mutations are disabled in this mode to prevent unclassified units.
use soroban_sdk::{contractimpl, contracttype, panic_with_error, symbol_short, Address, Env};

use crate::{
    stock_split, AssetTokenContract, AssetTokenContractArgs, AssetTokenContractClient, DataKey,
    Error,
};

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareClass {
    ClassA,
    ClassB,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassPolicy {
    pub transfers_enabled: bool,
    /// Only Class A supports conversion on a transfer to another account.
    pub convert_on_transfer: bool,
}

#[contracttype]
#[derive(Clone)]
enum ClassKey {
    ClassesEnabled,
    ClassSupply(ShareClass),
    ClassBalance(ShareClass, Address),
    ClassPolicy(ShareClass),
}

pub(crate) fn enabled(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&ClassKey::ClassesEnabled)
        .unwrap_or(false)
}

pub(crate) fn require_single_class(env: &Env) {
    if enabled(env) {
        panic_with_error!(env, Error::MultiClassEnabled);
    }
}

fn require_enabled(env: &Env) {
    if !enabled(env) {
        panic_with_error!(env, Error::MultiClassDisabled);
    }
}

fn balance(env: &Env, class: ShareClass, holder: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&ClassKey::ClassBalance(class, holder.clone()))
        .unwrap_or(0)
}

fn supply(env: &Env, class: ShareClass) -> i128 {
    env.storage()
        .instance()
        .get(&ClassKey::ClassSupply(class))
        .unwrap_or(0)
}

fn set_balance(env: &Env, class: ShareClass, holder: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&ClassKey::ClassBalance(class, holder.clone()), &amount);
}

fn add(env: &Env, a: i128, b: i128) -> i128 {
    a.checked_add(b)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow))
}

fn policy(env: &Env, class: ShareClass) -> ClassPolicy {
    env.storage()
        .instance()
        .get(&ClassKey::ClassPolicy(class))
        .unwrap_or(ClassPolicy {
            transfers_enabled: true,
            convert_on_transfer: class == ShareClass::ClassA,
        })
}

fn require_holder(env: &Env, holder: &Address) {
    holder.require_auth();
    AssetTokenContract::require_not_paused(env);
    AssetTokenContract::require_active(env);
    AssetTokenContract::require_not_locked(env, holder);
}

fn require_compliant(env: &Env, holder: &Address, error: Error) {
    let compliance: Address = env.storage().instance().get(&DataKey::Compliance).unwrap();
    if !AssetTokenContract::compliance_allows(env, &compliance, holder) {
        panic_with_error!(env, error);
    }
}

/// Used only by the holder-authorized burn and the configured liquidation vault.
pub(crate) fn burn(env: &Env, holder: &Address, class: ShareClass, amount: i128) {
    require_enabled(env);
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let held = balance(env, class, holder);
    if held < amount {
        panic_with_error!(env, Error::InsufficientBalance);
    }
    set_balance(env, class, holder, held - amount);
    env.storage().instance().set(
        &ClassKey::ClassSupply(class),
        &(supply(env, class) - amount),
    );
    let aggregate = AssetTokenContract::balance(env.clone(), holder.clone());
    stock_split::set_balance(env, holder, aggregate - amount);
    env.storage().instance().set(
        &DataKey::TotalSupply,
        &(AssetTokenContract::total_supply(env.clone()) - amount),
    );
    env.events()
        .publish((symbol_short!("clsburn"), class, holder.clone()), amount);
}

pub(crate) fn from_id(env: &Env, id: u32) -> ShareClass {
    match id {
        0 => ShareClass::ClassA,
        1 => ShareClass::ClassB,
        _ => panic_with_error!(env, Error::InvalidShareClass),
    }
}

#[contractimpl]
impl AssetTokenContract {
    pub fn enable_multi_class(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        require_single_class(&env);
        if Self::total_supply(env.clone()) != 0 {
            panic_with_error!(env, Error::NonZeroSupply);
        }
        env.storage()
            .instance()
            .set(&ClassKey::ClassesEnabled, &true);
        env.events().publish((symbol_short!("classes"),), admin);
    }

    pub fn set_class_policy(env: Env, admin: Address, class: ShareClass, policy: ClassPolicy) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        require_enabled(&env);
        if class == ShareClass::ClassB && policy.convert_on_transfer {
            panic_with_error!(env, Error::InvalidConversion);
        }
        env.storage()
            .instance()
            .set(&ClassKey::ClassPolicy(class), &policy);
        env.events()
            .publish((symbol_short!("clspolicy"), class), policy);
    }

    pub fn class_policy(env: Env, class: ShareClass) -> ClassPolicy {
        policy(&env, class)
    }

    pub fn class_supply(env: Env, class: ShareClass) -> i128 {
        supply(&env, class)
    }

    pub fn class_balance(env: Env, class: ShareClass, holder: Address) -> i128 {
        balance(&env, class, &holder)
    }

    pub fn mint_class(env: Env, admin: Address, class: ShareClass, to: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        Self::require_not_paused(&env);
        require_enabled(&env);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        require_compliant(&env, &to, Error::RecipientNotCompliant);
        let new_supply = add(&env, supply(&env, class), amount);
        if class == ShareClass::ClassA && new_supply.checked_mul(10).is_none() {
            panic_with_error!(env, Error::Overflow);
        }
        let aggregate_supply = add(&env, Self::total_supply(env.clone()), amount);
        let aggregate_balance = add(&env, Self::balance(env.clone(), to.clone()), amount);
        set_balance(
            &env,
            class,
            &to,
            add(&env, balance(&env, class, &to), amount),
        );
        env.storage()
            .instance()
            .set(&ClassKey::ClassSupply(class), &new_supply);
        stock_split::set_balance(&env, &to, aggregate_balance);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &aggregate_supply);
        env.events()
            .publish((symbol_short!("clsmint"), class, to), amount);
    }

    pub fn transfer_class(env: Env, class: ShareClass, from: Address, to: Address, amount: i128) {
        require_enabled(&env);
        require_holder(&env, &from);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let rules = policy(&env, class);
        if !rules.transfers_enabled {
            panic_with_error!(env, Error::ClassTransferDisabled);
        }
        require_compliant(&env, &from, Error::SenderNotCompliant);
        require_compliant(&env, &to, Error::RecipientNotCompliant);
        let held = balance(&env, class, &from);
        if held < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        if from == to {
            return;
        }
        let destination = if rules.convert_on_transfer {
            ShareClass::ClassB
        } else {
            class
        };
        let credited = add(&env, balance(&env, destination, &to), amount);
        set_balance(&env, class, &from, held - amount);
        set_balance(&env, destination, &to, credited);
        if destination != class {
            env.storage().instance().set(
                &ClassKey::ClassSupply(class),
                &(supply(&env, class) - amount),
            );
            env.storage().instance().set(
                &ClassKey::ClassSupply(destination),
                &add(&env, supply(&env, destination), amount),
            );
        }
        let debited = Self::balance(env.clone(), from.clone()) - amount;
        let credited = add(&env, Self::balance(env.clone(), to.clone()), amount);
        stock_split::set_balance(&env, &from, debited);
        stock_split::set_balance(&env, &to, credited);
        env.events().publish(
            (symbol_short!("clsxfer"), class, destination),
            (from, to, amount),
        );
    }

    /// Irreversible 1:1 Class A -> B conversion. No B -> A voting upgrade exists.
    pub fn convert_class(env: Env, holder: Address, amount: i128) {
        require_enabled(&env);
        require_holder(&env, &holder);
        require_compliant(&env, &holder, Error::SenderNotCompliant);
        let held = balance(&env, ShareClass::ClassA, &holder);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if held < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }
        let credited = add(&env, balance(&env, ShareClass::ClassB, &holder), amount);
        set_balance(&env, ShareClass::ClassA, &holder, held - amount);
        set_balance(&env, ShareClass::ClassB, &holder, credited);
        env.storage().instance().set(
            &ClassKey::ClassSupply(ShareClass::ClassA),
            &(supply(&env, ShareClass::ClassA) - amount),
        );
        env.storage().instance().set(
            &ClassKey::ClassSupply(ShareClass::ClassB),
            &add(&env, supply(&env, ShareClass::ClassB), amount),
        );
        env.events()
            .publish((symbol_short!("clsconv"), holder), amount);
    }

    pub fn burn_class(env: Env, holder: Address, class: ShareClass, amount: i128) {
        require_holder(&env, &holder);
        burn(&env, &holder, class, amount);
    }

    pub fn voting_weight(env: Env, holder: Address) -> i128 {
        balance(&env, ShareClass::ClassA, &holder)
            .checked_mul(10)
            .unwrap_or_else(|| panic_with_error!(env, Error::Overflow))
    }

    pub fn dividend_weight(env: Env, holder: Address) -> i128 {
        add(
            &env,
            balance(&env, ShareClass::ClassA, &holder),
            balance(&env, ShareClass::ClassB, &holder),
        )
    }
}
