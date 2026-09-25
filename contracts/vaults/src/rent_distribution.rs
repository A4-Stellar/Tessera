use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Token,
    TotalShares,
    RewardPerShareStored,
    LastUpdateTime,
    UserRewardPerTokenPaid(Address),
    Rewards(Address),
}

#[contract]
pub struct RentDistributionVault;

#[contractimpl]
impl RentDistributionVault {
    pub fn claim_yield(env: Env, holder: Address) {
        holder.require_auth();
        let _reward: i128 = env.storage().instance().get(&DataKey::Rewards(holder.clone())).unwrap_or(0);
        env.storage().instance().set(&DataKey::Rewards(holder), &0i128);
    }
    
    pub fn reinvest_yield(env: Env, holder: Address) {
        holder.require_auth();
        let _reward: i128 = env.storage().instance().get(&DataKey::Rewards(holder.clone())).unwrap_or(0);
        env.storage().instance().set(&DataKey::Rewards(holder), &0i128);
    }
}
