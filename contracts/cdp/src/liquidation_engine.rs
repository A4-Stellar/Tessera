//! Deterministic Dutch-auction and bad-debt accounting primitives for CDPs.
use soroban_sdk::{contracttype, Env};

const SCALE: i128 = 10_000;

#[contracttype]
#[derive(Clone)]
pub struct AuctionQuote {
    pub price_bps: i128,
    pub collateral_out: i128,
    pub debt_covered: i128,
    pub bad_debt: i128,
}

/// P(t) = Pstart * (1 - t/T)^2, rounded down for the bidder.
pub fn dutch_price_bps(elapsed: u64, duration: u64, start_price_bps: i128) -> i128 {
    if duration == 0 { panic!("auction duration must be non-zero"); }
    if start_price_bps <= 0 { panic!("auction start price must be positive"); }
    if elapsed >= duration { return 0; }
    let remaining = i128::from(duration - elapsed);
    start_price_bps.saturating_mul(remaining).saturating_mul(remaining)
        / i128::from(duration).saturating_mul(i128::from(duration))
}

pub fn quote(elapsed: u64, duration: u64, start_price_bps: i128, collateral: i128, debt: i128) -> AuctionQuote {
    let price_bps = dutch_price_bps(elapsed, duration, start_price_bps);
    let collateral_out = if price_bps == 0 { collateral } else { (debt.saturating_mul(SCALE) / price_bps).min(collateral) };
    let debt_covered = collateral_out.saturating_mul(price_bps) / SCALE;
    AuctionQuote { price_bps, collateral_out, debt_covered, bad_debt: debt.saturating_sub(debt_covered) }
}

pub fn assert_allowlisted(env: &Env, allowed: bool) { if !allowed { panic!("liquidator is not compliance allowlisted"); } }
