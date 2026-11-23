//! # Automated Contract ABI Type Bindings (Issue #451 / #68)
//!
//! Generated automatically from Soroban contract interfaces by the ABI pipeline.
//! Provides strongly-typed cross-contract invocation helpers.

#![no_std]

use soroban_sdk::{symbol_short, Address, Env, IntoVal, String, Symbol, Val, Vec};

// ----------------------------------------------------------------------------
// AmmAbi: amm
// ----------------------------------------------------------------------------

/// Typed client wrapper for amm.
pub struct AmmAbi {
    pub contract_id: Address,
}

impl AmmAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Configure the pool. `initial_tick` must be a multiple of `tick_spacing`; the opening sqrt price is derived from it.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Set the position-NFT contract. Positions are minted there as NFTs on every `add_liquidity` call.
    pub fn set_position_nft_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_position_nft")
    }

    /// Add liquidity to `[tick_lower, tick_upper)` and mint a position.  Only the amounts actually required at the current price are pulled from `to`; any remainder stays in their wallet. Returns the position id and the liquidity minted.
    pub fn add_liquidity_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "add_liquidity")
    }

    /// Burn `liquidity` from a position and return both tokens to its owner. Fees accrued so far are paid in the same call.
    pub fn remove_liquidity_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "remove_liquidity")
    }

    /// Withdraw accrued fees without touching the liquidity.
    pub fn collect_fees_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "collect_fees")
    }

    /// Swap along the tick curve.  `amount_specified > 0` is the exact input amount; a negative value requests that much output. `sqrt_price_limit` bounds the price the trade may reach; `max_ticks_crossed` bounds how many initialised ticks the loop may cross before the call reverts with `TooManyTicksCrossed`.
    pub fn swap_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "swap")
    }

    /// Invokes `pool_state` on amm.
    pub fn pool_state_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "pool_state")
    }

    /// Invokes `get_position` on amm.
    pub fn get_position_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_position")
    }

    /// Invokes `tick_info` on amm.
    pub fn tick_info_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "tick_info")
    }

    /// The tick range a position with `liquidity` covers at the current price.
    pub fn position_amounts_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "position_amounts")
    }

}

// ----------------------------------------------------------------------------
// AssetTokenAbi: asset-token
// ----------------------------------------------------------------------------

/// Typed client wrapper for asset-token.
pub struct AssetTokenAbi {
    pub contract_id: Address,
}

impl AssetTokenAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on asset-token.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Invokes `transfer` on asset-token.
    pub fn transfer_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "transfer")
    }

    /// Invokes `permit` on asset-token.
    pub fn permit_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "permit")
    }

    /// Invokes `allowance` on asset-token.
    pub fn allowance_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "allowance")
    }

    /// Invokes `approve` on asset-token.
    pub fn approve_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "approve")
    }

    /// Invokes `transfer_from` on asset-token.
    pub fn transfer_from_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "transfer_from")
    }

    /// Invokes `mint` on asset-token.
    pub fn mint_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "mint")
    }

    /// Invokes `burn` on asset-token.
    pub fn burn_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "burn")
    }

    /// Invokes `balance` on asset-token.
    pub fn balance_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "balance")
    }

    /// Issue #90 - execute a forward (`num > den`) or reverse (`num < den`) split. Balances settle lazily; supply is the sum of individually rounded balances, scanning holders and pending history. Admin-authenticated.
    pub fn execute_share_split_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "execute_share_split")
    }

}

// ----------------------------------------------------------------------------
// BridgeAbi: bridge
// ----------------------------------------------------------------------------

/// Typed client wrapper for bridge.
pub struct BridgeAbi {
    pub contract_id: Address,
}

impl BridgeAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on bridge.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Register a validator public key that may sign inbound messages.
    pub fn register_validator_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "register_validator")
    }

    /// Invokes `revoke_validator` on bridge.
    pub fn revoke_validator_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "revoke_validator")
    }

    /// Invokes `is_validator` on bridge.
    pub fn is_validator_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "is_validator")
    }

    /// Record a lock-and-mint request and advance the caller's nonce.  The token lock/mint itself remains a stub, as it was; the request is now persisted so the nonce is meaningful and the request is auditable.
    pub fn lock_and_mint_request_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "lock_and_mint_request")
    }

    /// Accept an inbound burn-and-unlock message, once it carries [`MIN_SIGNATURES`] valid signatures from distinct registered validators.  Two defects were fixed here:  * **Signature verification was discarded.** The loop called   `ed25519_verify` and then incremented `valid_signatures`   unconditionally, so *any* two 64-byte blobs — including two copies of   all zeroes — were accepted as a valid two-of-N multisig. The result is   now checked, the key must be a registered validator, and the same   validator cannot sign twice to reach the threshold. * **`public_keys.get(i).unwrap()` trapped** whenever `signatures` was   longer than `public_keys`, which the fuzzer reaches with a single   mismatched-length call. Lengths are now compared up front and reported   as [`Error::LengthMismatch`].
    pub fn burn_and_unlock_request_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "burn_and_unlock_request")
    }

}

// ----------------------------------------------------------------------------
// CapTableAbi: cap-table
// ----------------------------------------------------------------------------

/// Typed client wrapper for cap-table.
pub struct CapTableAbi {
    pub contract_id: Address,
}

impl CapTableAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on cap-table.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Record a new cap-table snapshot for `asset_token`: `merkle_root` commits (via [`Self::verify_holder`]) to every holder's balance at the current ledger.
    pub fn submit_snapshot_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "submit_snapshot")
    }

    /// Invokes `get_snapshot` on cap-table.
    pub fn get_snapshot_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_snapshot")
    }

    /// Set the dividend contract allowed to record completed DRIP purchases.
    pub fn set_drip_registrar_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_drip_registrar")
    }

    /// Record asset tokens delivered to a holder by a DRIP swap. This is a cumulative audit ledger; token balances remain authoritative. The configured dividend contract is the only permitted caller.
    pub fn record_drip_acquisition_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "record_drip_acquisition")
    }

    /// Invokes `drip_acquisitions` on cap-table.
    pub fn drip_acquisitions_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "drip_acquisitions")
    }

    /// Verify that `holder` held `balance` at `snapshot_id`'s recorded Merkle root.  The leaf is `sha256(holder XDR bytes ++ balance big-endian bytes)`. The proof is then folded bottom-up to the root using SORTED-PAIR hashing at each level (`sha256(min(a, b) ++ max(a, b))`) rather than naive left/right concatenation, so a verifier does not need to also carry left/right-branch metadata, and the tree is not vulnerable to the classic ambiguous-ordering second-preimage trick. Returns `false` (never panics) for an unknown snapshot or a proof that doesn't fold to the stored root.
    pub fn verify_holder_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "verify_holder")
    }

    /// Record an acquisition lot for `holder` (admin only).
    pub fn record_acquisition_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "record_acquisition")
    }

    /// Set affiliate status for `holder` (admin only). Affiliate status is read on every call to [`Self::is_freely_tradeable`], so a change takes effect immediately; there is no cached result to invalidate.
    pub fn set_affiliate_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_affiliate")
    }

    /// Add `amount` traded volume to the current day's bucket (admin only).
    pub fn record_volume_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "record_volume")
    }

}

// ----------------------------------------------------------------------------
// CdpAbi: cdp
// ----------------------------------------------------------------------------

/// Typed client wrapper for cdp.
pub struct CdpAbi {
    pub contract_id: Address,
}

impl CdpAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the CDP vault.  - `admin`               — vault administrator (can update oracle prices). - `stablecoin_contract` — address of a mintable stablecoin contract used                           for loan disbursements and repayments.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Admin-only: update the USD-cent price for a collateral token.  In production this would be called by a decentralized oracle bridge (e.g. Band Protocol / Reflector) rather than manually.
    pub fn update_price_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "update_price")
    }

    /// Return the current oracle price (USD cents per base unit) for `token`.
    pub fn get_price_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_price")
    }

    /// Lock `collateral_amount` of `collateral_token` and open a new CDP position with zero initial debt.  Returns the new position ID.
    pub fn open_position_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "open_position")
    }

    /// Borrow `stablecoin_amount` against an existing position.  Reverts if the resulting collateralization ratio would fall below `MIN_COLLATERAL_RATIO_BPS` (150 %).
    pub fn borrow_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "borrow")
    }

    /// Repay `stablecoin_amount` of debt on a position.
    pub fn repay_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "repay")
    }

    /// Withdraw `amount` of collateral from a zero-debt (or sufficiently over-collateralized) position.
    pub fn withdraw_collateral_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "withdraw_collateral")
    }

    /// Liquidate an under-collateralized position.  Any caller may liquidate a position whose collateralization ratio has dropped below `MIN_COLLATERAL_RATIO_BPS`.  The liquidator must supply the outstanding stablecoin debt; in return they receive the full collateral at a **10 % discount** to its current market value.  The position is marked `closed` after liquidation.
    pub fn liquidate_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "liquidate")
    }

    /// Return the full state of a position.
    pub fn get_position_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_position")
    }

    /// Current collateralization ratio for a position, in basis points ×100. Returns `0` for positions with zero debt (infinite ratio).
    pub fn collateral_ratio_bps_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "collateral_ratio_bps")
    }

}

// ----------------------------------------------------------------------------
// ComplianceAbi: compliance
// ----------------------------------------------------------------------------

/// Typed client wrapper for compliance.
pub struct ComplianceAbi {
    pub contract_id: Address,
}

impl ComplianceAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on compliance.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Invokes `add_to_allowlist` on compliance.
    pub fn add_to_allowlist_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "add_to_allowlist")
    }

    /// Invokes `suspend` on compliance.
    pub fn suspend_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "suspend")
    }

    /// Invokes `remove` on compliance.
    pub fn remove_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "remove")
    }

    /// Invokes `is_allowed` on compliance.
    pub fn is_allowed_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "is_allowed")
    }

    /// Invokes `set_velocity_window` on compliance.
    pub fn set_velocity_window_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_velocity_window")
    }

    /// Invokes `set_velocity_cap` on compliance.
    pub fn set_velocity_cap_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_velocity_cap")
    }

    /// Invokes `set_default_velocity_cap` on compliance.
    pub fn set_default_velocity_cap_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_default_velocity_cap")
    }

    /// Invokes `get_velocity_config` on compliance.
    pub fn get_velocity_config_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_velocity_config")
    }

    /// Invokes `get_velocity_state` on compliance.
    pub fn get_velocity_state_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_velocity_state")
    }

}

// ----------------------------------------------------------------------------
// ConfidentialTokenAbi: confidential-token
// ----------------------------------------------------------------------------

/// Typed client wrapper for confidential-token.
pub struct ConfidentialTokenAbi {
    pub contract_id: Address,
}

impl ConfidentialTokenAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on confidential-token.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Public verifier parameters stored at initialization.
    pub fn public_parameters_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "public_parameters")
    }

    /// Derive the next `count` Bulletproofs generator pairs with `hash_to_g1`. Permissionless and deterministic, so anyone can finish the setup, split across as many transactions as the budget needs. Returns how many pairs exist.
    pub fn extend_generators_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "extend_generators")
    }

    /// Invokes `generators_ready` on confidential-token.
    pub fn generators_ready_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "generators_ready")
    }

    /// Issue `amount` publicly to `to`. The amount is committed with zero blinding (`amount*G`) into the recipient's pending balance; issuance stays visible so total supply is auditable.
    pub fn mint_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "mint")
    }

    /// Move a hidden amount from `from` to `to`, verifying in one call.  `proof` must be an aggregated range proof, bound to `transfer_context(from, to)`, over the two commitments `[amount_commitment, available(from) - amount_commitment]`. See the crate docs for when to use `begin_transfer` instead.
    pub fn confidential_transfer_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "confidential_transfer")
    }

    /// Start a transfer whose proof is verified across several transactions. Performs every check of `confidential_transfer` except the final MSM and returns the session id for `verify_step` / `finish`.
    pub fn begin_transfer_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "begin_transfer")
    }

    /// Redeem a public `amount` from the spendable balance, verifying in one call. `proof` is a single range proof, bound to `burn_context(from, amount)`, over `available(from) - amount*G`, proving the balance stays non-negative.
    pub fn burn_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "burn")
    }

    /// Multi-transaction variant of `burn`.
    pub fn begin_burn_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "begin_burn")
    }

    /// Evaluate up to `max_points` more MSM terms of a session. Permissionless: the result is fully determined by the stored session, so anyone may pay to advance it. Returns the number of terms still outstanding.
    pub fn verify_step_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "verify_step")
    }

}

// ----------------------------------------------------------------------------
// DebtTokenAbi: debt-token
// ----------------------------------------------------------------------------

/// Typed client wrapper for debt-token.
pub struct DebtTokenAbi {
    pub contract_id: Address,
}

impl DebtTokenAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Install the admin. Required before any ledger mutation, because `accrue_interest` and `trigger_default` are admin-gated and previously had no admin at all — anyone could inflate any tranche's debt.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Apply `amount` to the waterfall, paying `Senior` first, then `Mezzanine`, then `Equity`.  Fixed here:  * a non-positive `amount` is rejected. It used to be accepted, and a   negative amount *increased* every tranche's owed balance, because each   tranche was credited `owed - payment` with `payment` itself negative; * each tranche's payment is now capped at that tranche's own owed   balance. The `Equity` leg previously took the entire uncapped   remainder, so repaying more than was outstanding underflowed   `equity_owed - equity_payment` and trapped; * `i128::MIN` is rejected up front, so no leg can compute   `owed - i128::MIN`.  Overpayment beyond the total outstanding is accepted and simply retires the whole ledger; the excess is not credited anywhere.
    pub fn deposit_repayment_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "deposit_repayment")
    }

    /// Invokes `accrue_interest` on debt-token.
    pub fn accrue_interest_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "accrue_interest")
    }

    /// Issue #85: set kinked-curve parameters (first caller becomes rate admin).
    pub fn set_rate_params_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_rate_params")
    }

    /// Invokes `get_rate_params` on debt-token.
    pub fn get_rate_params_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_rate_params")
    }

    /// Annual borrow rate (WAD) for the given pool balances.
    pub fn current_borrow_rate_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "current_borrow_rate")
    }

    /// Pool utilization (WAD).
    pub fn pool_utilization_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "pool_utilization")
    }

    /// Accrue the WAD borrow index to the current ledger time; returns it.
    pub fn accrue_rate_index_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "accrue_rate_index")
    }

    /// Invokes `trigger_default` on debt-token.
    pub fn trigger_default_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "trigger_default")
    }

}

// ----------------------------------------------------------------------------
// DividendAbi: dividend
// ----------------------------------------------------------------------------

/// Typed client wrapper for dividend.
pub struct DividendAbi {
    pub contract_id: Address,
}

impl DividendAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on dividend.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Invokes `create_distribution` on dividend.
    pub fn create_distribution_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "create_distribution")
    }

    /// Invokes `claimable` on dividend.
    pub fn claimable_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "claimable")
    }

    /// Legacy live-balance claim, retained so existing holders and fuzz targets keep working alongside the new Merkle path.
    pub fn claim_legacy_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "claim_legacy")
    }

    /// Pull-based Merkle claim. The proof authorizes the committed payee, so no `holder.require_auth()` is needed. `leaf_index` selects both the claim bitmap slot and the left/right sibling ordering.
    pub fn claim_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "claim")
    }

    /// Commit (or rotate) the Merkle root for a distribution. The prior root remains claimable until `valid_until`, so pending proofs are not invalidated by a rotation.
    pub fn set_root_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_root")
    }

    /// Invokes `rotate_root` on dividend.
    pub fn rotate_root_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "rotate_root")
    }

    /// Invokes `get_root` on dividend.
    pub fn get_root_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_root")
    }

    /// Invokes `get_root_meta` on dividend.
    pub fn get_root_meta_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_root_meta")
    }

    /// Invokes `sync_root_cache` on dividend.
    pub fn sync_root_cache_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "sync_root_cache")
    }

}

// ----------------------------------------------------------------------------
// EmergencyLiquidationAbi: emergency-liquidation
// ----------------------------------------------------------------------------

/// Typed client wrapper for emergency-liquidation.
pub struct EmergencyLiquidationAbi {
    pub contract_id: Address,
}

impl EmergencyLiquidationAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

}

// ----------------------------------------------------------------------------
// EscrowAbi: escrow
// ----------------------------------------------------------------------------

/// Typed client wrapper for escrow.
pub struct EscrowAbi {
    pub contract_id: Address,
}

impl EscrowAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Create and fund an escrow. `inspection_window` and `dispute_window` are ledger counts. Returns the escrow id.
    pub fn create_escrow_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "create_escrow")
    }

    /// Record `signer`'s approval; releases to the seller when the threshold is met and the inspector has signed. After the inspection deadline no further approvals are accepted unless the inspector already approved.
    pub fn approve_release_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "approve_release")
    }

    /// Buyer or seller freezes release and starts the dispute window.
    pub fn raise_dispute_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "raise_dispute")
    }

    /// Inspector settles a dispute (before the dispute window lapses).
    pub fn resolve_dispute_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "resolve_dispute")
    }

    /// Return funds to the buyer. Permissionless, but only valid once a deadline has lapsed: the inspection deadline with no inspector approval, or the dispute deadline of an unresolved dispute.
    pub fn refund_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "refund")
    }

    /// Invokes `get_escrow` on escrow.
    pub fn get_escrow_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_escrow")
    }

}

// ----------------------------------------------------------------------------
// FlashLoanAbi: flash-loan
// ----------------------------------------------------------------------------

/// Typed client wrapper for flash-loan.
pub struct FlashLoanAbi {
    pub contract_id: Address,
}

impl FlashLoanAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the flash loan engine with its admin, compliance registry contract, and protocol fee treasury.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Set protocol insurance pool address for fee redistribution.
    pub fn set_insurance_pool_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_insurance_pool")
    }

    /// Set governance token contract address for discounted fee tier verification.
    pub fn set_governance_token_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_governance_token")
    }

    /// Whitelist or remove liquidator address for fee waiver.
    pub fn set_liquidator_whitelist_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_liquidator_whitelist")
    }

    /// Update the flash-loan premium fee in basis points (e.g., `9` = 0.09%).
    pub fn set_fee_bps_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_fee_bps")
    }

    /// Determine dynamic fee basis points for a borrower address based on fee tiers: - Whitelisted liquidator: 0 bps (0.00%) - Governance token holder: 5 bps (0.05%) - Standard: base fee (default 9 bps = 0.09%)
    pub fn get_borrower_fee_bps_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_borrower_fee_bps")
    }

    /// Execute an atomic, compliance-gated flash loan of `amount` units of `token_address` to `receiver_contract`.
    pub fn flash_loan_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "flash_loan")
    }

    /// Multi-hop atomic swap flash loan arbitrage path: 1. Borrower borrows Asset A. 2. Swaps A -> B on `amm_pool_ab`. 3. Swaps B -> C on `amm_pool_bc`. 4. Verifies net arbitrage value is non-negative using oracle spot prices. 5. Repays loan in Asset C (or equivalent value) plus tier fee.
    pub fn flash_loan_multi_hop_arbitrage_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "flash_loan_multi_hop_arbitrage")
    }

    /// Compute the flash loan fee for `amount` using a specific fee basis points rate.
    pub fn calculate_fee_with_bps_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "calculate_fee_with_bps")
    }

    /// Compute the flash loan fee for `amount` (default 0.09% = 9 bps).
    pub fn calculate_fee_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "calculate_fee")
    }

}

// ----------------------------------------------------------------------------
// FxHedgeAbi: fx-hedge
// ----------------------------------------------------------------------------

/// Typed client wrapper for fx-hedge.
pub struct FxHedgeAbi {
    pub contract_id: Address,
}

impl FxHedgeAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the FX hedging pool with an admin and an authorized oracle address.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Set or update the authorized oracle feed provider.
    pub fn set_oracle_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "set_oracle")
    }

    /// Ingest FX rate oracle feeds (e.g., EUR/USD, GBP/USD, JPY/USD). Rates scaled by 10,000 (e.g. 10850 = 1.0850).
    pub fn ingest_oracle_rate_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "ingest_oracle_rate")
    }

    /// Register or configure a currency forward hedge for an international real estate asset. Calculates the initial required forward hedge ratio based on projected annual dividend yield.
    pub fn setup_hedge_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "setup_hedge")
    }

    /// Calculate required FX forward hedge ratio (in basis points) based on projected annual dividend yields. Model: Higher dividend yields require scaled coverage to insulate cashflow against downside FX swings. - Yield <= 5% (500 bps) -> 80% coverage (8000 bps) - Yield between 5% and 10% -> 90% coverage (9000 bps) - Yield > 10% -> 100% coverage (10000 bps)
    pub fn calculate_hedge_ratio_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "calculate_hedge_ratio")
    }

    /// Execute automated rebalancing swaps when currency fluctuations exceed variance limits. Returns the rebalanced swap amount and new hedged position.
    pub fn execute_rebalance_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "execute_rebalance")
    }

    /// Read hedge position for a given pair.
    pub fn get_hedge_position_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_hedge_position")
    }

}

// ----------------------------------------------------------------------------
// GovernanceAbi: governance
// ----------------------------------------------------------------------------

/// Typed client wrapper for governance.
pub struct GovernanceAbi {
    pub contract_id: Address,
}

impl GovernanceAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

}

// ----------------------------------------------------------------------------
// IndexTokenAbi: index-token
// ----------------------------------------------------------------------------

/// Typed client wrapper for index-token.
pub struct IndexTokenAbi {
    pub contract_id: Address,
}

impl IndexTokenAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Configures the index. `fee_bps` is the annual management fee and `max_price_age` the oldest oracle price (seconds) accepted for NAV.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Underlying amounts (basket order) that must be deposited to mint `amount` index base units right now. Rounded up in the index's favour.
    pub fn quote_mint_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "quote_mint")
    }

    /// Underlying amounts (basket order) returned for burning `amount` index base units right now. Rounded down in the index's favour.
    pub fn quote_redeem_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "quote_redeem")
    }

    /// Mints `amount` index base units to `to` against a deposit of the exact basket ratio of every component. Returns the deposits made.
    pub fn mint_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "mint")
    }

    /// Burns `amount` index base units from `from` and returns the pro-rata share of every component reserve. Returns the withdrawals made.
    pub fn redeem_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "redeem")
    }

    /// Composite price of one whole index token, in oracle quote units with the oracle's decimals. Uses the live (post-fee) basket composition.
    pub fn index_price_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "index_price")
    }

    /// Net asset value of the whole index (all outstanding tokens), in oracle quote units.
    pub fn nav_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "nav")
    }

    /// Invokes `price_decimals` on index-token.
    pub fn price_decimals_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "price_decimals")
    }

    /// Fees owed to the maintainer, per component, including the portion streamed since the last accrual.
    pub fn accrued_fees_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "accrued_fees")
    }

    /// Accrues fees and transfers everything owed to the maintainer.
    pub fn claim_fees_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "claim_fees")
    }

}

// ----------------------------------------------------------------------------
// InsuranceAbi: insurance
// ----------------------------------------------------------------------------

/// Typed client wrapper for insurance.
pub struct InsuranceAbi {
    pub contract_id: Address,
}

impl InsuranceAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

}

// ----------------------------------------------------------------------------
// LeaseTokenAbi: lease-token
// ----------------------------------------------------------------------------

/// Typed client wrapper for lease-token.
pub struct LeaseTokenAbi {
    pub contract_id: Address,
}

impl LeaseTokenAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the lease tokenization & rental sweep vault.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Register a new tokenized commercial lease with senior debt priority interest obligations and fractional equity sweep routing.
    pub fn create_lease_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "create_lease")
    }

    /// Record additional periodic priority interest accrued on the senior debt tranche.
    pub fn accrue_senior_interest_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "accrue_senior_interest")
    }

    /// Receive a stablecoin rental payment from `tenant_address` and automatically sweep proceeds to the senior debt tranche (up to `senior_interest_due`) followed by the fractional equity payout contract.  Emits `RentSwept(lease_id, amount, tenant_address)`.
    pub fn pay_rent_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "pay_rent")
    }

    /// Deactivate a lease agreement.
    pub fn close_lease_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "close_lease")
    }

    /// Retrieve the full `LeaseAgreement` state for `lease_id`.
    pub fn get_lease_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_lease")
    }

    /// Invokes `mint` on lease-token.
    pub fn mint_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "mint")
    }

    /// Invokes `balance` on lease-token.
    pub fn balance_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "balance")
    }

    /// Invokes `transfer` on lease-token.
    pub fn transfer_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "transfer")
    }

}

// ----------------------------------------------------------------------------
// NftDeedAbi: nft-deed
// ----------------------------------------------------------------------------

/// Typed client wrapper for nft-deed.
pub struct NftDeedAbi {
    pub contract_id: Address,
}

impl NftDeedAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the NFT deed contract.  - `admin`              — contract administrator. - `fractional_token`   — the fractional asset-token contract address. - `vault`              — the vault contract where the NFT is locked. - `deed_uri`           — URI of the legal deed document (e.g. IPFS). - `registry_id`        — municipal land registry identifier. - `beneficiary`        — the address that can claim the NFT upon full                          redemption of all fractional tokens.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Returns the owner of the given token ID.  While the NFT is locked, the owner is the vault contract. After unlocking, the owner is the beneficiary who redeemed all tokens.
    pub fn owner_of_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "owner_of")
    }

    /// Transfer the master NFT to a new owner.  Can only be called when the NFT is unlocked (all fractional tokens have been redeemed). The caller must be the current owner.
    pub fn transfer_nft_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "transfer_nft")
    }

    /// Returns the metadata URI for the given token ID.  This URI points to the legal deed document stored in the municipal land registry (or an IPFS mirror of it).
    pub fn token_uri_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "token_uri")
    }

    /// Redeem fractional tokens to unlock the master NFT.  The caller specifies how many fractional tokens they are redeeming. Once the cumulative redeemed amount reaches 100% of the original total supply, the NFT is unlocked and can be transferred to the beneficiary.  This function: 1. Burns the specified amount of fractional tokens from the caller. 2. Tracks cumulative redemptions. 3. If 100% redeemed, unlocks the NFT and assigns ownership to the    beneficiary.
    pub fn redeem_and_unlock_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "redeem_and_unlock")
    }

    /// Check if the master NFT is unlocked (all fractional tokens redeemed).
    pub fn is_unlocked_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "is_unlocked")
    }

    /// Returns the bound fractional asset-token contract address.
    pub fn fractional_token_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "fractional_token")
    }

    /// Returns the vault contract address.
    pub fn vault_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "vault")
    }

    /// Returns the beneficiary address.
    pub fn beneficiary_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "beneficiary")
    }

    /// Returns the municipal land registry identifier.
    pub fn registry_id_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "registry_id")
    }

}

// ----------------------------------------------------------------------------
// OrderbookAbi: orderbook
// ----------------------------------------------------------------------------

/// Typed client wrapper for orderbook.
pub struct OrderbookAbi {
    pub contract_id: Address,
}

impl OrderbookAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

}

// ----------------------------------------------------------------------------
// RegistryAbi: registry
// ----------------------------------------------------------------------------

/// Typed client wrapper for registry.
pub struct RegistryAbi {
    pub contract_id: Address,
}

impl RegistryAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on registry.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Invokes `register_asset` on registry.
    pub fn register_asset_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "register_asset")
    }

    /// Invokes `get_asset` on registry.
    pub fn get_asset_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_asset")
    }

    /// Invokes `get_proof` on registry.
    pub fn get_proof_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_proof")
    }

    /// Invokes `verify_mpt_proof` on registry.
    pub fn verify_mpt_proof_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "verify_mpt_proof")
    }

    /// Invokes `get_mpt_root` on registry.
    pub fn get_mpt_root_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_mpt_root")
    }

    /// Invokes `get_assets_page` on registry.
    pub fn get_assets_page_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_assets_page")
    }

    /// Invokes `get_assets_by_issuer` on registry.
    pub fn get_assets_by_issuer_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_assets_by_issuer")
    }

    /// Invokes `get_assets_by_type` on registry.
    pub fn get_assets_by_type_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_assets_by_type")
    }

    /// Invokes `get_all_assets` on registry.
    pub fn get_all_assets_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_all_assets")
    }

}

// ----------------------------------------------------------------------------
// SettlementAbi: settlement
// ----------------------------------------------------------------------------

/// Typed client wrapper for settlement.
pub struct SettlementAbi {
    pub contract_id: Address,
}

impl SettlementAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Invokes `initialize` on settlement.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Register `token` as tradeable in batches. `compliance` is the compliance contract that gates the token; `None` registers it as an unregulated settlement asset (a cash leg such as USDC).
    pub fn register_asset_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "register_asset")
    }

    /// Invokes `deregister_asset` on settlement.
    pub fn deregister_asset_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "deregister_asset")
    }

    /// Invokes `get_asset_policy` on settlement.
    pub fn get_asset_policy_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_asset_policy")
    }

    /// Validate and atomically execute a multilateral cross-trade.  Returns the batch id. See [`batch_settler`] for the full rule set; every rule is checked before the first transfer, and any failure — including one raised by a token during execution — reverts every leg.
    pub fn execute_multilateral_settlement_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "execute_multilateral_settlement")
    }

    /// Run every pre-execution check on `trade_legs` without moving funds or requiring auth. Panics with the same error the real settlement would.
    pub fn validate_settlement_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "validate_settlement")
    }

    /// Invokes `batch_count` on settlement.
    pub fn batch_count_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "batch_count")
    }

    /// Invokes `pause` on settlement.
    pub fn pause_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "pause")
    }

    /// Invokes `unpause` on settlement.
    pub fn unpause_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "unpause")
    }

    /// Invokes `upgrade` on settlement.
    pub fn upgrade_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "upgrade")
    }

}

// ----------------------------------------------------------------------------
// StateChannelAbi: state-channel
// ----------------------------------------------------------------------------

/// Typed client wrapper for state-channel.
pub struct StateChannelAbi {
    pub contract_id: Address,
}

impl StateChannelAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Open a new payment channel. Both parties must call this to fund their stakes. channel_id is a unique identifier (e.g., hash of (party_a, party_b, nonce))
    pub fn open_channel_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "open_channel")
    }

    /// Cooperatively close the channel with mutually signed final state. This is the gas-optimal path and settles immediately without disputes.
    pub fn close_channel_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "close_channel")
    }

    /// Initiate unilateral dispute with the latest state you have. Starts a 24-hour challenge window for the counterparty to submit a newer state.
    pub fn initiate_dispute_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initiate_dispute")
    }

    /// Challenge a dispute by submitting a state with a higher sequence number. If successful, the challenger proves the dispute initiator submitted stale state (fraud).
    pub fn challenge_dispute_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "challenge_dispute")
    }

    /// Finalize the dispute after the challenge window expires. Settles balances according to the latest submitted state. If fraud was detected (challenge succeeded), slashes the dishonest party's escrow stake.
    pub fn finalize_dispute_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "finalize_dispute")
    }

    /// One-time setup of the adjudication parameters. Until this is called the contract uses `DISPUTE_WINDOW_LEDGERS` and `DEFAULT_FRAUD_BOUNTY_BPS`.
    pub fn init_dispute_config_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "init_dispute_config")
    }

    /// Admin-only update of the adjudication parameters. Disputes that are already open keep the window and bounty they were opened with.
    pub fn update_dispute_config_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "update_dispute_config")
    }

    /// Invokes `get_dispute_config` on state-channel.
    pub fn get_dispute_config_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_dispute_config")
    }

    /// Register the Ed25519 key `party` signs channel states with. Each key can be set once, before any dispute is opened, so a party cannot swap keys to repudiate states it already signed.
    pub fn register_signing_key_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "register_signing_key")
    }

    /// Invokes `get_signing_keys` on state-channel.
    pub fn get_signing_keys_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_signing_keys")
    }

}

// ----------------------------------------------------------------------------
// VaultsAbi: vaults
// ----------------------------------------------------------------------------

/// Typed client wrapper for vaults.
pub struct VaultsAbi {
    pub contract_id: Address,
}

impl VaultsAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

}

// ----------------------------------------------------------------------------
// VestingAbi: vesting
// ----------------------------------------------------------------------------

/// Typed client wrapper for vesting.
pub struct VestingAbi {
    pub contract_id: Address,
}

impl VestingAbi {
    pub fn new(contract_id: Address) -> Self {
        Self { contract_id }
    }

    /// Initialize the vesting contract with its funding token and refund treasury.
    pub fn initialize_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "initialize")
    }

    /// Create and fund a schedule. `start_time` and durations are ledger timestamp seconds. At the cliff, the beneficiary may claim the linear amount accrued since `start_time`.
    pub fn create_vesting_schedule_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "create_vesting_schedule")
    }

    /// Claim all currently vested but previously unclaimed tokens for one schedule. #388: guarded against reentrancy via a TemporaryStorage mutex flag.
    pub fn claim_vested_tokens_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "claim_vested_tokens")
    }

    /// #388: Slash unvested tokens by `bps` basis points (100 = 1%). Slashed tokens are transferred to the governance treasury, not destroyed. `reason_hash` is a governance proposal ID for auditability.
    pub fn slash_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "slash")
    }

    /// Revoke an active schedule and return its unvested balance to the treasury. Any vested amount remains claimable by the beneficiary.
    pub fn revoke_vesting_schedule_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "revoke_vesting_schedule")
    }

    /// Invokes `get_vesting_schedule` on vesting.
    pub fn get_vesting_schedule_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "get_vesting_schedule")
    }

    /// Invokes `version` on vesting.
    pub fn version_symbol(env: &Env) -> Symbol {
        Symbol::new(env, "version")
    }

}

