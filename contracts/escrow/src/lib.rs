//! Issue #86 - multi-party threshold escrow (buyer / seller / inspector) with
//! ledger-window timeouts.
//!
//! Flow: the buyer funds the escrow in `create_escrow`. Each party calls
//! `approve_release`; once `threshold` distinct approvals are recorded AND the
//! inspector has approved, the funds are paid to the seller. If the inspector
//! has not approved by `inspection_deadline` (current ledger + a configurable
//! window), anyone may call `refund` and the funds return to the buyer.
//! Buyer or seller may `raise_dispute`, which freezes release; the inspector
//! settles it with `resolve_dispute`, and if nobody does within
//! `dispute_window` ledgers `refund` returns the funds to the buyer.
#![no_std]

use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    vec, Address, Env, IntoVal, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotFound = 1,
    InvalidAmount = 2,
    InvalidThreshold = 3,
    InvalidWindow = 4,
    NotParty = 5,
    AlreadyApproved = 6,
    NotOpen = 7,
    Disputed = 8,
    DeadlinePassed = 9,
    NotRefundable = 10,
    NotDisputed = 11,
    NotInspector = 12,
    DuplicateParties = 13,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Status {
    Open,
    Disputed,
    Released,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Escrow {
    pub buyer: Address,
    pub seller: Address,
    pub inspector: Address,
    pub token: Address,
    pub amount: i128,
    /// Distinct approvals required to release to the seller (2 or 3).
    pub threshold: u32,
    pub approvals: Vec<Address>,
    /// Ledger sequence after which a missing inspector approval means refund.
    pub inspection_deadline: u32,
    /// Ledgers granted to resolve a dispute before it becomes refundable.
    pub dispute_window: u32,
    /// Set when a dispute is raised; refundable once the ledger passes it.
    pub dispute_deadline: u32,
    pub status: Status,
}

#[contracttype]
enum DataKey {
    Count,
    Escrow(u32),
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Create and fund an escrow. `inspection_window` and `dispute_window`
    /// are ledger counts. Returns the escrow id.
    #[allow(clippy::too_many_arguments)]
    pub fn create_escrow(
        env: Env,
        buyer: Address,
        seller: Address,
        inspector: Address,
        token: Address,
        amount: i128,
        threshold: u32,
        inspection_window: u32,
        dispute_window: u32,
    ) -> u32 {
        buyer.require_auth();
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if !(2..=3).contains(&threshold) {
            panic_with_error!(env, Error::InvalidThreshold);
        }
        if inspection_window == 0 || dispute_window == 0 {
            panic_with_error!(env, Error::InvalidWindow);
        }
        if buyer == seller || buyer == inspector || seller == inspector {
            panic_with_error!(env, Error::DuplicateParties);
        }
        token::Client::new(&env, &token).transfer(&buyer, &env.current_contract_address(), &amount);

        let id: u32 = env.storage().instance().get(&DataKey::Count).unwrap_or(0);
        env.storage().instance().set(&DataKey::Count, &(id + 1));
        let deadline = env
            .ledger()
            .sequence()
            .checked_add(inspection_window)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidWindow));
        let e = Escrow {
            buyer: buyer.clone(),
            seller,
            inspector,
            token,
            amount,
            threshold,
            approvals: Vec::new(&env),
            inspection_deadline: deadline,
            dispute_window,
            dispute_deadline: 0,
            status: Status::Open,
        };
        env.storage().persistent().set(&DataKey::Escrow(id), &e);
        env.events()
            .publish((symbol_short!("created"), id), (buyer, amount, deadline));
        id
    }

    /// Record `signer`'s approval; releases to the seller when the threshold
    /// is met and the inspector has signed. After the inspection deadline no
    /// further approvals are accepted unless the inspector already approved.
    pub fn approve_release(env: Env, id: u32, signer: Address) {
        signer.require_auth();
        let mut e = Self::load(&env, id);
        Self::require_open(&env, &e);
        if signer != e.buyer && signer != e.seller && signer != e.inspector {
            panic_with_error!(env, Error::NotParty);
        }
        if e.approvals.contains(&signer) {
            panic_with_error!(env, Error::AlreadyApproved);
        }
        if env.ledger().sequence() > e.inspection_deadline && !e.approvals.contains(&e.inspector) {
            panic_with_error!(env, Error::DeadlinePassed);
        }
        e.approvals.push_back(signer.clone());
        env.events().publish((symbol_short!("approved"), id), signer);

        if e.approvals.len() >= e.threshold && e.approvals.contains(&e.inspector) {
            e.status = Status::Released;
            Self::pay(&env, &e, &e.seller);
            env.events().publish((symbol_short!("released"), id), e.amount);
        }
        env.storage().persistent().set(&DataKey::Escrow(id), &e);
    }

    /// Buyer or seller freezes release and starts the dispute window.
    pub fn raise_dispute(env: Env, id: u32, party: Address) {
        party.require_auth();
        let mut e = Self::load(&env, id);
        Self::require_open(&env, &e);
        if party != e.buyer && party != e.seller {
            panic_with_error!(env, Error::NotParty);
        }
        e.status = Status::Disputed;
        e.dispute_deadline = env
            .ledger()
            .sequence()
            .checked_add(e.dispute_window)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidWindow));
        env.storage().persistent().set(&DataKey::Escrow(id), &e);
        env.events().publish((symbol_short!("disputed"), id), party);
    }

    /// Inspector settles a dispute (before the dispute window lapses).
    pub fn resolve_dispute(env: Env, id: u32, inspector: Address, release_to_seller: bool) {
        inspector.require_auth();
        let mut e = Self::load(&env, id);
        if e.status != Status::Disputed {
            panic_with_error!(env, Error::NotDisputed);
        }
        if inspector != e.inspector {
            panic_with_error!(env, Error::NotInspector);
        }
        if env.ledger().sequence() > e.dispute_deadline {
            panic_with_error!(env, Error::DeadlinePassed);
        }
        let to = if release_to_seller {
            e.seller.clone()
        } else {
            e.buyer.clone()
        };
        e.status = if release_to_seller {
            Status::Released
        } else {
            Status::Refunded
        };
        Self::pay(&env, &e, &to);
        env.storage().persistent().set(&DataKey::Escrow(id), &e);
        env.events()
            .publish((symbol_short!("resolved"), id), release_to_seller);
    }

    /// Return funds to the buyer. Permissionless, but only valid once a
    /// deadline has lapsed: the inspection deadline with no inspector
    /// approval, or the dispute deadline of an unresolved dispute.
    pub fn refund(env: Env, id: u32) {
        let mut e = Self::load(&env, id);
        let now = env.ledger().sequence();
        let ok = match e.status {
            Status::Open => now > e.inspection_deadline && !e.approvals.contains(&e.inspector),
            Status::Disputed => now > e.dispute_deadline,
            _ => false,
        };
        if !ok {
            panic_with_error!(env, Error::NotRefundable);
        }
        e.status = Status::Refunded;
        Self::pay(&env, &e, &e.buyer);
        env.storage().persistent().set(&DataKey::Escrow(id), &e);
        env.events().publish((symbol_short!("refunded"), id), e.amount);
    }

    pub fn get_escrow(env: Env, id: u32) -> Escrow {
        Self::load(&env, id)
    }

    fn load(env: &Env, id: u32) -> Escrow {
        env.storage()
            .persistent()
            .get(&DataKey::Escrow(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::NotFound))
    }

    fn require_open(env: &Env, e: &Escrow) {
        match e.status {
            Status::Open => {}
            Status::Disputed => panic_with_error!(env, Error::Disputed),
            _ => panic_with_error!(env, Error::NotOpen),
        }
    }

    /// Pay out from the contract. The contract pre-authorizes the token
    /// `transfer` sub-invocation via `authorize_as_current_contract`.
    fn pay(env: &Env, e: &Escrow, to: &Address) {
        let me = env.current_contract_address();
        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: e.token.clone(),
                    fn_name: symbol_short!("transfer"),
                    args: (me.clone(), to.clone(), e.amount).into_val(env),
                },
                sub_invocations: vec![env],
            }),
        ]);
        token::Client::new(env, &e.token).transfer(&me, to, &e.amount);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};

    struct Ctx<'a> {
        env: Env,
        c: EscrowContractClient<'a>,
        tok: token::Client<'a>,
        buyer: Address,
        seller: Address,
        insp: Address,
        id: u32,
    }

    fn setup<'a>(threshold: u32) -> Ctx<'a> {
        let env = Env::default();
        env.mock_all_auths();
        let issuer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(issuer);
        let tok_addr = sac.address();
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);
        let insp = Address::generate(&env);
        token::StellarAssetClient::new(&env, &tok_addr).mint(&buyer, &1_000);
        let c = EscrowContractClient::new(&env, &env.register(EscrowContract, ()));
        let tok = token::Client::new(&env, &tok_addr);
        let id = c.create_escrow(&buyer, &seller, &insp, &tok_addr, &600, &threshold, &100, &50);
        Ctx {
            env,
            c,
            tok,
            buyer,
            seller,
            insp,
            id,
        }
    }

    fn advance(env: &Env, ledgers: u32) {
        let s = env.ledger().sequence();
        env.ledger().set_sequence_number(s + ledgers);
    }

    #[test]
    fn funds_locked_then_released_on_threshold() {
        let x = setup(3);
        assert_eq!(x.tok.balance(&x.c.address), 600);
        assert_eq!(x.tok.balance(&x.buyer), 400);
        x.c.approve_release(&x.id, &x.buyer);
        x.c.approve_release(&x.id, &x.seller);
        assert_eq!(x.c.get_escrow(&x.id).status, Status::Open);
        x.c.approve_release(&x.id, &x.insp);
        assert_eq!(x.c.get_escrow(&x.id).status, Status::Released);
        assert_eq!(x.tok.balance(&x.seller), 600);
        assert_eq!(x.tok.balance(&x.c.address), 0);
    }

    #[test]
    fn two_of_three_still_needs_inspector() {
        let x = setup(2);
        x.c.approve_release(&x.id, &x.buyer);
        x.c.approve_release(&x.id, &x.seller);
        assert_eq!(x.c.get_escrow(&x.id).status, Status::Open);
        x.c.approve_release(&x.id, &x.insp);
        assert_eq!(x.tok.balance(&x.seller), 600);
    }

    #[test]
    fn timeout_refunds_buyer_without_inspector() {
        let x = setup(2);
        x.c.approve_release(&x.id, &x.buyer);
        x.c.approve_release(&x.id, &x.seller);
        assert!(x.c.try_refund(&x.id).is_err());
        advance(&x.env, 101);
        assert!(x.c.try_approve_release(&x.id, &x.insp).is_err());
        x.c.refund(&x.id);
        assert_eq!(x.tok.balance(&x.buyer), 1_000);
        assert_eq!(x.c.get_escrow(&x.id).status, Status::Refunded);
        assert!(x.c.try_refund(&x.id).is_err());
    }

    #[test]
    fn dispute_blocks_release_and_inspector_resolves() {
        let x = setup(2);
        x.c.raise_dispute(&x.id, &x.seller);
        assert!(x.c.try_approve_release(&x.id, &x.buyer).is_err());
        x.c.resolve_dispute(&x.id, &x.insp, &true);
        assert_eq!(x.tok.balance(&x.seller), 600);
    }

    #[test]
    fn unresolved_dispute_times_out_to_buyer() {
        let x = setup(2);
        x.c.raise_dispute(&x.id, &x.buyer);
        assert!(x.c.try_refund(&x.id).is_err());
        advance(&x.env, 51);
        assert!(x.c.try_resolve_dispute(&x.id, &x.insp, &true).is_err());
        x.c.refund(&x.id);
        assert_eq!(x.tok.balance(&x.buyer), 1_000);
    }

    #[test]
    fn rejects_outsiders_duplicates_and_bad_input() {
        let x = setup(2);
        let stranger = Address::generate(&x.env);
        assert!(x.c.try_approve_release(&x.id, &stranger).is_err());
        assert!(x.c.try_raise_dispute(&x.id, &x.insp).is_err());
        x.c.approve_release(&x.id, &x.buyer);
        assert!(x.c.try_approve_release(&x.id, &x.buyer).is_err());
        let t = x.tok.address.clone();
        assert!(x.c.try_create_escrow(&x.buyer, &x.seller, &x.insp, &t, &0, &2, &10, &10).is_err());
        assert!(x.c.try_create_escrow(&x.buyer, &x.seller, &x.insp, &t, &1, &1, &10, &10).is_err());
        assert!(x.c.try_create_escrow(&x.buyer, &x.buyer, &x.insp, &t, &1, &2, &10, &10).is_err());
        assert!(x.c.try_get_escrow(&99).is_err());
    }
}
