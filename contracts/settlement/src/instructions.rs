//! T+0 delivery-versus-payment instructions, multilateral netting and a
//! settlement guarantee fund (issue #390).
//!
//! The older [`crate::batch_settler`] engine settles a *graph* of legs that the
//! caller submits in one transaction. This module adds the second, complementary
//! model: a buyer and a seller sign a `SettlementInstruction` in advance, and on
//! (or after) its `settlement_ledger` either party can call `settle`. The two
//! legs — `asset: seller -> buyer` and `payment_asset: buyer -> seller` — are
//! executed as two token transfers inside the same Soroban invocation, so a
//! failure of either leg reverts both (DVP).
//!
//! `net_settle` replaces a set of pending instructions by their *net* positions
//! per `(account, token)`. For every token the debtors and creditors are matched
//! greedily, producing at most `debtors + creditors - 1` transfers, which is the
//! minimum needed to clear a non-cyclic net position. No principal is ever
//! custodied by this contract except the optional guarantee fund.

use soroban_sdk::{
    contracttype, panic_with_error, symbol_short, token, Address, Env, IntoVal, Map, Symbol, Vec,
};

use crate::{batch_settler::AssetPolicy, DataKey, Error};

/// Upper bound on one `net_settle` call. Keeps the nested loops bounded and
/// leaves room for the batch engine's own limits in the same deployment.
pub const MAX_NET_INSTRUCTIONS: u32 = 32;

/// Lifecycle of a settlement instruction.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionStatus {
    /// Both parties signed, but the settlement ledger has not executed yet.
    Pending,
    /// Both legs transferred atomically.
    Settled,
    /// A pre-flight check failed and the guarantee fund compensated the
    /// non-defaulting party. No principal leg moved.
    Guaranteed,
}

/// A bilateral DVP instruction signed by both counterparties.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementInstruction {
    pub id: u32,
    pub buyer: Address,
    pub seller: Address,
    pub asset: Address,
    pub amount: i128,
    pub payment_asset: Address,
    pub payment_amount: i128,
    pub settlement_ledger: u32,
    pub created_at: u32,
    pub status: InstructionStatus,
}

/// Outcome of checking one leg before any funds move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LegCheck {
    sender_compliant: bool,
    recipient_compliant: bool,
    funded: bool,
}

impl LegCheck {
    fn ok(&self) -> bool {
        self.sender_compliant && self.recipient_compliant && self.funded
    }
}

fn require_initialized(env: &Env) {
    if tessera_common::is_paused(env) {
        panic_with_error!(env, Error::Paused);
    }
    if !env.storage().instance().has(&DataKey::Admin) {
        panic_with_error!(env, Error::NotInitialized);
    }
}

fn load(env: &Env, id: u32) -> SettlementInstruction {
    env.storage()
        .persistent()
        .get(&DataKey::Instruction(id))
        .unwrap_or_else(|| panic_with_error!(env, Error::InstructionNotFound))
}

fn store(env: &Env, instruction: &SettlementInstruction) {
    env.storage()
        .persistent()
        .set(&DataKey::Instruction(instruction.id), instruction);
}

fn add(env: &Env, a: i128, b: i128) -> i128 {
    a.checked_add(b)
        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow))
}

fn min(a: i128, b: i128) -> i128 {
    if a < b { a } else { b }
}

fn require_registered(env: &Env, token: &Address) {
    let registered: Option<AssetPolicy> = env
        .storage()
        .persistent()
        .get(&DataKey::Asset(token.clone()));
    if registered.is_none() {
        panic_with_error!(env, Error::AssetNotRegistered);
    }
}

/// True when `account` may send/receive `token`, using the same compliance
/// interface as the batch engine. Unregistered tokens fail closed.
fn is_allowed(env: &Env, token: &Address, account: &Address) -> bool {
    let policy: Option<AssetPolicy> = env
        .storage()
        .persistent()
        .get(&DataKey::Asset(token.clone()));
    match policy {
        Some(AssetPolicy::Unregulated) => true,
        Some(AssetPolicy::Regulated(compliance)) => {
            let args: Vec<soroban_sdk::Val> = (account.clone(),).into_val(env);
            env.invoke_contract(&compliance, &Symbol::new(env, "is_allowed"), args)
        }
        None => false,
    }
}

fn check_leg(
    env: &Env,
    from: &Address,
    to: &Address,
    token: &Address,
    amount: i128,
) -> LegCheck {
    LegCheck {
        sender_compliant: is_allowed(env, token, from),
        recipient_compliant: is_allowed(env, token, to),
        funded: token::TokenClient::new(env, token).balance(from) >= amount,
    }
}

/// Create a DVP instruction. Both parties sign once at creation time, recording
/// their consent to the exact price, assets and settlement ledger.
#[allow(deprecated)]
pub(crate) fn create(
    env: &Env,
    buyer: &Address,
    seller: &Address,
    asset: &Address,
    amount: i128,
    payment_asset: &Address,
    payment_amount: i128,
    settlement_ledger: u32,
) -> u32 {
    require_initialized(env);
    buyer.require_auth();
    seller.require_auth();

    if amount <= 0 || payment_amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    if buyer == seller {
        panic_with_error!(env, Error::SameAccount);
    }
    if asset == payment_asset {
        panic_with_error!(env, Error::SameAsset);
    }
    if settlement_ledger < env.ledger().sequence() {
        panic_with_error!(env, Error::InvalidSettlementLedger);
    }
    require_registered(env, asset);
    require_registered(env, payment_asset);

    let id: u32 = env
        .storage()
        .instance()
        .get(&DataKey::InstructionCount)
        .unwrap_or(0);
    let next = id.checked_add(1).unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
    env.storage().instance().set(&DataKey::InstructionCount, &next);

    let instruction = SettlementInstruction {
        id,
        buyer: buyer.clone(),
        seller: seller.clone(),
        asset: asset.clone(),
        amount,
        payment_asset: payment_asset.clone(),
        payment_amount,
        settlement_ledger,
        created_at: env.ledger().sequence(),
        status: InstructionStatus::Pending,
    };
    store(env, &instruction);
    env.events().publish(
        (symbol_short!("inst_new"), id),
        (buyer.clone(), seller.clone(), settlement_ledger),
    );
    id
}

/// Pay the non-defaulting party from the guarantee fund and mark the
/// instruction as covered. No principal leg moves, so DVP atomicity is kept.
#[allow(deprecated)]
fn cover_failure(
    env: &Env,
    instruction: &mut SettlementInstruction,
    asset_check: LegCheck,
    payment_check: LegCheck,
) {
    // The seller defaults when it cannot deliver the asset or cannot receive
    // the payment. Otherwise the buyer defaults. If both fail, the buyer is
    // treated as the non-defaulting party (the safer choice for investors).
    let seller_defaults = !asset_check.sender_compliant || !payment_check.recipient_compliant;

    let (beneficiary, token, owed) = if seller_defaults {
        (
            &instruction.buyer,
            &instruction.payment_asset,
            instruction.payment_amount,
        )
    } else {
        (&instruction.seller, &instruction.asset, instruction.amount)
    };

    let cap: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::GuaranteeCap(token.clone()))
        .unwrap_or(0);
    if cap <= 0 {
        panic_with_error!(env, Error::GuaranteeNotConfigured);
    }
    let available: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::Guarantee(token.clone()))
        .unwrap_or(0);
    let payout = min(min(owed, cap), available);
    if payout < owed {
        panic_with_error!(env, Error::InsufficientGuarantee);
    }

    env.storage()
        .persistent()
        .set(&DataKey::Guarantee(token.clone()), &(available - payout));
    token::TokenClient::new(env, token).transfer(
        &env.current_contract_address(),
        beneficiary,
        &payout,
    );

    instruction.status = InstructionStatus::Guaranteed;
    store(env, instruction);
    env.events().publish(
        (symbol_short!("inst_grt"), instruction.id),
        (beneficiary.clone(), payout),
    );
}

/// Execute a single instruction on/after its settlement ledger.
#[allow(deprecated)]
pub(crate) fn settle(env: &Env, id: u32) {
    require_initialized(env);
    let mut instruction = load(env, id);
    if instruction.status != InstructionStatus::Pending {
        panic_with_error!(env, Error::InstructionAlreadySettled);
    }
    if env.ledger().sequence() < instruction.settlement_ledger {
        panic_with_error!(env, Error::SettlementNotDue);
    }

    // The creation signature records consent to the terms; this signature
    // authorizes the actual token transfers in *this* invocation.
    instruction.buyer.require_auth();
    instruction.seller.require_auth();

    let asset_check = check_leg(
        env,
        &instruction.seller,
        &instruction.buyer,
        &instruction.asset,
        instruction.amount,
    );
    let payment_check = check_leg(
        env,
        &instruction.buyer,
        &instruction.seller,
        &instruction.payment_asset,
        instruction.payment_amount,
    );

    if !asset_check.ok() || !payment_check.ok() {
        cover_failure(env, &mut instruction, asset_check, payment_check);
        return;
    }

    token::TokenClient::new(env, &instruction.asset).transfer(
        &instruction.seller,
        &instruction.buyer,
        &instruction.amount,
    );
    token::TokenClient::new(env, &instruction.payment_asset).transfer(
        &instruction.buyer,
        &instruction.seller,
        &instruction.payment_amount,
    );

    instruction.status = InstructionStatus::Settled;
    store(env, &instruction);
    env.events().publish(
        (symbol_short!("inst_set"), id),
        (instruction.buyer.clone(), instruction.seller.clone()),
    );
}

fn push_unique_token(tokens: &mut Vec<Address>, token: &Address) {
    if !tokens.contains(token) {
        tokens.push_back(token.clone());
    }
}

fn push_unique_party(parties: &mut Vec<Address>, account: &Address) {
    if !parties.contains(account) {
        parties.push_back(account.clone());
    }
}

fn adjust(
    env: &Env,
    positions: &mut Map<(Address, Address), i128>,
    tokens: &mut Vec<Address>,
    parties: &mut Vec<Address>,
    account: &Address,
    token: &Address,
    delta: i128,
) {
    push_unique_token(tokens, token);
    push_unique_party(parties, account);
    let key = (token.clone(), account.clone());
    let current = positions.get(key.clone()).unwrap_or(0);
    positions.set(key, add(env, current, delta));
}

/// Clear pending instructions against each other. Returns the number of token
/// transfers executed, which is the minimum for the resulting net positions.
#[allow(deprecated)]
pub(crate) fn net_settle(env: &Env, ids: Vec<u32>) -> u32 {
    require_initialized(env);
    if ids.is_empty() || ids.len() > MAX_NET_INSTRUCTIONS {
        panic_with_error!(env, Error::InvalidBatchSize);
    }

    // (token, account) -> signed net position; positive means "is owed".
    let mut positions: Map<(Address, Address), i128> = Map::new(env);
    let mut tokens: Vec<Address> = Vec::new(env);
    let mut parties: Vec<Address> = Vec::new(env);

    for id in ids.iter() {
        let instruction = load(env, id);
        if instruction.status != InstructionStatus::Pending {
            panic_with_error!(env, Error::InstructionAlreadySettled);
        }
        if env.ledger().sequence() < instruction.settlement_ledger {
            panic_with_error!(env, Error::SettlementNotDue);
        }

        adjust(
            env,
            &mut positions,
            &mut tokens,
            &mut parties,
            &instruction.seller,
            &instruction.asset,
            -instruction.amount,
        );
        adjust(
            env,
            &mut positions,
            &mut tokens,
            &mut parties,
            &instruction.buyer,
            &instruction.asset,
            instruction.amount,
        );
        adjust(
            env,
            &mut positions,
            &mut tokens,
            &mut parties,
            &instruction.buyer,
            &instruction.payment_asset,
            -instruction.payment_amount,
        );
        adjust(
            env,
            &mut positions,
            &mut tokens,
            &mut parties,
            &instruction.seller,
            &instruction.payment_asset,
            instruction.payment_amount,
        );
    }

    let mut transfer_count: u32 = 0;
    for token in tokens.iter() {
        let mut debtors: Vec<(Address, i128)> = Vec::new(env);
        let mut creditors: Vec<(Address, i128)> = Vec::new(env);
        for party in parties.iter() {
            let position = positions
                .get((token.clone(), party.clone()))
                .unwrap_or(0);
            if position < 0 {
                debtors.push_back((party.clone(), -position));
            } else if position > 0 {
                creditors.push_back((party.clone(), position));
            }
        }
        if debtors.is_empty() || creditors.is_empty() {
            panic_with_error!(env, Error::ConservationViolation);
        }

        // Every check runs before the first transfer for this token.
        for (who, amount) in debtors.iter() {
            if !is_allowed(env, &token, &who) {
                panic_with_error!(env, Error::SenderNotCompliant);
            }
            if token::TokenClient::new(env, &token).balance(&who) < amount {
                panic_with_error!(env, Error::InsufficientBalance);
            }
        }
        for (who, _) in creditors.iter() {
            if !is_allowed(env, &token, &who) {
                panic_with_error!(env, Error::RecipientNotCompliant);
            }
        }
        for (who, _) in debtors.iter() {
            who.require_auth();
        }

        // Greedy debtor -> creditor matching is minimal for a bipartite net
        // position without cycles: each match clears at least one side.
        let mut i = 0u32;
        while i < debtors.len() {
            let (who, mut left) = debtors.get_unchecked(i);
            let mut j = 0u32;
            while j < creditors.len() && left > 0 {
                let (creditor, claim) = creditors.get_unchecked(j);
                let amount = min(left, claim);
                if amount > 0 {
                    token::TokenClient::new(env, &token).transfer(&who, &creditor, &amount);
                    transfer_count = transfer_count
                        .checked_add(1)
                        .unwrap_or_else(|| panic_with_error!(env, Error::Overflow));
                    left -= amount;
                    creditors.set(j, (creditor, claim - amount));
                }
                j += 1;
            }
            debtors.set(i, (who, left));
            i += 1;
        }
    }

    for id in ids.iter() {
        let mut instruction = load(env, id);
        instruction.status = InstructionStatus::Settled;
        store(env, &instruction);
    }
    env.storage()
        .instance()
        .set(&DataKey::LastNetTransferCount, &transfer_count);
    env.events()
        .publish((symbol_short!("net_set"), ids.len()), transfer_count);
    transfer_count
}

/// Configure the per-token cap for settlement-failure coverage. A cap of zero
/// disables the fund for that token.
#[allow(deprecated)]
pub(crate) fn configure_guarantee(env: &Env, admin: &Address, token: &Address, max_amount: i128) {
    let stored: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
    admin.require_auth();
    if admin != &stored {
        panic_with_error!(env, Error::Unauthorized);
    }
    if max_amount < 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    env.storage()
        .persistent()
        .set(&DataKey::GuaranteeCap(token.clone()), &max_amount);
    env.events()
        .publish((symbol_short!("gtee_cap"), token.clone()), max_amount);
}

/// Deposit collateral into the guarantee fund. The fund never takes custody of
/// principal legs — only of voluntarily deposited coverage collateral.
#[allow(deprecated)]
pub(crate) fn deposit_guarantee(env: &Env, depositor: &Address, token: &Address, amount: i128) {
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    depositor.require_auth();
    token::TokenClient::new(env, token).transfer(
        depositor,
        &env.current_contract_address(),
        &amount,
    );
    let balance: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::Guarantee(token.clone()))
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::Guarantee(token.clone()), &add(env, balance, amount));
    env.events()
        .publish((symbol_short!("gtee_dep"), token.clone()), (depositor.clone(), amount));
}

/// Return unused coverage collateral to the admin.
#[allow(deprecated)]
pub(crate) fn withdraw_guarantee(env: &Env, admin: &Address, token: &Address, amount: i128) {
    let stored: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
    admin.require_auth();
    if admin != &stored {
        panic_with_error!(env, Error::Unauthorized);
    }
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let balance: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::Guarantee(token.clone()))
        .unwrap_or(0);
    if balance < amount {
        panic_with_error!(env, Error::InsufficientGuarantee);
    }
    env.storage()
        .persistent()
        .set(&DataKey::Guarantee(token.clone()), &(balance - amount));
    token::TokenClient::new(env, token).transfer(
        &env.current_contract_address(),
        admin,
        &amount,
    );
    env.events()
        .publish((symbol_short!("gtee_wdr"), token.clone()), amount);
}
