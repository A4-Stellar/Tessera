extern crate std;

use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};

use crate::{
    DataKey, DynamicFeeConfig, Error, OrderStatus, RestrictedOrderbookDex,
    RestrictedOrderbookDexClient, Side, DEFAULT_RULE_144_HOLDING_PERIOD, MAX_FILLS_PER_ORDER,
    MAX_ORDERS_PER_LEVEL, MAX_PRICE_LEVELS, NON_REPORTING_ISSUER_HOLDING_PERIOD, PRICE_PRECISION,
    TTL_EXTEND_TO,
};

const DAY: u64 = 86_400;

struct TestFixture<'a> {
    env: Env,
    admin: Address,
    dex: RestrictedOrderbookDexClient<'a>,
    compliance: ComplianceContractClient<'a>,
    base: TokenClient<'a>,
    quote: TokenClient<'a>,
    treasury: Address,
    seller1: Address,
    seller2: Address,
    buyer1: Address,
    buyer2: Address,
}

impl TestFixture<'_> {
    fn now(&self) -> u64 {
        self.env.ledger().timestamp()
    }

    /// Admin attests that `seller` acquired its shares long enough ago to sell.
    fn attest_mature(&self, seller: &Address) {
        let acquired = self.now() - DEFAULT_RULE_144_HOLDING_PERIOD - DAY;
        self.dex
            .record_seller_acquisition(&self.admin, seller, &acquired);
    }

    /// Admin accredits `buyer` for the next ~115 days and KYC-approves it.
    fn accredit(&self, buyer: &Address) {
        self.dex
            .set_accredited_investor(&self.admin, buyer, &true, &(self.now() + 10_000_000));
        self.kyc(buyer);
    }

    fn kyc(&self, investor: &Address) {
        let us = String::from_str(&self.env, "US");
        self.compliance
            .add_to_allowlist(&self.admin, investor, &us, &0);
    }
}

fn setup<'a>() -> TestFixture<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(365 * DAY);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let seller1 = Address::generate(&env);
    let seller2 = Address::generate(&env);
    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    let compliance_id = env.register(ComplianceContract, ());
    let compliance = ComplianceContractClient::new(&env, &compliance_id);
    compliance.initialize(&admin);

    // Base token: Rule 144 restricted shares. Quote token: payment currency (USDC).
    let base_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let quote_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    StellarAssetClient::new(&env, &base_token).mint(&seller1, &100_000);
    StellarAssetClient::new(&env, &base_token).mint(&seller2, &100_000);
    StellarAssetClient::new(&env, &quote_token).mint(&buyer1, &1_000_000_000);
    StellarAssetClient::new(&env, &quote_token).mint(&buyer2, &1_000_000_000);

    let dex_id = env.register(RestrictedOrderbookDex, ());
    let dex = RestrictedOrderbookDexClient::new(&env, &dex_id);
    let fee_config = DynamicFeeConfig {
        maker_fee_bps: 10,                 // 0.10%
        taker_fee_bps: 25,                 // 0.25%
        volume_discount_threshold: 50_000, // 50,000 quote units
        volume_discount_bps: 10,           // 0.10% off the taker fee
        fee_recipient: treasury.clone(),
    };
    dex.initialize(
        &admin,
        &base_token,
        &quote_token,
        &Some(compliance_id),
        &fee_config,
        &0, // default six-month holding period
    );

    TestFixture {
        base: TokenClient::new(&env, &base_token),
        quote: TokenClient::new(&env, &quote_token),
        env,
        admin,
        dex,
        compliance,
        treasury,
        seller1,
        seller2,
        buyer1,
        buyer2,
    }
}

// ---------------------------------------------------------------------------
// Acceptance criterion 1: price-time priority in persistent storage
// ---------------------------------------------------------------------------

#[test]
fn test_price_time_priority_matching() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.accredit(&f.buyer2);
    f.attest_mature(&f.seller1);

    let price_100 = 100 * PRICE_PRECISION;
    let price_105 = 105 * PRICE_PRECISION;
    let b1 = f.dex.place_order(&f.buyer1, &Side::Bid, &price_100, &100);
    let b2 = f.dex.place_order(&f.buyer2, &Side::Bid, &price_105, &50);

    let depth = f.dex.get_orderbook_depth(&Side::Bid, &10);
    assert_eq!(depth.len(), 2);
    assert_eq!(depth.get(0).unwrap(), (price_105, 50));
    assert_eq!(depth.get(1).unwrap(), (price_100, 100));
    assert_eq!(f.dex.get_best_price(&Side::Bid), Some(price_105));

    // Ask for 70 @ 95 crosses both levels: 50 fill at 105 first, then 20 at 100.
    let s1 = f
        .dex
        .place_order(&f.seller1, &Side::Ask, &(95 * PRICE_PRECISION), &70);

    let b2 = f.dex.get_order(&b2).unwrap();
    assert_eq!((b2.status, b2.filled_amount), (OrderStatus::Filled, 50));
    let b1 = f.dex.get_order(&b1).unwrap();
    assert_eq!(
        (b1.status, b1.filled_amount),
        (OrderStatus::PartiallyFilled, 20)
    );
    let s1 = f.dex.get_order(&s1).unwrap();
    assert_eq!((s1.status, s1.filled_amount), (OrderStatus::Filled, 70));

    assert_eq!(f.base.balance(&f.buyer1), 20);
    assert_eq!(f.base.balance(&f.buyer2), 50);
}

#[test]
fn test_time_priority_at_same_price() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.accredit(&f.buyer2);
    f.attest_mature(&f.seller1);

    let price = 50 * PRICE_PRECISION;
    let b1 = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &100);
    f.env.ledger().set_timestamp(f.now() + 10);
    let b2 = f.dex.place_order(&f.buyer2, &Side::Bid, &price, &100);

    f.dex.place_order(&f.seller1, &Side::Ask, &price, &120);

    let b1 = f.dex.get_order(&b1).unwrap();
    assert_eq!((b1.status, b1.filled_amount), (OrderStatus::Filled, 100));
    let b2 = f.dex.get_order(&b2).unwrap();
    assert_eq!(
        (b2.status, b2.filled_amount),
        (OrderStatus::PartiallyFilled, 20)
    );
}

#[test]
fn test_book_entries_have_ttl_extended() {
    let f = setup();
    f.accredit(&f.buyer1);
    let id = f
        .dex
        .place_order(&f.buyer1, &Side::Bid, &(10 * PRICE_PRECISION), &10);

    f.env.as_contract(&f.dex.address, || {
        let storage = f.env.storage().persistent();
        assert_eq!(storage.get_ttl(&DataKey::Order(id)), TTL_EXTEND_TO);
        assert_eq!(
            storage.get_ttl(&DataKey::PriceLadder(Side::Bid)),
            TTL_EXTEND_TO
        );
    });
}

// ---------------------------------------------------------------------------
// Acceptance criterion 2: seller holding period interception
// ---------------------------------------------------------------------------

#[test]
fn test_seller_without_attested_acquisition_is_rejected() {
    let f = setup();
    let res = f
        .dex
        .try_place_order(&f.seller1, &Side::Ask, &(100 * PRICE_PRECISION), &50);
    assert_eq!(res, Err(Ok(Error::SellerHoldingPeriodNotMet.into())));
}

#[test]
fn test_seller_holding_period_not_met_is_rejected() {
    let f = setup();
    f.accredit(&f.buyer1);
    let price = 100 * PRICE_PRECISION;
    f.dex.place_order(&f.buyer1, &Side::Bid, &price, &100);

    f.dex
        .record_seller_acquisition(&f.admin, &f.seller1, &(f.now() - 30 * DAY));
    let res = f.dex.try_place_order(&f.seller1, &Side::Ask, &price, &50);
    assert_eq!(res, Err(Ok(Error::SellerHoldingPeriodNotMet.into())));
}

#[test]
fn test_holding_period_boundary() {
    let f = setup();
    f.accredit(&f.buyer1);
    let price = 100 * PRICE_PRECISION;

    // One second short of six months: rejected.
    let acquired = f.now() - DEFAULT_RULE_144_HOLDING_PERIOD + 1;
    f.dex
        .record_seller_acquisition(&f.admin, &f.seller1, &acquired);
    let res = f.dex.try_place_order(&f.seller1, &Side::Ask, &price, &50);
    assert_eq!(res, Err(Ok(Error::SellerHoldingPeriodNotMet.into())));

    // One second later the holding period is met and the trade executes.
    f.env.ledger().set_timestamp(f.now() + 1);
    let ask = f.dex.place_order(&f.seller1, &Side::Ask, &price, &50);
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &50);
    assert_eq!(f.dex.get_order(&ask).unwrap().status, OrderStatus::Filled);
    assert_eq!(f.dex.get_order(&bid).unwrap().status, OrderStatus::Filled);
}

#[test]
fn test_seller_cannot_self_attest_acquisition() {
    let f = setup();
    // The seller (not the admin) tries to backdate its own acquisition.
    let res = f
        .dex
        .try_record_seller_acquisition(&f.seller1, &f.seller1, &1);
    assert_eq!(res, Err(Ok(Error::Unauthorized.into())));

    let (acquired, _, met) = f.dex.get_seller_holding_info(&f.seller1);
    assert_eq!((acquired, met), (0, false));
}

#[test]
fn test_future_acquisition_timestamp_rejected() {
    let f = setup();
    let res = f
        .dex
        .try_record_seller_acquisition(&f.admin, &f.seller1, &(f.now() + 1));
    assert_eq!(res, Err(Ok(Error::InvalidAcquisitionTimestamp.into())));
}

#[test]
fn test_resting_ask_cancelled_when_acquisition_corrected() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let price = 100 * PRICE_PRECISION;
    let ask = f.dex.place_order(&f.seller1, &Side::Ask, &price, &50);
    let seller_base = f.base.balance(&f.seller1);

    // Transfer agent corrects the acquisition date: the shares are not yet seasoned.
    f.dex
        .record_seller_acquisition(&f.admin, &f.seller1, &f.now());

    // The match is intercepted: the ask is cancelled and refunded, the bid rests.
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &50);
    assert_eq!(
        f.dex.get_order(&ask).unwrap().status,
        OrderStatus::Cancelled
    );
    assert_eq!(f.base.balance(&f.seller1), seller_base + 50);
    assert_eq!(f.base.balance(&f.buyer1), 0);
    assert_eq!(f.dex.get_order(&bid).unwrap().status, OrderStatus::Open);
    assert_eq!(f.dex.get_orderbook_depth(&Side::Ask, &10).len(), 0);
}

#[test]
fn test_holding_period_cannot_go_below_six_months() {
    let f = setup();
    let res = f
        .dex
        .try_set_holding_period_seconds(&f.admin, &(DEFAULT_RULE_144_HOLDING_PERIOD - 1));
    assert_eq!(res, Err(Ok(Error::InvalidHoldingPeriod.into())));

    // Non-reporting issuers need one year.
    f.dex
        .set_holding_period_seconds(&f.admin, &NON_REPORTING_ISSUER_HOLDING_PERIOD);
    let (_, holding_period, _) = f.dex.get_seller_holding_info(&f.seller1);
    assert_eq!(holding_period, NON_REPORTING_ISSUER_HOLDING_PERIOD);

    // A seller past six months but short of a year can no longer sell.
    f.attest_mature(&f.seller1);
    let res = f
        .dex
        .try_place_order(&f.seller1, &Side::Ask, &(100 * PRICE_PRECISION), &10);
    assert_eq!(res, Err(Ok(Error::SellerHoldingPeriodNotMet.into())));
}

// ---------------------------------------------------------------------------
// Acceptance criterion 2: buyer accreditation interception
// ---------------------------------------------------------------------------

#[test]
fn test_buyer_not_accredited_is_rejected() {
    let f = setup();
    f.kyc(&f.buyer1); // KYC alone is not accreditation
    let res = f
        .dex
        .try_place_order(&f.buyer1, &Side::Bid, &(100 * PRICE_PRECISION), &50);
    assert_eq!(res, Err(Ok(Error::BuyerNotAccredited.into())));
}

#[test]
fn test_accredited_buyer_must_also_pass_compliance_gate() {
    let f = setup();
    f.dex
        .set_accredited_investor(&f.admin, &f.buyer1, &true, &(f.now() + 10_000_000));
    assert!(!f.dex.is_accredited(&f.buyer1));
    let res = f
        .dex
        .try_place_order(&f.buyer1, &Side::Bid, &(100 * PRICE_PRECISION), &50);
    assert_eq!(res, Err(Ok(Error::BuyerNotAccredited.into())));

    f.kyc(&f.buyer1);
    assert!(f.dex.is_accredited(&f.buyer1));
}

#[test]
fn test_expired_accreditation_is_rejected() {
    let f = setup();
    f.kyc(&f.buyer1);
    f.dex
        .set_accredited_investor(&f.admin, &f.buyer1, &true, &(f.now() + 100));
    f.env.ledger().set_timestamp(f.now() + 101);

    // Still on the KYC allowlist, but accreditation has lapsed.
    assert!(!f.dex.is_accredited(&f.buyer1));
    let res = f
        .dex
        .try_place_order(&f.buyer1, &Side::Bid, &(100 * PRICE_PRECISION), &50);
    assert_eq!(res, Err(Ok(Error::BuyerNotAccredited.into())));
}

#[test]
fn test_accreditation_requires_future_expiry() {
    let f = setup();
    let res = f
        .dex
        .try_set_accredited_investor(&f.admin, &f.buyer1, &true, &f.now());
    assert_eq!(res, Err(Ok(Error::InvalidAccreditationExpiry.into())));
}

#[test]
fn test_resting_bid_cancelled_when_accreditation_lapses() {
    let f = setup();
    f.kyc(&f.buyer1);
    f.dex
        .set_accredited_investor(&f.admin, &f.buyer1, &true, &(f.now() + 100));
    let quote_before = f.quote.balance(&f.buyer1);
    let price = 100 * PRICE_PRECISION;
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &50);

    f.env.ledger().set_timestamp(f.now() + 101);
    f.attest_mature(&f.seller1);
    let ask = f.dex.place_order(&f.seller1, &Side::Ask, &price, &50);

    // The match is intercepted: the bid is cancelled with its full escrow refunded.
    assert_eq!(
        f.dex.get_order(&bid).unwrap().status,
        OrderStatus::Cancelled
    );
    assert_eq!(f.quote.balance(&f.buyer1), quote_before);
    assert_eq!(f.base.balance(&f.buyer1), 0);
    assert_eq!(f.dex.get_order(&ask).unwrap().status, OrderStatus::Open);
}

// ---------------------------------------------------------------------------
// Acceptance criterion 3: partial fills with dynamic fee deductions
// ---------------------------------------------------------------------------

#[test]
fn test_partial_filling_and_dynamic_fee_deductions() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let seller_quote = f.quote.balance(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);

    let price = 100 * PRICE_PRECISION;
    let maker_ask = f.dex.place_order(&f.seller1, &Side::Ask, &price, &100);
    let taker_bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &40);

    let maker = f.dex.get_order(&maker_ask).unwrap();
    assert_eq!(
        (maker.status, maker.filled_amount),
        (OrderStatus::PartiallyFilled, 40)
    );
    let taker = f.dex.get_order(&taker_bid).unwrap();
    assert_eq!(
        (taker.status, taker.filled_amount),
        (OrderStatus::Filled, 40)
    );
    assert_eq!(taker.quote_escrow, 0);

    // Gross 4,000. Seller is maker: 10 bps = 4 deducted. Buyer is taker: 25 bps = 10 on top.
    assert_eq!(f.quote.balance(&f.seller1) - seller_quote, 3_996);
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 4_010);
    assert_eq!(f.quote.balance(&f.treasury), 14);
    assert_eq!(f.quote.balance(&f.dex.address), 0);
    assert_eq!(f.dex.get_trader_volume(&f.buyer1), 4_000);
    assert_eq!(f.dex.get_trader_volume(&f.seller1), 4_000);
}

#[test]
fn test_seller_pays_taker_fee_when_seller_is_taker() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let seller_quote = f.quote.balance(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);

    let price = 100 * PRICE_PRECISION;
    f.dex.place_order(&f.buyer1, &Side::Bid, &price, &40);
    f.dex.place_order(&f.seller1, &Side::Ask, &price, &40);

    // Buyer is maker: 10 bps = 4 on top. Seller is taker: 25 bps = 10 deducted.
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 4_004);
    assert_eq!(f.quote.balance(&f.seller1) - seller_quote, 3_990);
    assert_eq!(f.quote.balance(&f.treasury), 14);
    assert_eq!(f.quote.balance(&f.dex.address), 0);
}

#[test]
fn test_volume_discount_dynamic_fee() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);

    // Build 60,000 of volume for buyer1 (> 50,000 threshold).
    let high_price = 600 * PRICE_PRECISION;
    f.dex.place_order(&f.seller1, &Side::Ask, &high_price, &100);
    f.dex.place_order(&f.buyer1, &Side::Bid, &high_price, &100);
    assert!(f.dex.get_trader_volume(&f.buyer1) >= 50_000);

    let treasury_before = f.quote.balance(&f.treasury);
    let price = 100 * PRICE_PRECISION;
    f.dex.place_order(&f.seller1, &Side::Ask, &price, &100);
    f.dex.place_order(&f.buyer1, &Side::Bid, &price, &100);

    // Gross 10,000: maker 10 bps = 10, discounted taker 15 bps = 15.
    assert_eq!(f.quote.balance(&f.treasury) - treasury_before, 25);
}

#[test]
fn test_price_improvement_is_refunded() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);

    f.dex
        .place_order(&f.seller1, &Side::Ask, &(95 * PRICE_PRECISION), &10);
    f.dex
        .place_order(&f.buyer1, &Side::Bid, &(100 * PRICE_PRECISION), &10);

    // Fills at the maker's 95: 950 + 25 bps taker fee (2) = 952, not the 1,000 limit.
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 952);
    assert_eq!(f.quote.balance(&f.dex.address), 0);
}

#[test]
fn test_order_cancellation_and_escrow_refund() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let seller_base = f.base.balance(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);
    let price = 50 * PRICE_PRECISION;

    let ask = f.dex.place_order(&f.seller1, &Side::Ask, &price, &100);
    assert_eq!(f.base.balance(&f.seller1), seller_base - 100);
    f.dex.cancel_order(&f.seller1, &ask);
    assert_eq!(f.base.balance(&f.seller1), seller_base);
    assert_eq!(
        f.dex.get_order(&ask).unwrap().status,
        OrderStatus::Cancelled
    );

    // Bid escrows 2,500 notional + 25 bps (6) fee headroom.
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &50);
    assert_eq!(f.quote.balance(&f.buyer1), buyer_quote - 2_506);
    f.dex.cancel_order(&f.buyer1, &bid);
    assert_eq!(f.quote.balance(&f.buyer1), buyer_quote);
    assert_eq!(
        f.dex.get_order(&bid).unwrap().status,
        OrderStatus::Cancelled
    );

    assert_eq!(f.dex.get_orderbook_depth(&Side::Bid, &10).len(), 0);
    assert_eq!(f.dex.get_orderbook_depth(&Side::Ask, &10).len(), 0);

    let res = f.dex.try_cancel_order(&f.buyer1, &bid);
    assert_eq!(res, Err(Ok(Error::OrderAlreadyFilledOrCancelled.into())));
}

#[test]
fn test_cancel_partially_filled_bid_refunds_exact_remainder() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);
    let price = 50 * PRICE_PRECISION;

    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &100);
    f.dex.place_order(&f.seller1, &Side::Ask, &price, &40);
    f.dex.cancel_order(&f.buyer1, &bid);

    // Buyer was maker on 40 units: 2,000 + 10 bps (2). Everything else comes back.
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 2_002);
    assert_eq!(f.base.balance(&f.buyer1), 40);
    assert_eq!(f.quote.balance(&f.dex.address), 0);
}

#[test]
fn test_only_owner_can_cancel() {
    let f = setup();
    f.attest_mature(&f.seller1);
    let ask = f
        .dex
        .place_order(&f.seller1, &Side::Ask, &(10 * PRICE_PRECISION), &10);
    let res = f.dex.try_cancel_order(&f.seller2, &ask);
    assert_eq!(res, Err(Ok(Error::Unauthorized.into())));
}

#[test]
fn test_dust_order_rejected() {
    let f = setup();
    f.attest_mature(&f.seller1);
    // 1 unit at 0.5 quote/unit rounds to zero quote tokens.
    let res = f
        .dex
        .try_place_order(&f.seller1, &Side::Ask, &(PRICE_PRECISION / 2), &1);
    assert_eq!(res, Err(Ok(Error::ZeroQuoteAmount.into())));
}

#[test]
fn test_dust_remainder_is_refunded_not_stranded() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let seller_base = f.base.balance(&f.seller1);
    let half = PRICE_PRECISION / 2;

    // 3 units @ 0.5 = 1 quote. After 2 fill, the last unit is worth 0 and can never trade.
    let ask = f.dex.place_order(&f.seller1, &Side::Ask, &half, &3);
    f.dex.place_order(&f.buyer1, &Side::Bid, &half, &2);

    let ask = f.dex.get_order(&ask).unwrap();
    assert_eq!((ask.status, ask.filled_amount), (OrderStatus::Cancelled, 2));
    assert_eq!(f.base.balance(&f.seller1), seller_base - 2);
    assert_eq!(f.dex.get_orderbook_depth(&Side::Ask, &10).len(), 0);
    assert_eq!(f.base.balance(&f.dex.address), 0);
}

#[test]
fn test_self_trade_rejected() {
    let f = setup();
    f.accredit(&f.seller1);
    f.attest_mature(&f.seller1);
    StellarAssetClient::new(&f.env, &f.quote.address).mint(&f.seller1, &1_000_000);
    let price = 10 * PRICE_PRECISION;
    f.dex.place_order(&f.seller1, &Side::Ask, &price, &10);
    let res = f.dex.try_place_order(&f.seller1, &Side::Bid, &price, &10);
    assert_eq!(res, Err(Ok(Error::SelfTrade.into())));
}

// ---------------------------------------------------------------------------
// Bounded work per transaction
// ---------------------------------------------------------------------------

#[test]
fn test_fill_cap_closes_remainder_instead_of_crossing_book() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);
    let price = 100 * PRICE_PRECISION;

    // The test budget accumulates across calls; only the final match is measured.
    f.env.cost_estimate().budget().reset_unlimited();
    for _ in 0..=MAX_FILLS_PER_ORDER {
        f.dex.place_order(&f.seller1, &Side::Ask, &price, &1);
    }
    let units = MAX_FILLS_PER_ORDER as i128 + 1;
    f.env.cost_estimate().budget().reset_unlimited();
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &units);

    // A fill-capped match must stay well inside one transaction's limits
    // (100M CPU / 40 MiB in the default test budget; testnet allows 400M CPU).
    let budget = f.env.cost_estimate().budget();
    assert!(budget.cpu_instruction_cost() < 100_000_000);
    assert!(budget.memory_bytes_cost() < 41_943_040);

    let bid = f.dex.get_order(&bid).unwrap();
    assert_eq!(bid.filled_amount, MAX_FILLS_PER_ORDER as i128);
    assert_eq!(bid.status, OrderStatus::Cancelled);
    // Paid only for the 10 fills (100 each, taker fee rounds to 0).
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 1_000);
    assert_eq!(
        f.dex.get_orderbook_depth(&Side::Ask, &10).get(0).unwrap(),
        (price, 1)
    );
    assert_eq!(f.dex.get_orderbook_depth(&Side::Bid, &10).len(), 0);
}

#[test]
fn test_price_level_order_cap() {
    let f = setup();
    f.attest_mature(&f.seller1);
    let price = 100 * PRICE_PRECISION;
    for _ in 0..MAX_ORDERS_PER_LEVEL {
        f.dex.place_order(&f.seller1, &Side::Ask, &price, &1);
    }
    let res = f.dex.try_place_order(&f.seller1, &Side::Ask, &price, &1);
    assert_eq!(res, Err(Ok(Error::PriceLevelFull.into())));
}

#[test]
fn test_price_ladder_cap() {
    let f = setup();
    f.attest_mature(&f.seller1);
    f.env.cost_estimate().budget().reset_unlimited();
    for i in 0..MAX_PRICE_LEVELS as i128 {
        f.dex
            .place_order(&f.seller1, &Side::Ask, &((100 + i) * PRICE_PRECISION), &1);
    }
    let res = f
        .dex
        .try_place_order(&f.seller1, &Side::Ask, &(500 * PRICE_PRECISION), &1);
    assert_eq!(res, Err(Ok(Error::PriceLadderFull.into())));
}

// ---------------------------------------------------------------------------
// Admin controls
// ---------------------------------------------------------------------------

#[test]
fn test_emergency_pause_and_admin_controls() {
    let f = setup();
    f.attest_mature(&f.seller1);
    f.dex.pause(&f.admin);
    let res = f
        .dex
        .try_place_order(&f.seller1, &Side::Ask, &(100 * PRICE_PRECISION), &50);
    assert_eq!(res, Err(Ok(Error::Paused.into())));
    f.dex.unpause(&f.admin);
    f.dex
        .place_order(&f.seller1, &Side::Ask, &(100 * PRICE_PRECISION), &50);
}

#[test]
fn test_fee_config_validation() {
    let f = setup();
    let mut config = f.dex.get_fee_config();
    config.taker_fee_bps = crate::MAX_FEE_BPS + 1;
    let res = f.dex.try_set_fee_config(&f.admin, &config);
    assert_eq!(res, Err(Ok(Error::InvalidFeeConfig.into())));

    let mut config = f.dex.get_fee_config();
    config.volume_discount_bps = config.taker_fee_bps + 1;
    let res = f.dex.try_set_fee_config(&f.admin, &config);
    assert_eq!(res, Err(Ok(Error::InvalidFeeConfig.into())));
}

#[test]
fn test_raised_fees_never_exceed_bid_escrow() {
    let f = setup();
    f.accredit(&f.buyer1);
    f.attest_mature(&f.seller1);
    let buyer_quote = f.quote.balance(&f.buyer1);
    let price = 100 * PRICE_PRECISION;
    let bid = f.dex.place_order(&f.buyer1, &Side::Bid, &price, &100);

    // Admin raises the maker fee above what the resting bid escrowed for.
    let mut config = f.dex.get_fee_config();
    config.maker_fee_bps = 100;
    f.dex.set_fee_config(&f.admin, &config);

    f.dex.place_order(&f.seller1, &Side::Ask, &price, &100);

    // Buyer is charged at most its escrowed 25 bps: 10,000 + 25.
    assert_eq!(f.dex.get_order(&bid).unwrap().status, OrderStatus::Filled);
    assert_eq!(buyer_quote - f.quote.balance(&f.buyer1), 10_025);
    assert_eq!(f.quote.balance(&f.dex.address), 0);
}
