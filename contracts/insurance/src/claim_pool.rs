//! Decentralized Property Insurance Claim Escrow & Payout Pool (Issue #223).
//!
//! Enables tokenized property issuers to maintain an on-chain emergency insurance pool
//! funded by recurring premium deposits from rental yields, validates claim requests via
//! 3-of-5 independent claims assessors, and disburses approved claim payouts directly
//! to affected token holders pro-rata.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Map, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidAmount = 3,
    InvalidAssessors = 4,
    NotAssessor = 5,
    ClaimNotFound = 6,
    ClaimAlreadyProcessed = 7,
    AlreadyApproved = 8,
    InsufficientPoolFunds = 9,
    ZeroHolders = 10,
    TotalSharesZero = 11,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Pending,
    Approved,
    Disbursed,
    Rejected,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub id: u32,
    pub property_id: u32,
    pub amount_requested: i128,
    pub approvals: Vec<Address>,
    pub status: ClaimStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenHolderShare {
    pub holder: Address,
    pub shares: i128,
}

#[contracttype]
enum DataKey {
    Admin,
    Assessors,
    PoolBalance,
    YieldInterestBalance,
    ClaimCount,
    Claim(u32),
}

#[contract]
pub struct PropertyInsuranceClaimPool;

#[contractimpl]
impl PropertyInsuranceClaimPool {
    /// Initialize the insurance claim pool with an admin and exactly 5 independent assessors (3-of-5 threshold).
    pub fn initialize(env: Env, admin: Address, assessors: Vec<Address>) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        if assessors.len() != 5 {
            panic_with_error!(env, Error::InvalidAssessors);
        }
        // Verify unique assessors
        for i in 0..assessors.len() {
            for j in (i + 1)..assessors.len() {
                if assessors.get(i).unwrap() == assessors.get(j).unwrap() {
                    panic_with_error!(env, Error::InvalidAssessors);
                }
            }
        }

        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Assessors, &assessors);
        env.storage().instance().set(&DataKey::PoolBalance, &0i128);
        env.storage()
            .instance()
            .set(&DataKey::YieldInterestBalance, &0i128);
        env.storage().instance().set(&DataKey::ClaimCount, &0u32);
    }

    /// Accept recurring premium deposits from rental yields into the insurance vault.
    /// Also compounds interest into the vault balance.
    pub fn deposit_premium(env: Env, depositor: Address, amount: i128, accrued_interest: i128) {
        depositor.require_auth();
        if amount <= 0 || accrued_interest < 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let mut current_pool: i128 = env
            .storage()
            .instance()
            .get(&DataKey::PoolBalance)
            .unwrap_or(0);
        let mut interest_balance: i128 = env
            .storage()
            .instance()
            .get(&DataKey::YieldInterestBalance)
            .unwrap_or(0);

        current_pool = current_pool.checked_add(amount).expect("pool overflow");
        current_pool = current_pool
            .checked_add(accrued_interest)
            .expect("interest overflow");
        interest_balance = interest_balance
            .checked_add(accrued_interest)
            .expect("interest overflow");

        env.storage()
            .instance()
            .set(&DataKey::PoolBalance, &current_pool);
        env.storage()
            .instance()
            .set(&DataKey::YieldInterestBalance, &interest_balance);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("deposit"), depositor),
            (amount, accrued_interest, current_pool),
        );
    }

    /// File a claim request for property damage event.
    pub fn submit_claim(
        env: Env,
        caller: Address,
        property_id: u32,
        amount_requested: i128,
    ) -> u32 {
        caller.require_auth();
        if amount_requested <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }

        let claim_id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ClaimCount)
            .unwrap_or(0)
            + 1;

        let claim = Claim {
            id: claim_id,
            property_id,
            amount_requested,
            approvals: Vec::new(&env),
            status: ClaimStatus::Pending,
        };

        env.storage()
            .instance()
            .set(&DataKey::Claim(claim_id), &claim);
        env.storage()
            .instance()
            .set(&DataKey::ClaimCount, &claim_id);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("claim_sub"), caller),
            (claim_id, property_id, amount_requested),
        );

        claim_id
    }

    /// Assessors vote to validate a claim. Requires 3 distinct approvals out of the 5 assessors.
    pub fn assess_claim(env: Env, assessor: Address, claim_id: u32, approve: bool) -> ClaimStatus {
        assessor.require_auth();

        let assessors: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Assessors)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        let mut is_valid_assessor = false;
        for i in 0..assessors.len() {
            if assessors.get(i).unwrap() == assessor {
                is_valid_assessor = true;
                break;
            }
        }
        if !is_valid_assessor {
            panic_with_error!(env, Error::NotAssessor);
        }

        let mut claim: Claim = env
            .storage()
            .instance()
            .get(&DataKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ClaimNotFound));

        if claim.status != ClaimStatus::Pending {
            panic_with_error!(env, Error::ClaimAlreadyProcessed);
        }

        if !approve {
            // Assessors can reject or we check approvals
            return claim.status;
        }

        for i in 0..claim.approvals.len() {
            if claim.approvals.get(i).unwrap() == assessor {
                panic_with_error!(env, Error::AlreadyApproved);
            }
        }

        claim.approvals.push_back(assessor.clone());

        // 3-of-5 threshold check
        if claim.approvals.len() >= 3 {
            claim.status = ClaimStatus::Approved;
        }

        env.storage()
            .instance()
            .set(&DataKey::Claim(claim_id), &claim);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("assessed"), assessor),
            (claim_id, claim.approvals.len() as u32, claim.status as u32),
        );

        claim.status
    }

    /// Disburse approved claim payouts directly to affected token holders pro-rata based on their shares.
    /// Returns map of token holder address -> disbursed payout amount.
    pub fn disburse_claim(
        env: Env,
        caller: Address,
        claim_id: u32,
        holders: Vec<TokenHolderShare>,
    ) -> Map<Address, i128> {
        caller.require_auth();

        let mut claim: Claim = env
            .storage()
            .instance()
            .get(&DataKey::Claim(claim_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::ClaimNotFound));

        if claim.status != ClaimStatus::Approved {
            panic_with_error!(env, Error::ClaimAlreadyProcessed);
        }

        if holders.is_empty() {
            panic_with_error!(env, Error::ZeroHolders);
        }

        let mut pool_balance: i128 = env
            .storage()
            .instance()
            .get(&DataKey::PoolBalance)
            .unwrap_or(0);

        if pool_balance < claim.amount_requested {
            panic_with_error!(env, Error::InsufficientPoolFunds);
        }

        let mut total_shares: i128 = 0;
        for i in 0..holders.len() {
            let h = holders.get(i).unwrap();
            if h.shares <= 0 {
                panic_with_error!(env, Error::InvalidAmount);
            }
            total_shares = total_shares.checked_add(h.shares).expect("share overflow");
        }

        if total_shares == 0 {
            panic_with_error!(env, Error::TotalSharesZero);
        }

        let mut payouts: Map<Address, i128> = Map::new(&env);
        let mut total_distributed: i128 = 0;

        for i in 0..holders.len() {
            let h = holders.get(i).unwrap();
            let payout = (claim.amount_requested * h.shares) / total_shares;
            payouts.set(h.holder.clone(), payout);
            total_distributed = total_distributed
                .checked_add(payout)
                .expect("payout overflow");
        }

        // Deduct from pool balance
        pool_balance = pool_balance
            .checked_sub(total_distributed)
            .expect("underflow");
        env.storage()
            .instance()
            .set(&DataKey::PoolBalance, &pool_balance);

        claim.status = ClaimStatus::Disbursed;
        env.storage()
            .instance()
            .set(&DataKey::Claim(claim_id), &claim);

        #[allow(deprecated)]
        env.events().publish(
            (symbol_short!("disbursed"), caller),
            (claim_id, total_distributed, pool_balance),
        );

        payouts
    }

    /// Read current pool balance.
    pub fn get_pool_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::PoolBalance)
            .unwrap_or(0)
    }

    /// Read claim details.
    pub fn get_claim(env: Env, claim_id: u32) -> Option<Claim> {
        env.storage().instance().get(&DataKey::Claim(claim_id))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Env};

    #[test]
    fn test_claim_pool_lifecycle_and_pro_rata_payout() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(PropertyInsuranceClaimPool, ());
        let client = PropertyInsuranceClaimPoolClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let a1 = Address::generate(&env);
        let a2 = Address::generate(&env);
        let a3 = Address::generate(&env);
        let a4 = Address::generate(&env);
        let a5 = Address::generate(&env);

        let assessors = vec![
            &env,
            a1.clone(),
            a2.clone(),
            a3.clone(),
            a4.clone(),
            a5.clone(),
        ];

        // 1. Initialize
        client.initialize(&admin, &assessors);

        // 2. Deposit premiums from rental yield with interest
        let issuer = Address::generate(&env);
        client.deposit_premium(&issuer, &100_000, &5_000);
        assert_eq!(client.get_pool_balance(), 105_000);

        // 3. Submit damage claim for 60,000
        let claim_id = client.submit_claim(&issuer, &1, &60_000);
        let claim = client.get_claim(&claim_id).unwrap();
        assert_eq!(claim.status, ClaimStatus::Pending);

        // 4. Assessors vote (3-of-5 needed)
        // 1st vote
        let status1 = client.assess_claim(&a1, &claim_id, &true);
        assert_eq!(status1, ClaimStatus::Pending);

        // 2nd vote
        let status2 = client.assess_claim(&a2, &claim_id, &true);
        assert_eq!(status2, ClaimStatus::Pending);

        // 3rd vote reaches 3-of-5 threshold -> Approved
        let status3 = client.assess_claim(&a3, &claim_id, &true);
        assert_eq!(status3, ClaimStatus::Approved);

        // 5. Disburse pro-rata to token holders
        let h1 = Address::generate(&env);
        let h2 = Address::generate(&env);
        let h3 = Address::generate(&env);

        let holder_shares = vec![
            &env,
            TokenHolderShare {
                holder: h1.clone(),
                shares: 50, // 50%
            },
            TokenHolderShare {
                holder: h2.clone(),
                shares: 30, // 30%
            },
            TokenHolderShare {
                holder: h3.clone(),
                shares: 20, // 20%
            },
        ];

        let payouts = client.disburse_claim(&issuer, &claim_id, &holder_shares);
        assert_eq!(payouts.get(h1).unwrap(), 30_000); // 50% of 60,000
        assert_eq!(payouts.get(h2).unwrap(), 18_000); // 30% of 60,000
        assert_eq!(payouts.get(h3).unwrap(), 12_000); // 20% of 60,000

        // Remaining pool balance = 105,000 - 60,000 = 45,000
        assert_eq!(client.get_pool_balance(), 45_000);

        let final_claim = client.get_claim(&claim_id).unwrap();
        assert_eq!(final_claim.status, ClaimStatus::Disbursed);
    }
}
