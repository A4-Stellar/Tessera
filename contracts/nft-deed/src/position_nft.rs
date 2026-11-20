//! AMM position NFTs (issue #361).
//!
//! Every concentrated-liquidity position is represented by a non-fungible
//! token minted by the nft-deed crate. The AMM is the only account allowed to
//! mint or burn: `mint_position` checks the recorded AMM address and requires
//! its authorization, so a position can never be fabricated by a third party.
//!
//! Token ids are the AMM's monotonically increasing position ids, and each
//! token stores its tick range so wallets and indexers can display the range
//! without querying the pool.

#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PositionNftError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    PositionExists = 4,
    PositionNotFound = 5,
    InvalidRange = 6,
    NotOwner = 7,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionNft {
    pub owner: Address,
    pub amm: Address,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: i128,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Amm,
    Position(u64),
}

#[contract]
pub struct AmmPositionNft;

#[contractimpl]
impl AmmPositionNft {
    /// Bind the NFT collection to its AMM. The deployer authorizes this once.
    pub fn initialize(env: Env, admin: Address, amm: Address) {
        if env.storage().instance().has(&DataKey::Amm) {
            panic_with_error!(env, PositionNftError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Amm, &amm);
    }

    /// Mint a position NFT. Only the configured AMM may call this, and the
    /// AMM must authorize the call.
    pub fn mint_position(
        env: Env,
        to: Address,
        position_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: i128,
    ) -> u64 {
        if tick_lower >= tick_upper {
            panic_with_error!(env, PositionNftError::InvalidRange);
        }
        if liquidity <= 0 {
            panic_with_error!(env, PositionNftError::InvalidRange);
        }
        let amm = Self::require_amm(&env);
        amm.require_auth();
        let key = DataKey::Position(position_id);
        if env.storage().persistent().has(&key) {
            panic_with_error!(env, PositionNftError::PositionExists);
        }
        let nft = PositionNft {
            owner: to.clone(),
            amm: amm.clone(),
            tick_lower,
            tick_upper,
            liquidity,
        };
        env.storage().persistent().set(&key, &nft);
        env.storage()
            .persistent()
            .extend_ttl(&key, 17_280, 535_680);
        env.events().publish(
            (symbol_short!("mint"), to),
            (position_id, tick_lower, tick_upper, liquidity),
        );
        position_id
    }

    /// Burn a position NFT when its liquidity is fully withdrawn.
    pub fn burn_position(env: Env, position_id: u64) -> Address {
        let amm = Self::require_amm(&env);
        amm.require_auth();
        let key = DataKey::Position(position_id);
        let nft: PositionNft = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(env, PositionNftError::PositionNotFound));
        env.storage().persistent().remove(&key);
        env.events()
            .publish((symbol_short!("burn"), nft.owner.clone()), position_id);
        nft.owner
    }

    /// Update the recorded liquidity after a partial withdrawal.
    pub fn set_liquidity(env: Env, position_id: u64, liquidity: i128) {
        let amm = Self::require_amm(&env);
        amm.require_auth();
        let key = DataKey::Position(position_id);
        let mut nft: PositionNft = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(env, PositionNftError::PositionNotFound));
        nft.liquidity = liquidity;
        env.storage().persistent().set(&key, &nft);
        env.storage()
            .persistent()
            .extend_ttl(&key, 17_280, 535_680);
    }

    /// Current owner of a position NFT.
    pub fn owner_of(env: Env, position_id: u64) -> Address {
        Self::load(&env, position_id).owner
    }

    /// Full NFT metadata for a position.
    pub fn position(env: Env, position_id: u64) -> PositionNft {
        Self::load(&env, position_id)
    }

    /// The AMM allowed to mint and burn.
    pub fn amm(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Amm)
            .unwrap_or_else(|| panic_with_error!(env, PositionNftError::NotInitialized))
    }

    fn load(env: &Env, position_id: u64) -> PositionNft {
        env.storage()
            .persistent()
            .get(&DataKey::Position(position_id))
            .unwrap_or_else(|| panic_with_error!(env, PositionNftError::PositionNotFound))
    }

    fn require_amm(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Amm)
            .unwrap_or_else(|| panic_with_error!(env, PositionNftError::NotInitialized))
    }
}
