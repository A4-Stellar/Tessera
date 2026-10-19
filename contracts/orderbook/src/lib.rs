#![no_std]

pub mod restricted_dex;

#[cfg(test)]
mod test;

pub use restricted_dex::{
    DataKey, DynamicFeeConfig, Error, Order, OrderStatus, RestrictedOrderbookDex,
    RestrictedOrderbookDexClient, Side, TradeExecution, DEFAULT_RULE_144_HOLDING_PERIOD,
    MAX_FEE_BPS, MAX_FILLS_PER_ORDER, MAX_ORDERS_PER_LEVEL, MAX_PRICE_LEVELS,
    NON_REPORTING_ISSUER_HOLDING_PERIOD, PRICE_PRECISION, TTL_EXTEND_TO,
};
