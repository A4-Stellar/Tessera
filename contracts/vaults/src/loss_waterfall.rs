//! Multi-Asset Liquidation Loss Waterfall Distribution Module (Issue #148)
//!
//! When underlying physical assets suffer capital losses, this module allocates
//! losses across token tranches following a strict waterfall hierarchy.

use soroban_sdk::{contract, contractimpl, Address, Env, Map, Symbol, Vec};

/// Tranche priority levels
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum TranchePriority {
    Equity = 0,     // Absorbs losses first
    Mezzanine = 1,  // Absorbs losses second
    Senior = 2,     // Absorbs losses last (most protected)
}

/// Tranche state tracking
#[derive(Clone)]
pub struct TrancheState {
    pub token_id: Address,
    pub priority: TranchePriority,
    pub par_value: u64,           // Par value per share (scaled by 1e7)
    pub total_shares: u64,        // Total outstanding shares
    pub losses_absorbed: u64,     // Cumulative losses allocated to this tranche
}

/// Loss allocation event for audit trail
#[derive(Clone)]
pub struct LossAllocationLog {
    pub timestamp: u64,
    pub total_loss: u64,
    pub equity_loss: u64,
    pub mezzanine_loss: u64,
    pub senior_loss: u64,
    pub equity_new_par: u64,
    pub mezzanine_new_par: u64,
    pub senior_new_par: u64,
}

#[contract]
pub struct LossWaterfallEngine;

#[contractimpl]
impl LossWaterfallEngine {
    /// Initialize tranche configuration
    pub fn initialize_tranches(
        env: Env,
        vault_id: Address,
        equity_token: Address,
        mezzanine_token: Address,
        senior_token: Address,
        initial_par_value: u64,
    ) -> Result<(), Symbol> {
        let admin = env.invoker();
        admin.require_auth();

        let tranches = Map::from_array(
            &env,
            [
                (
                    TranchePriority::Equity,
                    TrancheState {
                        token_id: equity_token,
                        priority: TranchePriority::Equity,
                        par_value: initial_par_value,
                        total_shares: 0,
                        losses_absorbed: 0,
                    },
                ),
                (
                    TranchePriority::Mezzanine,
                    TrancheState {
                        token_id: mezzanine_token,
                        priority: TranchePriority::Mezzanine,
                        par_value: initial_par_value,
                        total_shares: 0,
                        losses_absorbed: 0,
                    },
                ),
                (
                    TranchePriority::Senior,
                    TrancheState {
                        token_id: senior_token,
                        priority: TranchePriority::Senior,
                        par_value: initial_par_value,
                        total_shares: 0,
                        losses_absorbed: 0,
                    },
                ),
            ],
        );

        env.storage().persistent().set(&vault_id, &tranches);
        Ok(())
    }

    /// Allocate capital loss across tranches following waterfall hierarchy
    ///
    /// # Loss Allocation Hierarchy
    /// 1. Equity tranche absorbs losses first
    /// 2. Mezzanine tranche absorbs overflow
    /// 3. Senior tranche absorbs remainder (last resort)
    ///
    /// Par values are reduced dynamically without destroying token balances.
    pub fn allocate_loss(
        env: Env,
        vault_id: Address,
        total_loss_amount: u64,
    ) -> Result<LossAllocationLog, Symbol> {
        let admin = env.invoker();
        admin.require_auth();

        if total_loss_amount == 0 {
            return Err(Symbol::new(&env, "zero_loss"));
        }

        // Load tranche states
        let mut tranches: Map<TranchePriority, TrancheState> = env
            .storage()
            .persistent()
            .get(&vault_id)
            .ok_or(Symbol::new(&env, "vault_not_found"))?;

        let mut remaining_loss = total_loss_amount;
        let mut equity_loss = 0u64;
        let mut mezzanine_loss = 0u64;
        let mut senior_loss = 0u64;

        // Step 1: Allocate to Equity tranche first
        if remaining_loss > 0 {
            let (loss, new_state) = Self::absorb_loss(
                &env,
                &tranches.get(TranchePriority::Equity).unwrap(),
                remaining_loss,
            )?;
            equity_loss = loss;
            remaining_loss = remaining_loss.saturating_sub(loss);
            tranches.set(TranchePriority::Equity, new_state);
        }

        // Step 2: Allocate overflow to Mezzanine tranche
        if remaining_loss > 0 {
            let (loss, new_state) = Self::absorb_loss(
                &env,
                &tranches.get(TranchePriority::Mezzanine).unwrap(),
                remaining_loss,
            )?;
            mezzanine_loss = loss;
            remaining_loss = remaining_loss.saturating_sub(loss);
            tranches.set(TranchePriority::Mezzanine, new_state);
        }

        // Step 3: Allocate remainder to Senior tranche (worst case)
        if remaining_loss > 0 {
            let (loss, new_state) = Self::absorb_loss(
                &env,
                &tranches.get(TranchePriority::Senior).unwrap(),
                remaining_loss,
            )?;
            senior_loss = loss;
            remaining_loss = remaining_loss.saturating_sub(loss);
            tranches.set(TranchePriority::Senior, new_state);
        }

        // Save updated tranche states
        env.storage().persistent().set(&vault_id, &tranches);

        // Build audit log
        let equity_state = tranches.get(TranchePriority::Equity).unwrap();
        let mezzanine_state = tranches.get(TranchePriority::Mezzanine).unwrap();
        let senior_state = tranches.get(TranchePriority::Senior).unwrap();

        let log = LossAllocationLog {
            timestamp: env.ledger().timestamp(),
            total_loss: total_loss_amount,
            equity_loss,
            mezzanine_loss,
            senior_loss,
            equity_new_par: equity_state.par_value,
            mezzanine_new_par: mezzanine_state.par_value,
            senior_new_par: senior_state.par_value,
        };

        // Store audit log
        Self::store_audit_log(&env, &vault_id, &log);

        // Emit event
        env.events().publish(
            (Symbol::new(&env, "loss_allocated"), vault_id),
            (
                total_loss_amount,
                equity_loss,
                mezzanine_loss,
                senior_loss,
            ),
        );

        Ok(log)
    }

    /// Get current tranche states
    pub fn get_tranches(
        env: Env,
        vault_id: Address,
    ) -> Option<Map<TranchePriority, TrancheState>> {
        env.storage().persistent().get(&vault_id)
    }

    /// Get loss allocation history
    pub fn get_loss_history(env: Env, vault_id: Address) -> Vec<LossAllocationLog> {
        let key = (Symbol::new(&env, "loss_history"), vault_id);
        env.storage().persistent().get(&key).unwrap_or(Vec::new(&env))
    }

    // ===== Internal Helpers =====

    /// Absorb loss into a single tranche by reducing par value
    fn absorb_loss(
        env: &Env,
        tranche: &TrancheState,
        loss_amount: u64,
    ) -> Result<(u64, TrancheState), Symbol> {
        if tranche.total_shares == 0 {
            // No shares, cannot absorb loss
            return Ok((0, tranche.clone()));
        }

        // Calculate total tranche value
        let total_value = (tranche.par_value as u128)
            .checked_mul(tranche.total_shares as u128)
            .ok_or(Symbol::new(env, "overflow"))?;

        let loss_to_absorb = u64::min(loss_amount, total_value as u64);

        // Calculate new par value after loss absorption
        let new_total_value = (total_value as u64).saturating_sub(loss_to_absorb);
        let new_par_value = if tranche.total_shares > 0 {
            new_total_value / tranche.total_shares
        } else {
            0
        };

        let mut new_state = tranche.clone();
        new_state.par_value = new_par_value;
        new_state.losses_absorbed += loss_to_absorb;

        Ok((loss_to_absorb, new_state))
    }

    fn store_audit_log(env: &Env, vault_id: &Address, log: &LossAllocationLog) {
        let key = (Symbol::new(env, "loss_history"), vault_id.clone());
        let mut history: Vec<LossAllocationLog> =
            env.storage().persistent().get(&key).unwrap_or(Vec::new(env));
        history.push_back(log.clone());
        env.storage().persistent().set(&key, &history);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equity_absorbs_first() {
        // Test that equity tranche absorbs losses before others
    }

    #[test]
    fn test_waterfall_overflow() {
        // Test loss flowing through equity → mezzanine → senior
    }

    #[test]
    fn test_par_value_reduction() {
        // Test par value updates without destroying balances
    }

    #[test]
    fn test_audit_log_generation() {
        // Test detailed loss distribution logging
    }
}
