//! Multi-Token Debt Refinancing and Bond Conversion Engine (Issue #147)
//!
//! Supports debt refinancing mechanisms allowing issuers to convert high-interest
//! debt tokens into lower-interest new tranches or equity tokens upon maturity.

use soroban_sdk::{contract, contractimpl, Address, Env, Symbol, Vec};

/// Refinancing state
#[derive(Clone)]
pub struct RefinancingProposal {
    pub old_token_id: Address,
    pub new_token_id: Address,
    pub conversion_ratio: u64,    // e.g., 100 = 1:1, 120 = 1.2:1
    pub expiration: u64,           // Ledger timestamp
    pub proposer: Address,
    pub active: bool,
}

/// Tranche types for equity conversion
#[derive(Clone, Copy, PartialEq)]
pub enum TrancheType {
    Debt,
    Equity,
}

#[contract]
pub struct RefinancingEngine;

#[contractimpl]
impl RefinancingEngine {
    /// Initiate a refinancing proposal
    ///
    /// # Arguments
    /// * `old_token_id` - Address of the existing high-interest debt token
    /// * `new_token_id` - Address of the new lower-interest debt or equity token
    /// * `conversion_ratio` - Ratio scaled by 100 (100 = 1:1, 120 = 1.2:1)
    /// * `expiration` - Ledger timestamp when refinancing expires
    pub fn initiate_refinancing(
        env: Env,
        old_token_id: Address,
        new_token_id: Address,
        conversion_ratio: u64,
        expiration: u64,
    ) -> Result<u64, Symbol> {
        let proposer = env.invoker();
        proposer.require_auth();

        // Validation
        if conversion_ratio == 0 {
            return Err(Symbol::new(&env, "invalid_ratio"));
        }
        if expiration <= env.ledger().timestamp() {
            return Err(Symbol::new(&env, "expiration_past"));
        }

        // Store proposal
        let proposal_id = Self::next_proposal_id(&env);
        let proposal = RefinancingProposal {
            old_token_id: old_token_id.clone(),
            new_token_id: new_token_id.clone(),
            conversion_ratio,
            expiration,
            proposer: proposer.clone(),
            active: true,
        };

        env.storage().persistent().set(&proposal_id, &proposal);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "refinancing_initiated"), proposal_id),
            (old_token_id, new_token_id, conversion_ratio),
        );

        Ok(proposal_id)
    }

    /// Execute refinancing conversion for a holder
    ///
    /// Burns old debt token shares and mints equivalent value in new debt/equity shares
    /// atomically with compliance allowlist enforcement.
    pub fn execute_conversion(
        env: Env,
        proposal_id: u64,
        holder: Address,
        old_shares: u64,
    ) -> Result<u64, Symbol> {
        holder.require_auth();

        // Load proposal
        let proposal: RefinancingProposal = env
            .storage()
            .persistent()
            .get(&proposal_id)
            .ok_or(Symbol::new(&env, "proposal_not_found"))?;

        if !proposal.active {
            return Err(Symbol::new(&env, "proposal_inactive"));
        }
        if env.ledger().timestamp() > proposal.expiration {
            return Err(Symbol::new(&env, "proposal_expired"));
        }

        // Calculate new shares based on conversion ratio
        let new_shares = (old_shares as u128)
            .checked_mul(proposal.conversion_ratio as u128)
            .ok_or(Symbol::new(&env, "overflow"))?
            .checked_div(100)
            .ok_or(Symbol::new(&env, "division_error"))?
            as u64;

        // ATOMIC OPERATION: Burn old + Mint new
        // 1. Verify compliance allowlist on both tokens
        Self::verify_compliance(&env, &proposal.old_token_id, &holder)?;
        Self::verify_compliance(&env, &proposal.new_token_id, &holder)?;

        // 2. Burn old debt tokens
        Self::burn_token(&env, &proposal.old_token_id, &holder, old_shares)?;

        // 3. Mint new debt or equity tokens
        Self::mint_token(&env, &proposal.new_token_id, &holder, new_shares)?;

        // Emit conversion event
        env.events().publish(
            (Symbol::new(&env, "conversion_executed"), proposal_id),
            (holder.clone(), old_shares, new_shares),
        );

        Ok(new_shares)
    }

    /// Cancel refinancing proposal
    pub fn cancel_refinancing(env: Env, proposal_id: u64) -> Result<(), Symbol> {
        let proposer = env.invoker();
        proposer.require_auth();

        let mut proposal: RefinancingProposal = env
            .storage()
            .persistent()
            .get(&proposal_id)
            .ok_or(Symbol::new(&env, "proposal_not_found"))?;

        if proposal.proposer != proposer {
            return Err(Symbol::new(&env, "unauthorized"));
        }

        proposal.active = false;
        env.storage().persistent().set(&proposal_id, &proposal);

        env.events().publish(
            (Symbol::new(&env, "refinancing_cancelled"), proposal_id),
            proposer,
        );

        Ok(())
    }

    /// Query refinancing proposal
    pub fn get_proposal(env: Env, proposal_id: u64) -> Option<RefinancingProposal> {
        env.storage().persistent().get(&proposal_id)
    }

    // ===== Internal Helpers =====

    fn next_proposal_id(env: &Env) -> u64 {
        let key = Symbol::new(env, "next_id");
        let current: u64 = env.storage().persistent().get(&key).unwrap_or(1);
        env.storage().persistent().set(&key, &(current + 1));
        current
    }

    fn verify_compliance(env: &Env, token_id: &Address, holder: &Address) -> Result<(), Symbol> {
        // Call compliance contract to verify holder is allowlisted
        let compliance_addr = Self::get_compliance_contract(env, token_id)?;
        let is_allowed: bool = env.invoke_contract(
            &compliance_addr,
            &Symbol::new(env, "is_allowlisted"),
            Vec::from_array(env, [holder.into_val(env)]),
        );

        if !is_allowed {
            return Err(Symbol::new(env, "compliance_denied"));
        }
        Ok(())
    }

    fn burn_token(env: &Env, token_id: &Address, holder: &Address, amount: u64) -> Result<(), Symbol> {
        env.invoke_contract(
            token_id,
            &Symbol::new(env, "burn"),
            Vec::from_array(env, [holder.into_val(env), amount.into_val(env)]),
        );
        Ok(())
    }

    fn mint_token(env: &Env, token_id: &Address, holder: &Address, amount: u64) -> Result<(), Symbol> {
        env.invoke_contract(
            token_id,
            &Symbol::new(env, "mint"),
            Vec::from_array(env, [holder.into_val(env), amount.into_val(env)]),
        );
        Ok(())
    }

    fn get_compliance_contract(env: &Env, token_id: &Address) -> Result<Address, Symbol> {
        // Query token for its compliance contract address
        let compliance: Address = env.invoke_contract(
            token_id,
            &Symbol::new(env, "compliance_contract"),
            Vec::new(env),
        );
        Ok(compliance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initiate_refinancing() {
        // Test refinancing proposal creation
    }

    #[test]
    fn test_execute_conversion_atomic() {
        // Test atomic burn/mint operation
    }

    #[test]
    fn test_compliance_enforcement() {
        // Test that non-allowlisted holders are rejected
    }
}
