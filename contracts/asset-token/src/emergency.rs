//! A configured liquidation vault is immutable and exclusively controls the
//! permanent freeze and settlement burns. The administrator cannot bypass it
//! through the legacy liquidation entrypoints or a WASM upgrade.
use soroban_sdk::{
    contractclient, contractimpl, panic_with_error, symbol_short, Address, Env, U256,
};

use crate::{
    multi_class, stock_split, AssetStatus, AssetTokenContract, AssetTokenContractArgs,
    AssetTokenContractClient, DataKey, Error,
};

#[contractclient(name = "VaultInfoClient")]
#[allow(dead_code)]
trait VaultInfo {
    fn vault_asset(env: Env) -> Address;
}

pub(crate) fn require_no_vault(env: &Env) {
    if env.storage().instance().has(&DataKey::EmergencyVault) {
        panic_with_error!(env, Error::VaultConfigured);
    }
}

fn require_vault(env: &Env, vault: &Address) {
    let configured: Address = env
        .storage()
        .instance()
        .get(&DataKey::EmergencyVault)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidVault));
    if vault != &configured {
        panic_with_error!(env, Error::InvalidVault);
    }
}

/// floor(units * proceeds / supply), without narrowing the intermediate product.
pub fn pro_rata(env: &Env, units: i128, proceeds: i128, supply: i128) -> i128 {
    if units < 0 || proceeds < 0 || supply <= 0 || units > supply {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let value = U256::from_u128(env, units as u128)
        .mul(&U256::from_u128(env, proceeds as u128))
        .div(&U256::from_u128(env, supply as u128))
        .to_u128()
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
    i128::try_from(value).unwrap_or_else(|_| panic_with_error!(env, Error::Overflow))
}

#[contractimpl]
impl AssetTokenContract {
    pub fn token_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
    }

    pub fn set_emergency_vault(env: Env, admin: Address, vault: Address) {
        Self::require_admin(&env, &admin);
        Self::require_active(&env);
        require_no_vault(&env);
        if vault == env.current_contract_address() {
            panic_with_error!(env, Error::InvalidVault);
        }
        // Reject account addresses and vaults deployed for a different asset.
        if VaultInfoClient::new(&env, &vault).vault_asset() != env.current_contract_address() {
            panic_with_error!(env, Error::InvalidVault);
        }
        env.storage()
            .instance()
            .set(&DataKey::EmergencyVault, &vault);
        env.events().publish((symbol_short!("emervault"),), vault);
    }

    pub fn activate_liquidation(env: Env, vault: Address) {
        require_vault(&env, &vault);
        vault.require_auth();
        Self::require_active(&env);
        if Self::total_supply(env.clone()) <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::AssetStatus, &AssetStatus::Liquidated);
        env.events().publish((symbol_short!("emergliq"),), vault);
    }

    /// Share class: None for ordinary tokens, Some(0) for A, Some(1) for B.
    /// Both the settlement vault and the beneficial holder must authorize.
    pub fn liquidation_burn(
        env: Env,
        vault: Address,
        holder: Address,
        amount: i128,
        share_class: Option<u32>,
    ) {
        require_vault(&env, &vault);
        vault.require_auth();
        holder.require_auth();
        if Self::status(env.clone()) != AssetStatus::Liquidated {
            panic_with_error!(env, Error::NotLiquidated);
        }
        if multi_class::enabled(&env) {
            let id =
                share_class.unwrap_or_else(|| panic_with_error!(env, Error::InvalidShareClass));
            multi_class::burn(&env, &holder, multi_class::from_id(&env, id), amount);
        } else {
            if share_class.is_some() {
                panic_with_error!(env, Error::InvalidShareClass);
            }
            if amount <= 0 {
                panic_with_error!(env, Error::InvalidAmount);
            }
            let held = Self::balance(env.clone(), holder.clone());
            if held < amount {
                panic_with_error!(env, Error::InsufficientBalance);
            }
            stock_split::set_balance(&env, &holder, held - amount);
            let total = Self::total_supply(env.clone())
                .checked_sub(amount)
                .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
            if total < 0 {
                panic_with_error!(env, Error::InsufficientBalance);
            }
            env.storage().instance().set(&DataKey::TotalSupply, &total);
        }
        env.events()
            .publish((symbol_short!("liqburn"), holder), amount);
    }
}
