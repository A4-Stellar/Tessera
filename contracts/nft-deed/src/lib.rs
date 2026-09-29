//! NFT Property Deed & Legal Title Binding Contract.
//!
//! Binds fractional ERC-20 style asset tokens to a single master NFT
//! representing the legal deed title stored in municipal land registries.
//!
//! # Architecture
//!
//! - The master NFT (token_id `0`) represents the legal deed title.
//! - While fractional tokens are actively trading, the master NFT is
//!   locked in a vault contract.
//! - To unlock and transfer the master NFT, 100% of the fractional tokens
//!   must be redeemed (burned) back to the asset-token contract.
//! - Once all tokens are redeemed, the NFT can be transferred to the
//!   redeemer, completing the legal title transfer.
//!
//! # State machine
//!
//! ```text
//! Locked ──100% redemption──► Unlocked ──transfer_nft──► Transferred
//! ```
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    BytesN, Env, IntoVal, String, Symbol, Val, Vec,
};

// ---- Error codes --------------------------------------------------------- //

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    NotLocked = 3,
    StillLocked = 4,
    NoTokensToRedeem = 5,
    RedemptionIncomplete = 6,
    InvalidTokenId = 7,
    NftNotOwned = 8,
    TransferFailed = 9,
    VaultNotConfigured = 10,
    FractionalTokenNotSet = 11,
    MetadataNotFound = 12,
    Overflow = 13,
}

// ---- Storage keys -------------------------------------------------------- //

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    /// The fractional asset-token contract address bound to this deed.
    FractionalToken,
    /// The vault contract address where the master NFT is locked.
    Vault,
    /// The current owner of the master NFT (while locked, this is the vault).
    NftOwner,
    /// The legal deed metadata URI (e.g. IPFS hash of the deed document).
    DeedUri,
    /// The municipal land registry identifier.
    RegistryId,
    /// Total supply of fractional tokens at the time of binding.
    FractionalTotalSupply,
    /// Whether the NFT has been unlocked (all tokens redeemed).
    Unlocked,
    /// The beneficiary who can claim the NFT upon full redemption.
    Beneficiary,
}

// ---- Contract ------------------------------------------------------------ //

#[contract]
pub struct NftDeedContract;

/// The token ID of the master NFT representing the legal deed.
pub const MASTER_TOKEN_ID: u64 = 0;

#[contractimpl]
impl NftDeedContract {
    // ------------------------------------------------------------------
    // Initialization
    // ------------------------------------------------------------------

    /// Initialize the NFT deed contract.
    ///
    /// - `admin`              — contract administrator.
    /// - `fractional_token`   — the fractional asset-token contract address.
    /// - `vault`              — the vault contract where the NFT is locked.
    /// - `deed_uri`           — URI of the legal deed document (e.g. IPFS).
    /// - `registry_id`        — municipal land registry identifier.
    /// - `beneficiary`        — the address that can claim the NFT upon full
    ///                          redemption of all fractional tokens.
    pub fn initialize(
        env: Env,
        admin: Address,
        fractional_token: Address,
        vault: Address,
        deed_uri: String,
        registry_id: String,
        beneficiary: Address,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::FractionalToken, &fractional_token);
        env.storage().instance().set(&DataKey::Vault, &vault);
        env.storage().instance().set(&DataKey::NftOwner, &vault);
        env.storage().instance().set(&DataKey::DeedUri, &deed_uri);
        env.storage()
            .instance()
            .set(&DataKey::RegistryId, &registry_id);
        env.storage().instance().set(&DataKey::Unlocked, &false);
        env.storage()
            .instance()
            .set(&DataKey::Beneficiary, &beneficiary);

        // Record the total supply of fractional tokens at binding time.
        let total_supply: i128 = Self::query_fractional_supply(&env, &fractional_token);
        env.storage()
            .instance()
            .set(&DataKey::FractionalTotalSupply, &total_supply);

        env.events().publish(
            (symbol_short!("bind"), admin),
            (fractional_token, vault, registry_id),
        );
    }

    // ------------------------------------------------------------------
    // NFT Standard Methods
    // ------------------------------------------------------------------

    /// Returns the owner of the given token ID.
    ///
    /// While the NFT is locked, the owner is the vault contract.
    /// After unlocking, the owner is the beneficiary who redeemed all tokens.
    pub fn owner_of(env: Env, token_id: u64) -> Address {
        if token_id != MASTER_TOKEN_ID {
            panic_with_error!(env, Error::InvalidTokenId);
        }
        env.storage()
            .instance()
            .get(&DataKey::NftOwner)
            .unwrap_or_else(|| panic_with_error!(env, Error::NftNotOwned))
    }

    /// Transfer the master NFT to a new owner.
    ///
    /// Can only be called when the NFT is unlocked (all fractional tokens
    /// have been redeemed). The caller must be the current owner.
    pub fn transfer_nft(env: Env, from: Address, to: Address, token_id: u64) {
        if token_id != MASTER_TOKEN_ID {
            panic_with_error!(env, Error::InvalidTokenId);
        }
        from.require_auth();

        let unlocked: bool = env
            .storage()
            .instance()
            .get(&DataKey::Unlocked)
            .unwrap_or(false);
        if !unlocked {
            panic_with_error!(env, Error::StillLocked);
        }

        let current_owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::NftOwner)
            .unwrap_or_else(|| panic_with_error!(env, Error::NftNotOwned));
        if from != current_owner {
            panic_with_error!(env, Error::Unauthorized);
        }

        env.storage()
            .instance()
            .set(&DataKey::NftOwner, &to);

        env.events().publish(
            (symbol_short!("ntransfer"), from, to),
            token_id,
        );
    }

    /// Returns the metadata URI for the given token ID.
    ///
    /// This URI points to the legal deed document stored in the municipal
    /// land registry (or an IPFS mirror of it).
    pub fn token_uri(env: Env, token_id: u64) -> String {
        if token_id != MASTER_TOKEN_ID {
            panic_with_error!(env, Error::InvalidTokenId);
        }
        env.storage()
            .instance()
            .get(&DataKey::DeedUri)
            .unwrap_or_else(|| panic_with_error!(env, Error::MetadataNotFound))
    }

    // ------------------------------------------------------------------
    // Redemption & Unlock
    // ------------------------------------------------------------------

    /// Redeem fractional tokens to unlock the master NFT.
    ///
    /// The caller specifies how many fractional tokens they are redeeming.
    /// Once the cumulative redeemed amount reaches 100% of the original
    /// total supply, the NFT is unlocked and can be transferred to the
    /// beneficiary.
    ///
    /// This function:
    /// 1. Burns the specified amount of fractional tokens from the caller.
    /// 2. Tracks cumulative redemptions.
    /// 3. If 100% redeemed, unlocks the NFT and assigns ownership to the
    ///    beneficiary.
    pub fn redeem_and_unlock(env: Env, holder: Address, amount: i128) {
        holder.require_auth();

        if amount <= 0 {
            panic_with_error!(env, Error::NoTokensToRedeem);
        }

        let fractional_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::FractionalToken)
            .unwrap_or_else(|| panic_with_error!(env, Error::FractionalTokenNotSet));

        let total_supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::FractionalTotalSupply)
            .unwrap_or(0);

        // Burn the fractional tokens from the holder.
        let burn_args: Vec<Val> = (holder.clone(), amount).into_val(&env);
        let _: () = env.invoke_contract(
            &fractional_token,
            &Symbol::new(&env, "burn"),
            burn_args,
        );

        // Calculate cumulative redeemed amount.
        let current_supply: i128 = Self::query_fractional_supply(&env, &fractional_token);
        let redeemed = total_supply.saturating_sub(current_supply);

        if redeemed >= total_supply {
            // 100% redeemed — unlock the NFT.
            let beneficiary: Address = env
                .storage()
                .instance()
                .get(&DataKey::Beneficiary)
                .unwrap();
            env.storage()
                .instance()
                .set(&DataKey::NftOwner, &beneficiary);
            env.storage().instance().set(&DataKey::Unlocked, &true);

            env.events().publish(
                (symbol_short!("unlock"), holder),
                (redeemed, total_supply),
            );
        } else {
            env.events().publish(
                (symbol_short!("redeem"), holder),
                (redeemed, total_supply),
            );
        }
    }

    /// Check if the master NFT is unlocked (all fractional tokens redeemed).
    pub fn is_unlocked(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Unlocked)
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------
    // View functions
    // ------------------------------------------------------------------

    /// Returns the bound fractional asset-token contract address.
    pub fn fractional_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::FractionalToken)
            .unwrap_or_else(|| panic_with_error!(env, Error::FractionalTokenNotSet))
    }

    /// Returns the vault contract address.
    pub fn vault(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Vault)
            .unwrap_or_else(|| panic_with_error!(env, Error::VaultNotConfigured))
    }

    /// Returns the beneficiary address.
    pub fn beneficiary(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap()
    }

    /// Returns the municipal land registry identifier.
    pub fn registry_id(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::RegistryId)
            .unwrap()
    }

    /// Returns the original total supply of fractional tokens at binding time.
    pub fn fractional_total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::FractionalTotalSupply)
            .unwrap_or(0)
    }

    /// Returns the current total supply of fractional tokens.
    pub fn current_fractional_supply(env: Env) -> i128 {
        let fractional_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::FractionalToken)
            .unwrap_or_else(|| panic_with_error!(env, Error::FractionalTokenNotSet));
        Self::query_fractional_supply(&env, &fractional_token)
    }

    /// Returns the contract admin.
    pub fn admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    /// Current contract ABI version, polled by the off-chain indexer.
    pub fn version(_env: Env) -> u64 {
        1
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn query_fractional_supply(env: &Env, fractional_token: &Address) -> i128 {
        let args: Vec<Val> = ().into_val(env);
        env.invoke_contract(fractional_token, &Symbol::new(env, "total_supply"), args)
    }
}

// ---- Unit tests ---------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    use super::*;

    #[contract]
    struct MockFractionalToken;
    #[contractimpl]
    impl MockFractionalToken {
        pub fn initialize(env: Env, total_supply: i128) {
            env.storage()
                .instance()
                .set(&DataKey::TotalSupply, &total_supply);
        }
        pub fn total_supply(env: Env) -> i128 {
            env.storage()
                .instance()
                .get(&DataKey::TotalSupply)
                .unwrap_or(0)
        }
        pub fn burn(env: Env, from: Address, amount: i128) {
            let supply: i128 = env
                .storage()
                .instance()
                .get(&DataKey::TotalSupply)
                .unwrap_or(0);
            env.storage()
                .instance()
                .set(&DataKey::TotalSupply, &(supply - amount));
            env.events()
                .publish((symbol_short!("burn"), from), amount);
        }
    }

    // We need a local DataKey for the mock since it's a separate contract.
    #[derive(Clone)]
    #[contracttype]
    enum DataKey {
        TotalSupply,
    }

    use soroban_sdk::{contract, contractimpl};

    fn setup(env: &Env) -> (NftDeedContractClient<'_>, Address, Address, Address, Address) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let fractional_token = Address::generate(env);
        let vault = Address::generate(env);
        let beneficiary = Address::generate(env);

        // Deploy and initialize the mock fractional token.
        let ft_id = env.register(MockFractionalToken, ());
        let ft = MockFractionalTokenClient::new(env, &ft_id);
        ft.initialize(&1_000_000);

        let id = env.register(NftDeedContract, ());
        let c = NftDeedContractClient::new(env, &id);
        let deed_uri = String::from_str(env, "ipfs://QmDeed123");
        let registry_id = String::from_str(env, "LAND-REG-2024-001");
        c.initialize(
            &admin,
            &ft_id,
            &vault,
            &deed_uri,
            &registry_id,
            &beneficiary,
        );

        (c, admin, ft_id, vault, beneficiary)
    }

    #[test]
    fn initialize_binds_correctly() {
        let env = Env::default();
        let (c, _admin, ft_id, vault, beneficiary) = setup(&env);

        assert_eq!(c.fractional_token(), ft_id);
        assert_eq!(c.vault(), vault);
        assert_eq!(c.beneficiary(), beneficiary);
        assert_eq!(c.fractional_total_supply(), 1_000_000);
        assert!(!c.is_unlocked());
    }

    #[test]
    fn owner_of_returns_vault_when_locked() {
        let env = Env::default();
        let (c, _admin, _ft_id, vault, _beneficiary) = setup(&env);

        assert_eq!(c.owner_of(&MASTER_TOKEN_ID), vault);
    }

    #[test]
    fn token_uri_returns_deed_uri() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, _beneficiary) = setup(&env);

        let uri = c.token_uri(&MASTER_TOKEN_ID);
        assert_eq!(uri, String::from_str(&env, "ipfs://QmDeed123"));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn invalid_token_id_rejected() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, _beneficiary) = setup(&env);

        c.owner_of(&999);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn transfer_rejected_while_locked() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, beneficiary) = setup(&env);

        c.transfer_nft(&beneficiary, &beneficiary, &MASTER_TOKEN_ID);
    }

    #[test]
    fn redeem_partial_does_not_unlock() {
        let env = Env::default();
        let (c, _admin, _ft_id, vault, _beneficiary) = setup(&env);

        let holder = Address::generate(&env);
        c.redeem_and_unlock(&holder, &500_000);

        assert!(!c.is_unlocked());
        assert_eq!(c.owner_of(&MASTER_TOKEN_ID), vault);
    }

    #[test]
    fn redeem_full_unlocks_and_transfers() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, beneficiary) = setup(&env);

        let holder = Address::generate(&env);
        c.redeem_and_unlock(&holder, &1_000_000);

        assert!(c.is_unlocked());
        assert_eq!(c.owner_of(&MASTER_TOKEN_ID), beneficiary);

        // Now the beneficiary can transfer the NFT.
        let new_owner = Address::generate(&env);
        c.transfer_nft(&beneficiary, &new_owner, &MASTER_TOKEN_ID);
        assert_eq!(c.owner_of(&MASTER_TOKEN_ID), new_owner);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #5)")]
    fn redeem_zero_rejected() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, _beneficiary) = setup(&env);

        let holder = Address::generate(&env);
        c.redeem_and_unlock(&holder, &0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #2)")]
    fn transfer_by_non_owner_rejected() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, beneficiary) = setup(&env);

        let holder = Address::generate(&env);
        c.redeem_and_unlock(&holder, &1_000_000);

        let attacker = Address::generate(&env);
        c.transfer_nft(&attacker, &attacker, &MASTER_TOKEN_ID);
    }

    #[test]
    fn registry_id_returns_correct_value() {
        let env = Env::default();
        let (c, _admin, _ft_id, _vault, _beneficiary) = setup(&env);

        let reg_id = c.registry_id();
        assert_eq!(reg_id, String::from_str(&env, "LAND-REG-2024-001"));
    }
}
