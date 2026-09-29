#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{vec, Map};

#[contracttype]
#[derive(Clone)]
enum OracleKey {
    Prices,
}

/// SEP-40 style oracle test double with 7 decimals.
#[contract]
pub struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn set_price(env: Env, asset: Address, price: i128, timestamp: u64) {
        let mut prices: Map<Address, PriceData> = env
            .storage()
            .instance()
            .get(&OracleKey::Prices)
            .unwrap_or(Map::new(&env));
        prices.set(asset, PriceData { price, timestamp });
        env.storage().instance().set(&OracleKey::Prices, &prices);
    }

    pub fn lastprice(env: Env, asset: Asset) -> Option<PriceData> {
        let prices: Map<Address, PriceData> = env.storage().instance().get(&OracleKey::Prices)?;
        match asset {
            Asset::Stellar(address) => prices.get(address),
            Asset::Other(_) => None,
        }
    }

    pub fn decimals(_env: Env) -> u32 {
        7
    }
}

struct Setup<'a> {
    env: Env,
    index: IndexTokenClient<'a>,
    oracle: MockOracleClient<'a>,
    tokens: std::vec::Vec<Address>,
    maintainer: Address,
    user: Address,
}

const START: u64 = 1_700_000_000;

/// Two-asset basket: 2.0 of a 7-decimal T-bill token and 0.5 of a
/// 6-decimal real-estate token per whole index token.
fn setup<'a>(fee_bps: u32) -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let issuer = Address::generate(&env);
    let admin = Address::generate(&env);
    let maintainer = Address::generate(&env);
    let user = Address::generate(&env);

    let t_bill = env.register_stellar_asset_contract_v2(issuer.clone()).address();
    let property = env.register_stellar_asset_contract_v2(issuer).address();
    StellarAssetClient::new(&env, &t_bill).mint(&user, &1_000_000_000_000);
    StellarAssetClient::new(&env, &property).mint(&user, &1_000_000_000_000);

    let oracle_id = env.register(MockOracle, ());
    let oracle = MockOracleClient::new(&env, &oracle_id);
    // $1.00 and $250.00 with 7 oracle decimals.
    oracle.set_price(&t_bill, &10_000_000, &START);
    oracle.set_price(&property, &2_500_000_000, &START);

    let index_id = env.register(IndexToken, ());
    let index = IndexTokenClient::new(&env, &index_id);
    index.initialize(
        &admin,
        &maintainer,
        &oracle_id,
        &vec![
            &env,
            Component { token: t_bill.clone(), units: 20_000_000, decimals: 7 },
            Component { token: property.clone(), units: 500_000, decimals: 6 },
        ],
        &fee_bps,
        &3_600,
        &String::from_str(&env, "RWA Top 10"),
        &String::from_str(&env, "RWA-TOP10"),
    );

    Setup {
        env,
        index,
        oracle,
        tokens: std::vec![t_bill, property],
        maintainer,
        user,
    }
}

#[test]
fn first_mint_deposits_configured_basket_ratio() {
    let s = setup(0);
    let deposits = s.index.mint(&s.user, &(3 * INDEX_UNIT));
    assert_eq!(deposits, vec![&s.env, 60_000_000, 1_500_000]);
    assert_eq!(s.index.balance(&s.user), 3 * INDEX_UNIT);
    assert_eq!(s.index.total_supply(), 3 * INDEX_UNIT);
    assert_eq!(
        TokenClient::new(&s.env, &s.tokens[0]).balance(&s.index.address),
        60_000_000
    );
    assert_eq!(s.index.reserves(), vec![&s.env, 60_000_000, 1_500_000]);
}

#[test]
fn later_mints_follow_live_reserve_ratio_and_round_up() {
    let s = setup(0);
    s.index.mint(&s.user, &INDEX_UNIT);
    // 1 base unit must still cost at least 1 base unit of every component.
    assert_eq!(s.index.quote_mint(&1), vec![&s.env, 2, 1]);
    let deposits = s.index.mint(&s.user, &(INDEX_UNIT / 2));
    assert_eq!(deposits, vec![&s.env, 10_000_000, 250_000]);
}

#[test]
fn redeem_returns_pro_rata_basket() {
    let s = setup(0);
    s.index.mint(&s.user, &(4 * INDEX_UNIT));
    let before = TokenClient::new(&s.env, &s.tokens[1]).balance(&s.user);
    let out = s.index.redeem(&s.user, &INDEX_UNIT);
    assert_eq!(out, vec![&s.env, 20_000_000, 500_000]);
    assert_eq!(
        TokenClient::new(&s.env, &s.tokens[1]).balance(&s.user),
        before + 500_000
    );
    assert_eq!(s.index.total_supply(), 3 * INDEX_UNIT);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn redeem_more_than_balance_fails() {
    let s = setup(0);
    s.index.mint(&s.user, &INDEX_UNIT);
    s.index.redeem(&s.user, &(2 * INDEX_UNIT));
}

#[test]
fn composite_price_uses_oracle_and_basket() {
    let s = setup(0);
    // Before any mint: 2 × $1 + 0.5 × $250 = $127.
    assert_eq!(s.index.index_price(), 1_270_000_000);
    s.index.mint(&s.user, &(2 * INDEX_UNIT));
    assert_eq!(s.index.nav(), 2_540_000_000);
    s.oracle.set_price(&s.tokens[1], &3_000_000_000, &START);
    assert_eq!(s.index.index_price(), 1_520_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn stale_oracle_price_is_rejected() {
    let s = setup(0);
    s.env.ledger().set_timestamp(START + 3_601);
    s.index.index_price();
}

#[test]
fn management_fee_streams_to_maintainer() {
    // 2% per year.
    let s = setup(200);
    s.index.mint(&s.user, &(100 * INDEX_UNIT));
    s.env.ledger().set_timestamp(START + SECONDS_PER_YEAR as u64);
    s.oracle.set_price(&s.tokens[0], &10_000_000, &(START + SECONDS_PER_YEAR as u64));
    s.oracle.set_price(&s.tokens[1], &2_500_000_000, &(START + SECONDS_PER_YEAR as u64));

    // A full year at 2% moves 2% of every reserve to the maintainer.
    assert_eq!(s.index.accrued_fees(), vec![&s.env, 40_000_000, 1_000_000]);
    assert_eq!(s.index.reserves(), vec![&s.env, 1_960_000_000, 49_000_000]);
    assert_eq!(s.index.index_price(), 1_244_600_000);

    let claimed = s.index.claim_fees();
    assert_eq!(claimed, vec![&s.env, 40_000_000, 1_000_000]);
    assert_eq!(
        TokenClient::new(&s.env, &s.tokens[0]).balance(&s.maintainer),
        40_000_000
    );
    assert_eq!(s.index.accrued_fees(), vec![&s.env, 0, 0]);

    // Holders redeem the net-of-fee basket.
    let out = s.index.redeem(&s.user, &(100 * INDEX_UNIT));
    assert_eq!(out, vec![&s.env, 1_960_000_000, 49_000_000]);
}

#[test]
fn fee_accrues_per_second_and_new_minters_pay_diluted_ratio() {
    let s = setup(500);
    s.index.mint(&s.user, &(10 * INDEX_UNIT));
    s.env.ledger().set_timestamp(START + 86_400);
    let fees = s.index.accrued_fees();
    // 5%/yr for one day on 200_000_000 = 27_397 (floored).
    assert_eq!(fees.get_unchecked(0), 27_397);
    let quote = s.index.quote_mint(&INDEX_UNIT);
    assert_eq!(quote.get_unchecked(0), (200_000_000 - 27_397 + 9) / 10);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn fee_above_cap_rejected() {
    let s = setup(0);
    s.index.set_fee_bps(&(MAX_FEE_BPS + 1));
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn duplicate_components_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let index = IndexTokenClient::new(&env, &env.register(IndexToken, ()));
    index.initialize(
        &admin,
        &admin,
        &Address::generate(&env),
        &vec![
            &env,
            Component { token: token.clone(), units: 1, decimals: 7 },
            Component { token, units: 1, decimals: 7 },
        ],
        &0,
        &60,
        &String::from_str(&env, "Dup"),
        &String::from_str(&env, "DUP"),
    );
}

#[test]
fn sep41_transfer_and_allowance() {
    let s = setup(0);
    s.index.mint(&s.user, &INDEX_UNIT);
    let other = Address::generate(&s.env);
    let spender = Address::generate(&s.env);
    s.index.transfer(&s.user, &other, &100);
    assert_eq!(s.index.balance(&other), 100);
    s.index.approve(&s.user, &spender, &500, &1_000);
    s.index.transfer_from(&spender, &s.user, &other, &200);
    assert_eq!(s.index.allowance(&s.user, &spender), 300);
    assert_eq!(s.index.balance(&other), 300);
    assert_eq!(s.index.decimals(), 7);
    assert_eq!(s.index.symbol(), String::from_str(&s.env, "RWA-TOP10"));
}
