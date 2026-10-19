//! Secondary Market Orderbook DEX for SEC Rule 144 Restricted Shares (Issue #220).
//!
//! # Overview
//! An on-chain limit orderbook for trading SEC Rule 144 restricted securities, with
//! mandatory holding-period verification before any order is matched.
//!
//! # Design
//! 1. **Price-time priority** — bids and asks live in Soroban persistent storage as a
//!    sorted price ladder per side (`PriceLadder`: descending for bids, ascending for
//!    asks) plus a FIFO queue of order ids per price level (`PriceQueue`). Each order is
//!    its own storage entry (`Order(id)`).
//!
//! 2. **Rule 144 trade interception** — every matched trade is checked before it
//!    settles:
//!    - *Seller holding period*: `now >= acquisition_time + holding_period`. The
//!      acquisition time is attested by the admin (issuer / transfer agent) with
//!      [`RestrictedOrderbookDex::record_seller_acquisition`]; sellers cannot set it
//!      themselves. Rule 144(d)(1) requires six months for issuers that have been
//!      SEC-reporting for at least 90 days and one year otherwise; the default
//!      ([`DEFAULT_RULE_144_HOLDING_PERIOD`]) is 184 days, the longest span six calendar
//!      months can cover, and the admin can raise it (e.g. to
//!      [`NON_REPORTING_ISSUER_HOLDING_PERIOD`]) but never lower it.
//!    - *Buyer accreditation*: the buyer needs an admin-recorded, unexpired accredited
//!      investor record. When a compliance contract is configured its `is_allowed`
//!      (KYC / jurisdiction) check must pass as well; it is never a substitute for
//!      accreditation. Note that Rule 144 itself places no restriction on buyers —
//!      this is a venue-level requirement from the issue.
//!
//!    Both checks run when an order is placed and again for every fill. If an
//!    incoming order fails, the transaction reverts. If a resting order has become
//!    ineligible since it was placed (accreditation expired, acquisition date
//!    corrected), it is cancelled and its escrow refunded, so stale orders cannot
//!    clog the book.
//!
//! 3. **Partial fills and dynamic fees** — a taker can fill against several makers, and
//!    any remainder rests on the book. Fees are charged in the quote token: the buyer
//!    pays its fee on top of the trade value and the seller's fee is deducted from its
//!    proceeds, each at the maker or taker rate depending on its role. Takers whose
//!    cumulative volume reaches `volume_discount_threshold` get `volume_discount_bps`
//!    off the taker rate. Bids escrow the notional plus the highest fee rate at
//!    placement, and anything left over is refunded.
//!
//! # Bounded work
//! At most [`MAX_PRICE_LEVELS`] levels per side, [`MAX_ORDERS_PER_LEVEL`] orders per
//! level and [`MAX_FILLS_PER_ORDER`] resting orders touched per incoming order. When
//! the fill cap is hit, or the remainder is too small to trade (rounds to zero quote),
//! the remainder is refunded and the order closes as `Cancelled` instead of resting on
//! a crossed book.

// `env.events().publish` is deprecated in soroban-sdk 25; the other Tessera contracts
// keep using it to preserve their event ABI, and this contract follows suit.
#![allow(deprecated)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, BytesN, Env, IntoVal, Symbol, TryFromVal, Val, Vec,
};

/// Price scaling precision (10^7, standard Stellar 7 decimal places).
pub const PRICE_PRECISION: i128 = 10_000_000;

/// Basis points divisor (10,000 bps = 100%).
pub const BPS_DIVISOR: i128 = 10_000;

/// Upper bound for a single maker or taker fee rate (10%).
pub const MAX_FEE_BPS: i128 = 1_000;

/// Number of seconds in a standard day.
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Default Rule 144(d)(1)(i) holding period for reporting issuers: six months, taken as
/// 184 days (the longest span six calendar months can cover, e.g. July–December) so
/// that the check is never shorter than the statutory period.
pub const DEFAULT_RULE_144_HOLDING_PERIOD: u64 = 184 * SECONDS_PER_DAY;

/// Rule 144(d)(1)(ii) holding period for non-reporting issuers: one year (366 days to
/// cover leap years).
pub const NON_REPORTING_ISSUER_HOLDING_PERIOD: u64 = 366 * SECONDS_PER_DAY;

/// Maximum number of active price levels per side.
pub const MAX_PRICE_LEVELS: u32 = 100;

/// Maximum number of resting orders queued at one price level.
pub const MAX_ORDERS_PER_LEVEL: u32 = 50;

/// Maximum number of resting orders an incoming order may fill or cancel in one call.
pub const MAX_FILLS_PER_ORDER: u32 = 10;

const DAY_IN_LEDGERS: u32 = 17_280;
const TTL_THRESHOLD: u32 = 30 * DAY_IN_LEDGERS;
/// Ledgers that persistent and instance entries are extended to whenever touched.
pub const TTL_EXTEND_TO: u32 = 120 * DAY_IN_LEDGERS;

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Side {
    Bid = 1, // Buyer: offers QuoteToken to purchase BaseToken
    Ask = 2, // Seller: offers BaseToken (Rule 144 restricted) to receive QuoteToken
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OrderStatus {
    Open = 1,
    PartiallyFilled = 2,
    Filled = 3,
    /// Cancelled by the trader, closed because its remainder could no longer trade, or
    /// cancelled by the matching engine after the trader became ineligible.
    Cancelled = 4,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub id: u64,
    pub trader: Address,
    pub side: Side,
    pub price: i128,                  // In quote units scaled by PRICE_PRECISION
    pub amount: i128,                 // Total base token amount ordered
    pub filled_amount: i128,          // Base token amount filled so far
    pub created_at: u64,              // Ledger timestamp (seconds) when order was created
    pub seller_acquisition_time: u64, // Attested acquisition time at placement (asks only)
    pub fee_bps_cap: i128,            // Highest fee rate escrowed for (bids only)
    pub quote_escrow: i128,           // Quote tokens still held in escrow (bids only)
    pub status: OrderStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicFeeConfig {
    pub maker_fee_bps: i128, // Maker fee in basis points (e.g. 10 = 0.10%)
    pub taker_fee_bps: i128, // Taker fee in basis points (e.g. 25 = 0.25%)
    pub volume_discount_threshold: i128, // Cumulative quote volume threshold for discount
    pub volume_discount_bps: i128, // Discount in basis points deducted from taker fee
    pub fee_recipient: Address, // Protocol treasury address
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeExecution {
    pub order_id: u64,
    pub matched_order_id: u64,
    pub maker: Address,
    pub taker: Address,
    pub side: Side, // Taker's side
    pub price: i128,
    pub fill_amount: i128,  // Base token amount exchanged
    pub quote_amount: i128, // Gross quote token amount exchanged
    pub buyer_fee: i128,    // Fee paid by the buyer on top of quote_amount
    pub seller_fee: i128,   // Fee deducted from the seller's proceeds
    pub timestamp: u64,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    Paused = 4,
    InvalidAmount = 5,
    InvalidPrice = 6,
    OrderNotFound = 7,
    OrderAlreadyFilledOrCancelled = 8,
    /// Seller has no attested acquisition date or the Rule 144 holding period has not elapsed.
    SellerHoldingPeriodNotMet = 9,
    /// Buyer has no unexpired accredited investor record, or fails the compliance gate.
    BuyerNotAccredited = 10,
    InvalidFeeConfig = 11,
    PriceLadderFull = 12,
    /// Order value rounds to zero quote tokens.
    ZeroQuoteAmount = 13,
    InvalidAcquisitionTimestamp = 14,
    /// Holding period below the Rule 144 six-month minimum.
    InvalidHoldingPeriod = 15,
    /// Accreditation must carry an expiry in the future.
    InvalidAccreditationExpiry = 16,
    PriceLevelFull = 17,
    /// Incoming order would match the trader's own resting order.
    SelfTrade = 18,
    ArithmeticOverflow = 19,
}

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    BaseToken,            // Address of Restricted SEC Rule 144 Share Token
    QuoteToken,           // Address of Payment Currency (e.g. USDC)
    ComplianceContract,   // Option<Address>
    NextOrderId,          // u64
    FeeConfig,            // DynamicFeeConfig
    HoldingPeriodSeconds, // u64

    Order(u64),

    // Price-time priority queues:
    PriceLadder(Side),      // Vec<i128>: sorted distinct price levels
    PriceQueue(Side, i128), // Vec<u64>: FIFO order ids at a price level

    // Compliance & Rule 144 state:
    SellerAcquisition(Address), // u64: admin-attested acquisition timestamp
    AccreditedInvestor(Address), // (is_accredited: bool, valid_until: u64)
    TraderVolume(Address),      // i128: cumulative quote volume
}

/// Values shared by every fill of one incoming order.
struct MatchContext {
    base_token: Address,
    quote_token: Address,
    fee_config: DynamicFeeConfig,
    holding_period: u64,
    now: u64,
}

#[contract]
pub struct RestrictedOrderbookDex;

#[contractimpl]
impl RestrictedOrderbookDex {
    /// Initialize the DEX. A `holding_period_seconds` of 0 selects the six-month default.
    pub fn initialize(
        env: Env,
        admin: Address,
        base_token: Address,
        quote_token: Address,
        compliance_contract: Option<Address>,
        fee_config: DynamicFeeConfig,
        holding_period_seconds: u64,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();

        Self::validate_fee_config(&env, &fee_config);

        let holding_period = if holding_period_seconds == 0 {
            DEFAULT_RULE_144_HOLDING_PERIOD
        } else {
            holding_period_seconds
        };
        Self::validate_holding_period(&env, holding_period);

        let instance = env.storage().instance();
        instance.set(&DataKey::Admin, &admin);
        instance.set(&DataKey::BaseToken, &base_token);
        instance.set(&DataKey::QuoteToken, &quote_token);
        instance.set(&DataKey::ComplianceContract, &compliance_contract);
        instance.set(&DataKey::FeeConfig, &fee_config);
        instance.set(&DataKey::HoldingPeriodSeconds, &holding_period);
        instance.set(&DataKey::NextOrderId, &1u64);
        Self::bump_instance(&env);

        Self::put(
            &env,
            &DataKey::PriceLadder(Side::Bid),
            &Vec::<i128>::new(&env),
        );
        Self::put(
            &env,
            &DataKey::PriceLadder(Side::Ask),
            &Vec::<i128>::new(&env),
        );

        env.events().publish(
            (symbol_short!("init_dex"), admin),
            (base_token, quote_token, holding_period),
        );
    }

    /// Record an investor's accredited status. Admin only. An accredited record must
    /// expire in the future; verification is not open-ended.
    pub fn set_accredited_investor(
        env: Env,
        admin: Address,
        investor: Address,
        is_accredited: bool,
        valid_until: u64,
    ) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        if is_accredited && valid_until <= env.ledger().timestamp() {
            panic_with_error!(env, Error::InvalidAccreditationExpiry);
        }

        Self::put(
            &env,
            &DataKey::AccreditedInvestor(investor.clone()),
            &(is_accredited, valid_until),
        );

        env.events().publish(
            (symbol_short!("accredit"), investor),
            (is_accredited, valid_until),
        );
    }

    /// Whether `investor` may buy: it needs an unexpired accredited record and, when a
    /// compliance contract is configured, must also pass its `is_allowed` gate.
    pub fn is_accredited(env: Env, investor: Address) -> bool {
        let now = env.ledger().timestamp();
        let record: Option<(bool, u64)> =
            Self::get(&env, &DataKey::AccreditedInvestor(investor.clone()));
        let accredited = matches!(record, Some((true, valid_until)) if now <= valid_until);
        if !accredited {
            return false;
        }

        let compliance: Option<Address> = env
            .storage()
            .instance()
            .get::<DataKey, Option<Address>>(&DataKey::ComplianceContract)
            .flatten();
        match compliance {
            Some(compliance_addr) => {
                let args: Vec<Val> = (investor,).into_val(&env);
                env.invoke_contract(&compliance_addr, &Symbol::new(&env, "is_allowed"), args)
            }
            None => true,
        }
    }

    /// Attest the date a seller acquired (and fully paid for) their restricted shares.
    /// Admin only: the holding period cannot be self-certified by the seller.
    pub fn record_seller_acquisition(
        env: Env,
        admin: Address,
        seller: Address,
        acquisition_timestamp: u64,
    ) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);

        if acquisition_timestamp > env.ledger().timestamp() {
            panic_with_error!(env, Error::InvalidAcquisitionTimestamp);
        }

        Self::put(
            &env,
            &DataKey::SellerAcquisition(seller.clone()),
            &acquisition_timestamp,
        );

        env.events()
            .publish((symbol_short!("acq_date"), seller), acquisition_timestamp);
    }

    /// Returns `(acquisition_time, holding_period, holding_period_met)` for a seller.
    /// `acquisition_time` is 0 when none has been attested.
    pub fn get_seller_holding_info(env: Env, seller: Address) -> (u64, u64, bool) {
        let acquisition: Option<u64> = Self::get(&env, &DataKey::SellerAcquisition(seller));
        let holding_period = Self::holding_period(&env);
        let satisfied = Self::holding_met(acquisition, holding_period, env.ledger().timestamp());
        (acquisition.unwrap_or(0), holding_period, satisfied)
    }

    /// Place a limit order and match it against the opposite side of the book.
    ///
    /// - Asks escrow `amount` base tokens; the seller must have met the holding period.
    /// - Bids escrow the notional plus fees at the higher of the maker and taker rates;
    ///   the buyer must be accredited.
    /// - Matching runs best price first, FIFO within a price, settling at the resting
    ///   order's price. Every fill re-verifies the seller's holding period and the
    ///   buyer's accreditation (see the module docs).
    /// - A remainder rests on the book; unused bid escrow is refunded.
    pub fn place_order(env: Env, trader: Address, side: Side, price: i128, amount: i128) -> u64 {
        Self::require_not_paused(&env);
        trader.require_auth();
        Self::bump_instance(&env);

        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        if price <= 0 {
            panic_with_error!(env, Error::InvalidPrice);
        }
        let notional = Self::notional(&env, amount, price);
        if notional <= 0 {
            panic_with_error!(env, Error::ZeroQuoteAmount);
        }

        let ctx = Self::context(&env);
        let contract_address = env.current_contract_address();

        let instance = env.storage().instance();
        let order_id: u64 = instance.get(&DataKey::NextOrderId).unwrap();
        instance.set(&DataKey::NextOrderId, &(order_id + 1));

        let mut order = Order {
            id: order_id,
            trader: trader.clone(),
            side,
            price,
            amount,
            filled_amount: 0,
            created_at: ctx.now,
            seller_acquisition_time: 0,
            fee_bps_cap: 0,
            quote_escrow: 0,
            status: OrderStatus::Open,
        };

        match side {
            Side::Ask => {
                let acquisition: Option<u64> =
                    Self::get(&env, &DataKey::SellerAcquisition(trader.clone()));
                if !Self::holding_met(acquisition, ctx.holding_period, ctx.now) {
                    panic_with_error!(env, Error::SellerHoldingPeriodNotMet);
                }
                order.seller_acquisition_time = acquisition.unwrap_or(0);
                token::Client::new(&env, &ctx.base_token).transfer(
                    &trader,
                    &contract_address,
                    &amount,
                );
            }
            Side::Bid => {
                if !Self::is_accredited(env.clone(), trader.clone()) {
                    panic_with_error!(env, Error::BuyerNotAccredited);
                }
                order.fee_bps_cap = ctx
                    .fee_config
                    .maker_fee_bps
                    .max(ctx.fee_config.taker_fee_bps);
                order.quote_escrow = Self::bid_reserve(&env, amount, price, order.fee_bps_cap);
                token::Client::new(&env, &ctx.quote_token).transfer(
                    &trader,
                    &contract_address,
                    &order.quote_escrow,
                );
            }
        }

        let close_remainder = Self::match_incoming_order(&env, &mut order, &ctx);

        let remaining = order.amount - order.filled_amount;
        let rests =
            remaining > 0 && !close_remainder && Self::notional(&env, remaining, order.price) > 0;

        if remaining == 0 {
            order.status = OrderStatus::Filled;
            Self::refund_escrow(&env, &mut order, &ctx);
        } else if rests {
            order.status = if order.filled_amount > 0 {
                OrderStatus::PartiallyFilled
            } else {
                OrderStatus::Open
            };
            if side == Side::Bid {
                // Price improvement: keep only what the remainder can still cost.
                let reserve = Self::bid_reserve(&env, remaining, price, order.fee_bps_cap);
                let excess = order.quote_escrow - reserve;
                if excess > 0 {
                    token::Client::new(&env, &ctx.quote_token).transfer(
                        &contract_address,
                        &trader,
                        &excess,
                    );
                    order.quote_escrow = reserve;
                }
            }
            Self::insert_into_orderbook(&env, side, price, order_id);
        } else {
            Self::close_order(&env, &mut order, &ctx);
        }

        Self::put(&env, &DataKey::Order(order_id), &order);

        env.events().publish(
            (symbol_short!("order"), trader),
            (
                order_id,
                price,
                amount,
                order.filled_amount,
                order.status as u32,
            ),
        );

        order_id
    }

    /// Cancel an open or partially filled order and refund its remaining escrow.
    pub fn cancel_order(env: Env, trader: Address, order_id: u64) {
        Self::require_not_paused(&env);
        trader.require_auth();
        Self::bump_instance(&env);

        let mut order: Order = Self::get(&env, &DataKey::Order(order_id))
            .unwrap_or_else(|| panic_with_error!(env, Error::OrderNotFound));

        if order.trader != trader {
            panic_with_error!(env, Error::Unauthorized);
        }
        if !Self::is_active(order.status) {
            panic_with_error!(env, Error::OrderAlreadyFilledOrCancelled);
        }

        let ctx = Self::context(&env);
        let unfilled_amount = order.amount - order.filled_amount;
        Self::remove_from_orderbook(&env, order.side, order.price, order_id);
        Self::close_order(&env, &mut order, &ctx);
        Self::put(&env, &DataKey::Order(order_id), &order);

        env.events().publish(
            (symbol_short!("cancelled"), trader),
            (order_id, unfilled_amount),
        );
    }

    /// Query individual order details by ID.
    pub fn get_order(env: Env, order_id: u64) -> Option<Order> {
        Self::get(&env, &DataKey::Order(order_id))
    }

    /// Query current best price on a given side of the book.
    pub fn get_best_price(env: Env, side: Side) -> Option<i128> {
        Self::ladder(&env, side).first()
    }

    /// Query orderbook depth: returns a list of `(price, total_unfilled_volume)` up to `max_levels`.
    pub fn get_orderbook_depth(env: Env, side: Side, max_levels: u32) -> Vec<(i128, i128)> {
        let ladder = Self::ladder(&env, side);
        let mut depth_levels = Vec::new(&env);

        for price in ladder.iter().take(max_levels as usize) {
            let queue: Vec<u64> =
                Self::get(&env, &DataKey::PriceQueue(side, price)).unwrap_or(Vec::new(&env));

            let mut level_volume = 0i128;
            for oid in queue.iter() {
                if let Some(ord) = Self::get::<Order>(&env, &DataKey::Order(oid)) {
                    if Self::is_active(ord.status) {
                        level_volume += ord.amount - ord.filled_amount;
                    }
                }
            }
            if level_volume > 0 {
                depth_levels.push_back((price, level_volume));
            }
        }

        depth_levels
    }

    /// Query dynamic fee configuration.
    pub fn get_fee_config(env: Env) -> DynamicFeeConfig {
        env.storage().instance().get(&DataKey::FeeConfig).unwrap()
    }

    /// Update dynamic fee configuration. Admin only. Resting bids are never charged
    /// more than the fee rate they escrowed for.
    pub fn set_fee_config(env: Env, admin: Address, fee_config: DynamicFeeConfig) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);
        Self::validate_fee_config(&env, &fee_config);
        env.storage()
            .instance()
            .set(&DataKey::FeeConfig, &fee_config);
    }

    /// Update the holding period. Admin only; cannot go below the six-month default.
    pub fn set_holding_period_seconds(env: Env, admin: Address, holding_period_seconds: u64) {
        Self::require_not_paused(&env);
        Self::require_admin(&env, &admin);
        Self::validate_holding_period(&env, holding_period_seconds);
        env.storage()
            .instance()
            .set(&DataKey::HoldingPeriodSeconds, &holding_period_seconds);
    }

    /// Query cumulative trading volume for dynamic fee tier discounts.
    pub fn get_trader_volume(env: Env, trader: Address) -> i128 {
        Self::get(&env, &DataKey::TraderVolume(trader)).unwrap_or(0)
    }

    /// Contract ABI version for indexer compatibility.
    pub fn version(_env: Env) -> u64 {
        1
    }

    // ---- Emergency Pause & Upgrade Pattern ----

    pub fn pause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, true);
        env.events().publish((symbol_short!("pause"), admin), ());
    }

    pub fn unpause(env: Env, admin: Address) {
        Self::require_admin(&env, &admin);
        tessera_common::set_paused(&env, false);
        env.events().publish((symbol_short!("unpause"), admin), ());
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::require_admin(&env, &admin);
        tessera_common::upgrade(&env, new_wasm_hash);
    }
}

// ---- Internal Matching Engine & Helper Functions ----
impl RestrictedOrderbookDex {
    /// Match `taker` against the opposite side of the book, updating its fill state and
    /// escrow in place. Returns `true` when the taker's remainder must be closed rather
    /// than rested (fill cap reached, or the remainder rounds to zero quote).
    fn match_incoming_order(env: &Env, taker: &mut Order, ctx: &MatchContext) -> bool {
        let opposite_side = match taker.side {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        };
        let contract_address = env.current_contract_address();
        let base_client = token::Client::new(env, &ctx.base_token);
        let quote_client = token::Client::new(env, &ctx.quote_token);

        let mut ladder = Self::ladder(env, opposite_side);
        let mut ladder_idx = 0u32;
        let mut processed = 0u32;
        let mut close_remainder = false;

        while ladder_idx < ladder.len() && taker.amount > taker.filled_amount && !close_remainder {
            let maker_price = ladder.get(ladder_idx).unwrap();
            let crosses = match taker.side {
                Side::Bid => maker_price <= taker.price,
                Side::Ask => maker_price >= taker.price,
            };
            if !crosses {
                break;
            }

            let queue_key = DataKey::PriceQueue(opposite_side, maker_price);
            let queue: Vec<u64> = Self::get(env, &queue_key).unwrap_or(Vec::new(env));
            let mut kept = Vec::new(env);
            let mut queue_idx = 0u32;

            while queue_idx < queue.len() && taker.amount > taker.filled_amount {
                let maker_id = queue.get(queue_idx).unwrap();
                queue_idx += 1;

                let mut maker: Order = match Self::get(env, &DataKey::Order(maker_id)) {
                    Some(o) => o,
                    None => continue,
                };
                if !Self::is_active(maker.status) {
                    continue; // stale entry: drop it from the queue
                }
                if processed >= MAX_FILLS_PER_ORDER {
                    kept.push_back(maker_id);
                    close_remainder = true;
                    break;
                }
                processed += 1;

                if maker.trader == taker.trader {
                    panic_with_error!(env, Error::SelfTrade);
                }

                // =========================================================
                // TRADE INTERCEPTION GATES (SEC Rule 144 & Accreditation)
                // =========================================================
                let (seller, buyer) = match taker.side {
                    Side::Bid => (maker.trader.clone(), taker.trader.clone()),
                    Side::Ask => (taker.trader.clone(), maker.trader.clone()),
                };

                let acquisition: Option<u64> =
                    Self::get(env, &DataKey::SellerAcquisition(seller.clone()));
                if !Self::holding_met(acquisition, ctx.holding_period, ctx.now) {
                    if taker.side == Side::Ask {
                        panic_with_error!(env, Error::SellerHoldingPeriodNotMet);
                    }
                    Self::cancel_ineligible(env, &mut maker, ctx, symbol_short!("hold_per"));
                    continue;
                }

                if !Self::is_accredited(env.clone(), buyer.clone()) {
                    if taker.side == Side::Bid {
                        panic_with_error!(env, Error::BuyerNotAccredited);
                    }
                    Self::cancel_ineligible(env, &mut maker, ctx, symbol_short!("accredit"));
                    continue;
                }

                // =========================================================
                // PARTIAL FILL EXECUTION & DYNAMIC FEE DEDUCTION
                // =========================================================
                let trade_amount =
                    (taker.amount - taker.filled_amount).min(maker.amount - maker.filled_amount);
                let trade_quote = Self::notional(env, trade_amount, maker_price);
                if trade_quote <= 0 {
                    // The taker's remainder is too small to trade at this price.
                    kept.push_back(maker_id);
                    close_remainder = true;
                    break;
                }

                let taker_volume: i128 =
                    Self::get(env, &DataKey::TraderVolume(taker.trader.clone())).unwrap_or(0);
                let taker_bps = if taker_volume >= ctx.fee_config.volume_discount_threshold {
                    ctx.fee_config.taker_fee_bps - ctx.fee_config.volume_discount_bps
                } else {
                    ctx.fee_config.taker_fee_bps
                };
                let maker_bps = ctx.fee_config.maker_fee_bps;

                // The buyer is never charged more than the rate its bid escrowed for.
                let (buyer_bps, seller_bps, buyer_bps_cap) = match taker.side {
                    Side::Bid => (taker_bps, maker_bps, taker.fee_bps_cap),
                    Side::Ask => (maker_bps, taker_bps, maker.fee_bps_cap),
                };
                let buyer_fee = Self::fee_on(env, trade_quote, buyer_bps.min(buyer_bps_cap));
                let seller_fee = Self::fee_on(env, trade_quote, seller_bps);
                let buyer_cost = trade_quote + buyer_fee;
                let seller_proceeds = trade_quote - seller_fee;
                let total_fee = buyer_fee + seller_fee;

                base_client.transfer(&contract_address, &buyer, &trade_amount);
                if seller_proceeds > 0 {
                    quote_client.transfer(&contract_address, &seller, &seller_proceeds);
                }
                if total_fee > 0 {
                    quote_client.transfer(
                        &contract_address,
                        &ctx.fee_config.fee_recipient,
                        &total_fee,
                    );
                }

                match taker.side {
                    Side::Bid => taker.quote_escrow -= buyer_cost,
                    Side::Ask => maker.quote_escrow -= buyer_cost,
                }
                taker.filled_amount += trade_amount;
                maker.filled_amount += trade_amount;

                let maker_remaining = maker.amount - maker.filled_amount;
                if maker_remaining == 0 {
                    maker.status = OrderStatus::Filled;
                    Self::refund_escrow(env, &mut maker, ctx);
                } else if Self::notional(env, maker_remaining, maker.price) == 0 {
                    // Remainder can never trade: refund it instead of leaving dust queued.
                    Self::close_order(env, &mut maker, ctx);
                } else {
                    maker.status = OrderStatus::PartiallyFilled;
                    kept.push_back(maker_id);
                }
                Self::put(env, &DataKey::Order(maker_id), &maker);

                Self::accumulate_volume(env, &taker.trader, trade_quote);
                Self::accumulate_volume(env, &maker.trader, trade_quote);

                env.events().publish(
                    (symbol_short!("trade"), taker.trader.clone()),
                    TradeExecution {
                        order_id: taker.id,
                        matched_order_id: maker_id,
                        maker: maker.trader.clone(),
                        taker: taker.trader.clone(),
                        side: taker.side,
                        price: maker_price,
                        fill_amount: trade_amount,
                        quote_amount: trade_quote,
                        buyer_fee,
                        seller_fee,
                        timestamp: ctx.now,
                    },
                );
            }

            while queue_idx < queue.len() {
                kept.push_back(queue.get(queue_idx).unwrap());
                queue_idx += 1;
            }

            if kept.is_empty() {
                env.storage().persistent().remove(&queue_key);
                ladder.remove(ladder_idx);
            } else {
                Self::put(env, &queue_key, &kept);
                ladder_idx += 1;
            }
        }

        Self::put(env, &DataKey::PriceLadder(opposite_side), &ladder);
        close_remainder
    }

    fn context(env: &Env) -> MatchContext {
        let instance = env.storage().instance();
        MatchContext {
            base_token: instance.get(&DataKey::BaseToken).unwrap(),
            quote_token: instance.get(&DataKey::QuoteToken).unwrap(),
            fee_config: instance.get(&DataKey::FeeConfig).unwrap(),
            holding_period: Self::holding_period(env),
            now: env.ledger().timestamp(),
        }
    }

    /// Refund whatever an order still holds in escrow and mark it `Cancelled`.
    /// The caller persists the order and takes it off the book.
    fn close_order(env: &Env, order: &mut Order, ctx: &MatchContext) {
        if order.side == Side::Ask {
            let unfilled = order.amount - order.filled_amount;
            if unfilled > 0 {
                token::Client::new(env, &ctx.base_token).transfer(
                    &env.current_contract_address(),
                    &order.trader,
                    &unfilled,
                );
            }
        }
        Self::refund_escrow(env, order, ctx);
        order.status = OrderStatus::Cancelled;
    }

    /// Return any quote escrow a bid still holds.
    fn refund_escrow(env: &Env, order: &mut Order, ctx: &MatchContext) {
        if order.quote_escrow > 0 {
            token::Client::new(env, &ctx.quote_token).transfer(
                &env.current_contract_address(),
                &order.trader,
                &order.quote_escrow,
            );
            order.quote_escrow = 0;
        }
    }

    /// Cancel a resting order whose trader no longer passes the trade gates.
    fn cancel_ineligible(env: &Env, order: &mut Order, ctx: &MatchContext, reason: Symbol) {
        Self::close_order(env, order, ctx);
        Self::put(env, &DataKey::Order(order.id), order);
        env.events().publish(
            (symbol_short!("auto_cxl"), order.trader.clone()),
            (order.id, reason),
        );
    }

    fn insert_into_orderbook(env: &Env, side: Side, price: i128, order_id: u64) {
        let queue_key = DataKey::PriceQueue(side, price);
        let mut queue: Vec<u64> = Self::get(env, &queue_key).unwrap_or(Vec::new(env));

        if queue.len() >= MAX_ORDERS_PER_LEVEL {
            panic_with_error!(env, Error::PriceLevelFull);
        }

        let is_new_level = queue.is_empty();
        queue.push_back(order_id);
        Self::put(env, &queue_key, &queue);

        if is_new_level {
            let mut ladder = Self::ladder(env, side);
            if ladder.len() >= MAX_PRICE_LEVELS {
                panic_with_error!(env, Error::PriceLadderFull);
            }

            // Bids: descending (highest price first). Asks: ascending (lowest first).
            let position = ladder
                .iter()
                .position(|p| match side {
                    Side::Bid => price > p,
                    Side::Ask => price < p,
                })
                .unwrap_or(ladder.len() as usize);
            ladder.insert(position as u32, price);
            Self::put(env, &DataKey::PriceLadder(side), &ladder);
        }
    }

    fn remove_from_orderbook(env: &Env, side: Side, price: i128, order_id: u64) {
        let queue_key = DataKey::PriceQueue(side, price);
        let queue: Vec<u64> = match Self::get(env, &queue_key) {
            Some(q) => q,
            None => return,
        };

        let mut new_queue = Vec::new(env);
        for oid in queue.iter() {
            if oid != order_id {
                new_queue.push_back(oid);
            }
        }

        if new_queue.is_empty() {
            env.storage().persistent().remove(&queue_key);
            let mut ladder = Self::ladder(env, side);
            if let Some(idx) = ladder.first_index_of(price) {
                ladder.remove(idx);
            }
            Self::put(env, &DataKey::PriceLadder(side), &ladder);
        } else {
            Self::put(env, &queue_key, &new_queue);
        }
    }

    fn accumulate_volume(env: &Env, trader: &Address, quote_amount: i128) {
        let key = DataKey::TraderVolume(trader.clone());
        let current: i128 = Self::get(env, &key).unwrap_or(0);
        Self::put(env, &key, &current.saturating_add(quote_amount));
    }

    fn ladder(env: &Env, side: Side) -> Vec<i128> {
        Self::get(env, &DataKey::PriceLadder(side)).unwrap_or(Vec::new(env))
    }

    fn holding_period(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::HoldingPeriodSeconds)
            .unwrap_or(DEFAULT_RULE_144_HOLDING_PERIOD)
    }

    /// A seller with no attested acquisition date never meets the holding period.
    fn holding_met(acquisition: Option<u64>, holding_period: u64, now: u64) -> bool {
        match acquisition {
            Some(acquired_at) => now >= acquired_at.saturating_add(holding_period),
            None => false,
        }
    }

    fn is_active(status: OrderStatus) -> bool {
        status == OrderStatus::Open || status == OrderStatus::PartiallyFilled
    }

    /// Quote value of `amount` base units at `price`, rounded down.
    fn notional(env: &Env, amount: i128, price: i128) -> i128 {
        amount
            .checked_mul(price)
            .unwrap_or_else(|| panic_with_error!(env, Error::ArithmeticOverflow))
            / PRICE_PRECISION
    }

    /// Fee of `bps` on `quote`, rounded down.
    fn fee_on(env: &Env, quote: i128, bps: i128) -> i128 {
        quote
            .checked_mul(bps)
            .unwrap_or_else(|| panic_with_error!(env, Error::ArithmeticOverflow))
            / BPS_DIVISOR
    }

    /// Quote escrow a bid of `amount` at `price` needs: notional plus fee at `fee_bps`.
    /// Because both terms round down, the escrow of a whole order always covers the
    /// sum of its fills plus the reserve for any remainder.
    fn bid_reserve(env: &Env, amount: i128, price: i128, fee_bps: i128) -> i128 {
        let notional = Self::notional(env, amount, price);
        notional + Self::fee_on(env, notional, fee_bps)
    }

    /// Persistent write that also extends the entry's TTL so it is not archived.
    fn put<V: IntoVal<Env, Val>>(env: &Env, key: &DataKey, value: &V) {
        let storage = env.storage().persistent();
        storage.set(key, value);
        storage.extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Persistent read that extends the entry's TTL when it exists.
    fn get<V>(env: &Env, key: &DataKey) -> Option<V>
    where
        V: TryFromVal<Env, Val>,
        V::Error: core::fmt::Debug,
    {
        let storage = env.storage().persistent();
        let value = storage.get(key);
        if value.is_some() {
            storage.extend_ttl(key, TTL_THRESHOLD, TTL_EXTEND_TO);
        }
        value
    }

    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn validate_fee_config(env: &Env, config: &DynamicFeeConfig) {
        if config.maker_fee_bps < 0
            || config.taker_fee_bps < 0
            || config.maker_fee_bps > MAX_FEE_BPS
            || config.taker_fee_bps > MAX_FEE_BPS
            || config.volume_discount_bps < 0
            || config.volume_discount_bps > config.taker_fee_bps
            || config.volume_discount_threshold < 0
        {
            panic_with_error!(env, Error::InvalidFeeConfig);
        }
    }

    fn validate_holding_period(env: &Env, holding_period: u64) {
        if holding_period < DEFAULT_RULE_144_HOLDING_PERIOD {
            panic_with_error!(env, Error::InvalidHoldingPeriod);
        }
    }

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    fn require_not_paused(env: &Env) {
        if tessera_common::is_paused(env) {
            panic_with_error!(env, Error::Paused);
        }
    }
}
