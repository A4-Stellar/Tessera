//! Scenario tests for the Dutch-auction liquidation engine and the oracle
//! circuit-breaker (issue #360).

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    Address, Env,
};

use super::*;

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TokenKey::Balance(to);
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(balance + amount));
    }

    pub fn balance(env: Env, who: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(who))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_key = TokenKey::Balance(from);
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        let to_key = TokenKey::Balance(to);
        let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
    }
}

#[derive(Clone)]
#[contracttype]
enum EmergencyKey {
    Absorbed,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct Absorbed {
    operator: Address,
    vault_id: u64,
    collateral_amount: i128,
    debt_amount: i128,
}

#[contract]
struct MockEmergency;

#[contractimpl]
impl MockEmergency {
    pub fn absorb_vault(
        env: Env,
        operator: Address,
        vault_id: u64,
        _collateral_token: Address,
        collateral_amount: i128,
        debt_amount: i128,
    ) {
        env.storage().instance().set(
            &EmergencyKey::Absorbed,
            &Absorbed {
                operator,
                vault_id,
                collateral_amount,
                debt_amount,
            },
        );
    }

    pub fn absorbed(env: Env) -> Option<Absorbed> {
        env.storage().instance().get(&EmergencyKey::Absorbed)
    }
}

fn err(e: AuctionError) -> soroban_sdk::Error {
    e.into()
}

struct Setup<'a> {
    env: Env,
    client: LiquidationAuctionContractClient<'a>,
    token: MockTokenClient<'a>,
    stable: MockTokenClient<'a>,
    emergency: MockEmergencyClient<'a>,
    admin: Address,
    treasury: Address,
    owner: Address,
    bidder: Address,
    token_id: Address,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let owner = Address::generate(&env);
    let bidder = Address::generate(&env);

    let token_id = env.register(MockToken, ());
    let token = MockTokenClient::new(&env, &token_id);
    let stable_id = env.register(MockToken, ());
    let stable = MockTokenClient::new(&env, &stable_id);
    let emergency_id = env.register(MockEmergency, ());
    let emergency = MockEmergencyClient::new(&env, &emergency_id);

    let id = env.register(LiquidationAuctionContract, ());
    let client = LiquidationAuctionContractClient::new(&env, &id);
    client.initialize(
        &admin,
        &stable_id,
        &treasury,
        &emergency_id,
        &15_000,
        &100,
        &10,
    );

    token.mint(&owner, &1_000);
    stable.mint(&bidder, &100_000);
    Setup {
        env,
        client,
        token,
        stable,
        emergency,
        admin,
        treasury,
        owner,
        bidder,
        token_id,
    }
}

#[test]
fn auction_price_decays_from_110_percent_of_debt() {
    let s = setup();
    s.client.update_price(&s.admin, &s.token_id, &100);
    let vault_id = s.client.create_vault(&s.owner, &s.token_id, &100, &1_000);

    // Price drops from 100 to 50: value is 5 000 against 1 000 debt, still
    // healthy. A drop to 10 makes the ratio 100 % and triggers the auction.
    s.client.update_price(&s.admin, &s.token_id, &10);
    let start = s.client.start_auction(&vault_id);
    assert_eq!(start, 1_100);

    // A quarter of the way through the window the price is 75 % of start.
    s.env.ledger().set_sequence_number(125);
    assert_eq!(s.client.auction_price(&vault_id), 825);

    // Halfway through it is half.
    s.env.ledger().set_sequence_number(150);
    assert_eq!(s.client.auction_price(&vault_id), 550);
}

#[test]
fn bid_pays_current_price_and_receives_collateral() {
    let s = setup();
    // Open at a healthy 200 % ratio (threshold is 150 %), then drop the price.
    s.client.update_price(&s.admin, &s.token_id, &20);
    let vault_id = s.client.create_vault(&s.owner, &s.token_id, &100, &1_000);
    s.client.update_price(&s.admin, &s.token_id, &5);
    s.client.start_auction(&vault_id);

    s.env.ledger().set_sequence_number(150);
    // Keep the feed fresh: bids freeze on a stale oracle like every other
    // vault operation.
    s.client.update_price(&s.admin, &s.token_id, &5);
    let paid = s.client.bid(&s.bidder, &vault_id);
    assert_eq!(paid, 550);
    assert_eq!(s.stable.balance(&s.bidder), 100_000 - 550);
    assert_eq!(s.stable.balance(&s.treasury), 550);
    assert_eq!(s.token.balance(&s.bidder), 100);
    assert_eq!(s.token.balance(&s.owner), 900);

    let vault = s.client.get_vault(&vault_id);
    assert!(vault.liquidated);
    // A second bid cannot settle the same collateral twice.
    assert_eq!(
        s.client.try_bid(&s.bidder, &vault_id),
        Err(Ok(err(AuctionError::VaultClosed)))
    );
}

#[test]
fn expired_auction_calls_emergency_backstop() {
    let s = setup();
    // Open at a healthy 200 % ratio (threshold is 150 %), then drop the price.
    s.client.update_price(&s.admin, &s.token_id, &20);
    let vault_id = s.client.create_vault(&s.owner, &s.token_id, &100, &1_000);
    s.client.update_price(&s.admin, &s.token_id, &5);
    s.client.start_auction(&vault_id);

    // Not yet expired.
    s.env.ledger().set_sequence_number(150);
    assert_eq!(
        s.client.try_trigger_backstop(&vault_id),
        Err(Ok(err(AuctionError::AuctionNotExpired)))
    );

    s.env.ledger().set_sequence_number(201);
    s.client.trigger_backstop(&vault_id);
    let absorbed = s.emergency.absorbed().unwrap();
    assert_eq!(absorbed.vault_id, vault_id);
    assert_eq!(absorbed.collateral_amount, 100);
    assert_eq!(absorbed.debt_amount, 1_000);
    assert!(s.client.get_vault(&vault_id).liquidated);
}

/// Fork scenario: the oracle goes down, every vault operation freezes, and a
/// fresh update restores normal operation.
#[test]
fn oracle_downtime_freezes_then_recovers() {
    let s = setup();
    s.client.update_price(&s.admin, &s.token_id, &100);
    let vault_id = s.client.create_vault(&s.owner, &s.token_id, &100, &1_000);
    assert!(!s.client.is_frozen(&s.token_id));

    // No update for longer than max_oracle_staleness_ledgers (10).
    s.env.ledger().set_sequence_number(111);
    assert_eq!(
        s.client.try_price(&s.token_id),
        Err(Ok(err(AuctionError::OracleStale)))
    );
    assert!(s.client.is_frozen(&s.token_id));

    // Frozen: new vaults and auctions must not proceed on the stale price.
    assert_eq!(
        s.client
            .try_create_vault(&s.owner, &s.token_id, &10, &0),
        Err(Ok(err(AuctionError::OracleStale)))
    );
    assert_eq!(
        s.client.try_start_auction(&vault_id),
        Err(Ok(err(AuctionError::OracleStale)))
    );

    // Recovery: the next oracle update clears the freeze.
    s.client.update_price(&s.admin, &s.token_id, &90);
    assert!(!s.client.is_frozen(&s.token_id));
    assert_eq!(s.client.price(&s.token_id), 90);
    assert_eq!(s.client.snapshot(&vault_id).collateral_ratio_bps, 90_000);
}
