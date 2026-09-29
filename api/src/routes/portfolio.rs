//! Automated Asset Portfolio Performance & Return Tracking API (issue #162).
//!
//! Backs `GET /v1/holders/:address/portfolio` by aggregating token balances,
//! historical market values, and dividend payouts across an investor's holdings,
//! and computing formal financial performance metrics:
//!
//! * **Time-Weighted Return (TWR)**: Measures compound rate of growth across
//!   sub-periods, isolating investment performance from the distorting effects
//!   of cash inflows (deposits/purchases) and cash outflows (withdrawals/sales).
//! * **Net Present Value (NPV)**: Discounted sum of all past net cash flows
//!   and terminal portfolio valuation at a user-specified annual discount rate.
//! * **Internal Rate of Return (IRR)**: The annualized discount rate at which
//!   the NPV of all cash flows equals zero, solved via bounded Newton-Raphson
//!   with bisection fallback.
//! * **Structured Time Series**: Chronological valuation and cash flow points
//!   formatted for chart visualization.
//!
//! # Mathematical Specification
//!
//! ## Time-Weighted Return (TWR)
//! For evaluation intervals $i = 1, \dots, n$ with beginning value $V_{i-1}$,
//! external cash flow $C_i$ (positive for deposit, negative for withdrawal),
//! and ending value before flow $V_i^{\text{pre}}$:
//!
//! $$r_i = \frac{V_i^{\text{pre}} - V_{i-1}}{V_{i-1} + C_{i-1}}$$
//!
//! $$\text{TWR} = \prod_{i=1}^n (1 + r_i) - 1$$
//!
//! ## Net Present Value (NPV)
//! With time in fractional years $w_k = \frac{\Delta t_k}{365.25}$ from origin $t_0$,
//! cash flows $CF_k$ (negative for investment outlays, positive for dividend/liquidation proceeds):
//!
//! $$\text{NPV}(r) = \sum_{k=0}^M \frac{CF_k}{(1 + r)^{w_k}}$$
//!
//! ## Internal Rate of Return (IRR)
//! Solves $f(r) = \text{NPV}(r) = 0$ using:
//!
//! $$f'(r) = -\sum_{k=0}^M \frac{w_k \cdot CF_k}{(1 + r)^{w_k + 1}}$$
//!
//! # Complexity
//!
//! * **Time Complexity**: $O(A + E + N \log N)$ where $A$ is the number of assets,
//!   $E$ is the event log size, and $N$ is the count of portfolio timeline events.
//!   Sorting events takes $O(N \log N)$; computing TWR, NPV, and bounded IRR each
//!   runs in linear time $O(N)$ with at most 100 bounded iterations for IRR.
//! * **Space Complexity**: $O(A + N)$ auxiliary memory to aggregate holdings and
//!   timeline points without redundant allocations.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ApiError;
use crate::indexer::{AppState, Snapshot};

/// Default annualized discount rate for Net Present Value (5.0%).
pub const DEFAULT_DISCOUNT_RATE: f64 = 0.05;

/// Query parameters for `GET /v1/holders/:address/portfolio`.
#[derive(Debug, Deserialize, Default)]
pub struct PortfolioQuery {
    /// RFC3339 timestamp filter: only include activity on or after this timestamp.
    #[serde(default)]
    pub start_date: Option<String>,
    /// RFC3339 timestamp filter: only include activity on or before this timestamp.
    #[serde(default)]
    pub end_date: Option<String>,
    /// Annualized discount rate used for Net Present Value calculation (e.g. 0.05 for 5%).
    #[serde(default)]
    pub discount_rate: Option<f64>,
}

/// Comprehensive portfolio performance response for an investor address.
#[derive(Debug, Clone, Serialize)]
pub struct HolderPortfolioResponse {
    /// Evaluated investor account address.
    pub address: String,
    /// High-level portfolio summary metrics.
    pub summary: PortfolioSummary,
    /// Detailed breakdown of each tokenized asset held.
    pub holdings: Vec<PortfolioHoldingDetail>,
    /// Dividend payouts received by this address.
    pub dividends: Vec<PortfolioDividendPayout>,
    /// Financial rate of return analytics (TWR, NPV, IRR).
    pub performance: PortfolioPerformance,
    /// Structured time series formatted for charting (e.g., area/line charts).
    pub time_series: Vec<PortfolioTimeSeriesPoint>,
}

/// Summary of investor's portfolio valuation and cumulative capital flows.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioSummary {
    /// Current aggregate market value of all held assets in USD.
    pub total_valuation_usd: f64,
    /// Count of distinct active asset holdings.
    pub total_assets_held: usize,
    /// Cumulative capital inflows (deposits / purchases) in USD.
    pub total_cash_inflows_usd: f64,
    /// Cumulative capital outflows (withdrawals / transfers out) in USD.
    pub total_cash_outflows_usd: f64,
    /// Net invested capital: `total_cash_inflows - total_cash_outflows`.
    pub net_invested_capital_usd: f64,
    /// Cumulative dividends claimed / credited in USD.
    pub total_dividends_usd: f64,
    /// Total unrealized return in USD: `total_valuation + total_dividends - net_invested_capital`.
    pub total_unrealized_gain_usd: f64,
}

/// Per-asset breakdown in investor portfolio.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioHoldingDetail {
    pub asset_id: u64,
    pub asset_name: String,
    pub symbol: String,
    pub asset_type: String,
    /// Balance in raw token base units.
    pub balance: String,
    pub decimals: u32,
    /// Share percentage of asset's circulating supply (0–100%).
    pub share_percent: f64,
    /// Implied unit price in USD derived from contract valuation and supply.
    pub unit_price_usd: f64,
    /// Current holding valuation in USD: `balance_units * unit_price_usd`.
    pub market_value_usd: f64,
    /// Cumulative dividends received specifically from this asset in USD.
    pub dividends_received_usd: f64,
}

/// A dividend distribution credited to or claimed by the investor.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioDividendPayout {
    pub distribution_id: u64,
    pub asset_id: u64,
    pub payment_token: String,
    pub amount: String,
    pub amount_usd: f64,
    pub claimed_at_ledger: u32,
    pub timestamp: Option<String>,
}

/// Advanced financial return metrics computed for the portfolio.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioPerformance {
    /// Time-Weighted Return (TWR) as a decimal ratio (e.g., 0.1525 for 15.25%).
    pub time_weighted_return: f64,
    /// Time-Weighted Return formatted as a percentage (e.g., 15.25).
    pub time_weighted_return_pct: f64,
    /// Net Present Value (NPV) in USD, discounted at `discount_rate`.
    pub net_present_value: f64,
    /// Internal Rate of Return (IRR), annualized decimal. `None` if cash flows do not converge.
    pub internal_rate_of_return: Option<f64>,
    /// Annualized IRR percentage (e.g., 18.42).
    pub internal_rate_of_return_pct: Option<f64>,
    /// Annual discount rate used for NPV calculation.
    pub discount_rate: f64,
}

/// Data point in the chronological portfolio performance timeline for charting.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioTimeSeriesPoint {
    /// ISO 8601 / RFC3339 timestamp.
    pub timestamp: String,
    /// Stellar ledger sequence number.
    pub ledger: u32,
    /// Total portfolio market valuation at this step in USD.
    pub portfolio_value_usd: f64,
    /// Net cash flow at this point (inflow positive, outflow negative).
    pub cash_flow_usd: f64,
    /// Cumulative net capital invested up to this point.
    pub cumulative_invested_usd: f64,
    /// Cumulative dividends received up to this point.
    pub cumulative_dividends_usd: f64,
    /// Sub-period return between previous point and this point.
    pub period_return: f64,
    /// Cumulative Time-Weighted Return up to this point (percentage).
    pub cumulative_twr_pct: f64,
}

/// Internal chronological timeline entry used to drive TWR, NPV, and IRR calculations.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub timestamp_secs: i64,
    pub timestamp_rfc3339: String,
    pub ledger: u32,
    pub cash_inflow_usd: f64,
    pub cash_outflow_usd: f64,
    pub dividend_usd: f64,
    pub portfolio_value_usd: f64,
}

/// Main handler for `GET /v1/holders/:address/portfolio`.
pub async fn get_portfolio(
    State(state): State<AppState>,
    Path(address): Path<String>,
    Query(query): Query<PortfolioQuery>,
) -> Result<Json<HolderPortfolioResponse>, ApiError> {
    let snap = state.snapshot();
    let discount_rate = query.discount_rate.unwrap_or(DEFAULT_DISCOUNT_RATE);
    if discount_rate <= -1.0 || discount_rate > 10.0 {
        return Err(ApiError::BadRequest(
            "discount_rate must be greater than -1.0 and at most 10.0".to_string(),
        ));
    }

    let response = calculate_portfolio(
        &snap,
        &address,
        query.start_date.as_deref(),
        query.end_date.as_deref(),
        discount_rate,
    );

    Ok(Json(response))
}

/// Pure computation engine for portfolio analytics. Separated from Axum handlers
/// to facilitate direct unit testing and algorithmic verification.
pub fn calculate_portfolio(
    snap: &Snapshot,
    address: &str,
    start_date: Option<&str>,
    end_date: Option<&str>,
    discount_rate: f64,
) -> HolderPortfolioResponse {
    let start_ts = start_date
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp());
    let end_ts = end_date
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp());

    // 1. Gather all current holdings for this address across all assets.
    let mut holdings = Vec::new();
    let mut total_valuation_usd = 0.0;

    for asset in &snap.assets {
        let holders_for_asset = snap.holders.get(&asset.id);
        let holder = holders_for_asset.and_then(|list| list.iter().find(|h| h.address == address));

        let balance_str = holder.map(|h| h.balance.as_str()).unwrap_or("0");
        let share_percent = holder.map(|h| h.share_percent).unwrap_or(0.0);
        let balance_num = balance_str.parse::<f64>().unwrap_or(0.0);

        if balance_num <= 0.0 && holder.is_none() {
            continue;
        }

        // Calculate unit price: valuation_usd / (total_supply / 10^decimals)
        let total_supply_num = asset.total_supply.parse::<f64>().unwrap_or(0.0);
        let base_factor = 10_f64.powi(asset.decimals as i32);
        let normalized_supply = if total_supply_num > 0.0 {
            total_supply_num / base_factor
        } else {
            0.0
        };

        let unit_price_usd = if normalized_supply > 0.0 {
            asset.valuation_usd / normalized_supply
        } else {
            0.0
        };

        let normalized_balance = balance_num / base_factor;
        let market_value_usd = normalized_balance * unit_price_usd;

        // Sum historical dividends for this address and asset
        let asset_dividends_usd = compute_holder_dividends_for_asset(snap, address, asset.id);

        if balance_num > 0.0 {
            total_valuation_usd += market_value_usd;
            holdings.push(PortfolioHoldingDetail {
                asset_id: asset.id,
                asset_name: asset.name.clone(),
                symbol: asset.symbol.clone(),
                asset_type: asset.asset_type.clone(),
                balance: balance_str.to_string(),
                decimals: asset.decimals,
                share_percent,
                unit_price_usd,
                market_value_usd,
                dividends_received_usd: asset_dividends_usd,
            });
        }
    }

    // Sort holdings by market value descending
    holdings.sort_by(|a, b| {
        b.market_value_usd
            .partial_cmp(&a.market_value_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 2. Gather dividends received by this address
    let dividends = collect_holder_dividends(snap, address);

    // 3. Build chronological timeline of transactions and events for this address
    let mut timeline = build_portfolio_timeline(snap, address, &holdings, total_valuation_usd);

    // Filter timeline by start_date and end_date if supplied
    if let Some(s) = start_ts {
        timeline.retain(|entry| entry.timestamp_secs >= s);
    }
    if let Some(e) = end_ts {
        timeline.retain(|entry| entry.timestamp_secs <= e);
    }

    // 4. Compute financial rate of return analytics (TWR, NPV, IRR)
    let (twr, time_series) = compute_time_weighted_return(&timeline);
    let (npv, irr) = compute_npv_and_irr(&timeline, total_valuation_usd, discount_rate);

    let mut total_cash_inflows = 0.0;
    let mut total_cash_outflows = 0.0;
    let mut total_dividends = 0.0;

    for point in &timeline {
        total_cash_inflows += point.cash_inflow_usd;
        total_cash_outflows += point.cash_outflow_usd;
        total_dividends += point.dividend_usd;
    }

    let net_invested_capital = (total_cash_inflows - total_cash_outflows).max(0.0);
    let total_unrealized_gain = total_valuation_usd + total_dividends - net_invested_capital;

    let summary = PortfolioSummary {
        total_valuation_usd,
        total_assets_held: holdings.len(),
        total_cash_inflows_usd: total_cash_inflows,
        total_cash_outflows_usd: total_cash_outflows,
        net_invested_capital_usd: net_invested_capital,
        total_dividends_usd: total_dividends,
        total_unrealized_gain_usd: total_unrealized_gain,
    };

    let performance = PortfolioPerformance {
        time_weighted_return: twr,
        time_weighted_return_pct: (twr * 100.0 * 100.0).round() / 100.0,
        net_present_value: (npv * 100.0).round() / 100.0,
        internal_rate_of_return: irr,
        internal_rate_of_return_pct: irr.map(|r| (r * 100.0 * 100.0).round() / 100.0),
        discount_rate,
    };

    HolderPortfolioResponse {
        address: address.to_string(),
        summary,
        holdings,
        dividends,
        performance,
        time_series,
    }
}

/// Calculate dividend earnings for an address on a specific asset.
fn compute_holder_dividends_for_asset(snap: &Snapshot, address: &str, asset_id: u64) -> f64 {
    let mut total = 0.0;
    if let Some(distributions) = snap.dividends.get(&asset_id) {
        let share = snap
            .holders
            .get(&asset_id)
            .and_then(|list| list.iter().find(|h| h.address == address))
            .map(|h| h.share_percent / 100.0)
            .unwrap_or(0.0);

        for dist in distributions {
            let dist_usd = dist.fiat_equivalent_usd.unwrap_or_else(|| {
                dist.total_amount
                    .parse::<f64>()
                    .map(|v| v / 10_f64.powi(7))
                    .unwrap_or(0.0)
            });
            total += dist_usd * share;
        }
    }
    total
}

/// Collect all dividend payouts attributed to the investor address.
fn collect_holder_dividends(snap: &Snapshot, address: &str) -> Vec<PortfolioDividendPayout> {
    let mut payouts = Vec::new();

    for (asset_id, distributions) in &snap.dividends {
        let share = snap
            .holders
            .get(asset_id)
            .and_then(|list| list.iter().find(|h| h.address == address))
            .map(|h| h.share_percent / 100.0)
            .unwrap_or(0.0);

        if share <= 0.0 {
            continue;
        }

        for dist in distributions {
            let amount_raw = dist.total_amount.parse::<f64>().unwrap_or(0.0);
            let user_amount = amount_raw * share;
            let user_amount_usd = dist
                .fiat_equivalent_usd
                .map(|fiat| fiat * share)
                .unwrap_or_else(|| user_amount / 10_f64.powi(7));

            payouts.push(PortfolioDividendPayout {
                distribution_id: dist.id,
                asset_id: *asset_id,
                payment_token: dist.payment_token.clone(),
                amount: format!("{:.0}", user_amount),
                amount_usd: (user_amount_usd * 100.0).round() / 100.0,
                claimed_at_ledger: dist.created_at_ledger,
                timestamp: None,
            });
        }
    }

    payouts.sort_by_key(|p| p.claimed_at_ledger);
    payouts
}

/// Builds a structured chronological timeline of events for the holder:
/// transfers in (inflows), transfers out (outflows), dividends, and asset valuation adjustments.
fn build_portfolio_timeline(
    snap: &Snapshot,
    address: &str,
    holdings: &[PortfolioHoldingDetail],
    current_valuation_usd: f64,
) -> Vec<TimelineEntry> {
    let mut entries = Vec::new();

    // 1. Process contract events involving this address
    for event in &snap.events {
        let ty = event.event_type.to_ascii_lowercase();
        let from = event.data.get("from").and_then(|v| v.as_str());
        let to = event.data.get("to").and_then(|v| v.as_str());
        let holder = event.data.get("holder").and_then(|v| v.as_str());

        let is_inflow = to == Some(address) || (holder == Some(address) && ty == "mint");
        let is_outflow = from == Some(address);

        if !is_inflow && !is_outflow {
            continue;
        }

        let amount_num = event
            .data
            .get("amount")
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| v.as_f64())
            })
            .unwrap_or(0.0);

        // Approximate price from matching asset if known
        let matching_asset = snap
            .assets
            .iter()
            .find(|a| a.token_contract == event.contract);
        let unit_price = matching_asset
            .map(|a| {
                let supply = a.total_supply.parse::<f64>().unwrap_or(0.0);
                if supply > 0.0 {
                    a.valuation_usd / (supply / 10_f64.powi(a.decimals as i32))
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0);

        let decimals = matching_asset.map(|a| a.decimals).unwrap_or(7);
        let amount_usd = (amount_num / 10_f64.powi(decimals as i32)) * unit_price;

        let ts = event
            .timestamp
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or_else(|| (event.ledger as i64) * 5);

        let rfc3339 = event
            .timestamp
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        entries.push(TimelineEntry {
            timestamp_secs: ts,
            timestamp_rfc3339: rfc3339,
            ledger: event.ledger,
            cash_inflow_usd: if is_inflow { amount_usd } else { 0.0 },
            cash_outflow_usd: if is_outflow { amount_usd } else { 0.0 },
            dividend_usd: 0.0,
            portfolio_value_usd: 0.0,
        });
    }

    // 2. Process dividend distribution events as cash flow points
    for (asset_id, dist_list) in &snap.dividends {
        let share = snap
            .holders
            .get(asset_id)
            .and_then(|list| list.iter().find(|h| h.address == address))
            .map(|h| h.share_percent / 100.0)
            .unwrap_or(0.0);

        if share <= 0.0 {
            continue;
        }

        for dist in dist_list {
            let amount_raw = dist.total_amount.parse::<f64>().unwrap_or(0.0);
            let dist_usd = dist
                .fiat_equivalent_usd
                .map(|v| v * share)
                .unwrap_or_else(|| (amount_raw * share) / 10_f64.powi(7));

            let ts = (dist.created_at_ledger as i64) * 5;
            entries.push(TimelineEntry {
                timestamp_secs: ts,
                timestamp_rfc3339: Utc::now().to_rfc3339(),
                ledger: dist.created_at_ledger,
                cash_inflow_usd: 0.0,
                cash_outflow_usd: 0.0,
                dividend_usd: dist_usd,
                portfolio_value_usd: 0.0,
            });
        }
    }

    // Sort all events chronologically
    entries.sort_by_key(|e| (e.ledger, e.timestamp_secs));

    // If no events exist but current holdings exist, synthesize initial entry
    if entries.is_empty() && !holdings.is_empty() {
        let now_sec = Utc::now().timestamp();
        entries.push(TimelineEntry {
            timestamp_secs: now_sec - 86400 * 30, // 30 days ago
            timestamp_rfc3339: (Utc::now() - chrono::Duration::days(30)).to_rfc3339(),
            ledger: snap.stats.last_indexed_ledger.saturating_sub(1000),
            cash_inflow_usd: current_valuation_usd,
            cash_outflow_usd: 0.0,
            dividend_usd: 0.0,
            portfolio_value_usd: current_valuation_usd,
        });
    }

    // 3. Compute running valuation across timeline
    let mut running_val = 0.0;
    for entry in &mut entries {
        running_val += entry.cash_inflow_usd - entry.cash_outflow_usd;
        if running_val < 0.0 {
            running_val = 0.0;
        }
        entry.portfolio_value_usd = running_val;
    }

    // Ensure the terminal point reflects current valuation
    if let Some(last) = entries.last_mut() {
        if current_valuation_usd > 0.0 {
            last.portfolio_value_usd = current_valuation_usd;
        }
    } else {
        // Entirely empty portfolio
        let now = Utc::now();
        entries.push(TimelineEntry {
            timestamp_secs: now.timestamp(),
            timestamp_rfc3339: now.to_rfc3339(),
            ledger: snap.stats.last_indexed_ledger,
            cash_inflow_usd: 0.0,
            cash_outflow_usd: 0.0,
            dividend_usd: 0.0,
            portfolio_value_usd: 0.0,
        });
    }

    entries
}

/// Compute Time-Weighted Return (TWR) taking into account cash inflows and outflows.
///
/// Returns `(twr_ratio, time_series_points)`.
///
/// Each sub-period return is:
/// $$R_i = \frac{V_i^{\text{pre}} - V_{i-1}}{V_{i-1} + C_{i-1}}$$
/// where $C_{i-1}$ is cash flow occurring at beginning of sub-period.
pub fn compute_time_weighted_return(
    timeline: &[TimelineEntry],
) -> (f64, Vec<PortfolioTimeSeriesPoint>) {
    if timeline.is_empty() {
        return (0.0, Vec::new());
    }

    let mut points = Vec::with_capacity(timeline.len());
    let mut cumulative_twr_factor = 1.0;
    let mut cumulative_invested = 0.0;
    let mut cumulative_dividends = 0.0;
    let mut prev_value = 0.0;

    for (i, entry) in timeline.iter().enumerate() {
        let net_cash_flow = entry.cash_inflow_usd - entry.cash_outflow_usd;
        cumulative_invested += net_cash_flow;
        cumulative_dividends += entry.dividend_usd;

        let period_return = if i == 0 {
            0.0
        } else {
            let base = prev_value;
            if base > 0.0 {
                // Return on capital before new external flow is injected
                let pre_flow_value = (entry.portfolio_value_usd - net_cash_flow).max(0.0);
                (pre_flow_value - base) / base
            } else if entry.portfolio_value_usd > 0.0 && net_cash_flow > 0.0 {
                0.0
            } else {
                0.0
            }
        };

        cumulative_twr_factor *= 1.0 + period_return;
        prev_value = entry.portfolio_value_usd;

        let cum_twr_pct = ((cumulative_twr_factor - 1.0) * 100.0 * 100.0).round() / 100.0;

        points.push(PortfolioTimeSeriesPoint {
            timestamp: entry.timestamp_rfc3339.clone(),
            ledger: entry.ledger,
            portfolio_value_usd: (entry.portfolio_value_usd * 100.0).round() / 100.0,
            cash_flow_usd: (net_cash_flow * 100.0).round() / 100.0,
            cumulative_invested_usd: (cumulative_invested.max(0.0) * 100.0).round() / 100.0,
            cumulative_dividends_usd: (cumulative_dividends * 100.0).round() / 100.0,
            period_return: (period_return * 10000.0).round() / 10000.0,
            cumulative_twr_pct: cum_twr_pct,
        });
    }

    let final_twr = cumulative_twr_factor - 1.0;
    (final_twr, points)
}

/// Compute Net Present Value (NPV) and Internal Rate of Return (IRR).
///
/// Cash flows are dated relative to the first event $t_0$.
/// Capital inflows (investor outlays) are negative cash flows.
/// Dividends and terminal portfolio value are positive cash flows.
pub fn compute_npv_and_irr(
    timeline: &[TimelineEntry],
    terminal_value_usd: f64,
    discount_rate: f64,
) -> (f64, Option<f64>) {
    if timeline.is_empty() {
        return (0.0, None);
    }

    let t0 = timeline[0].timestamp_secs;
    let mut dated_flows: Vec<(f64, f64)> = Vec::new(); // (years_elapsed, net_flow_to_investor)

    for entry in timeline {
        let years = ((entry.timestamp_secs - t0) as f64) / (365.25 * 86400.0);
        // From investor's perspective: inflow to portfolio = cash outflow from investor wallet
        let net_flow = entry.dividend_usd + entry.cash_outflow_usd - entry.cash_inflow_usd;
        if net_flow.abs() > 0.001 {
            dated_flows.push((years.max(0.0), net_flow));
        }
    }

    // Add terminal portfolio liquidation value at last timestamp
    let last_years = ((timeline.last().unwrap().timestamp_secs - t0) as f64) / (365.25 * 86400.0);
    if terminal_value_usd > 0.0 {
        dated_flows.push((last_years.max(0.0), terminal_value_usd));
    }

    if dated_flows.is_empty() {
        return (0.0, None);
    }

    let npv = calculate_npv(&dated_flows, discount_rate);
    let irr = calculate_irr(&dated_flows);

    (npv, irr)
}

/// Calculate Net Present Value at a given annual discount rate:
/// $$\text{NPV}(r) = \sum \frac{CF_k}{(1 + r)^{t_k}}$$
pub fn calculate_npv(dated_flows: &[(f64, f64)], rate: f64) -> f64 {
    if rate <= -1.0 {
        return 0.0;
    }
    let mut npv = 0.0;
    for &(t, cf) in dated_flows {
        let discount = (1.0 + rate).powf(t);
        if discount.is_finite() && discount > 1e-12 {
            npv += cf / discount;
        }
    }
    npv
}

/// Calculate Internal Rate of Return (IRR) via bounded Newton-Raphson with bisection fallback.
///
/// Solves $f(r) = \text{NPV}(r) = 0$.
///
/// Guaranteed $O(N)$ execution with a hard limit of 100 iterations and $O(1)$ space.
pub fn calculate_irr(dated_flows: &[(f64, f64)]) -> Option<f64> {
    // Check for sign change in cash flows: without both positive and negative flows, no real IRR exists.
    let mut has_positive = false;
    let mut has_negative = false;
    for &(_, cf) in dated_flows {
        if cf > 1e-5 {
            has_positive = true;
        } else if cf < -1e-5 {
            has_negative = true;
        }
    }

    if !has_positive || !has_negative {
        return None;
    }

    // Objective function and its derivative
    let f = |r: f64| -> f64 {
        if r <= -0.9999 {
            return f64::MAX;
        }
        let mut sum = 0.0;
        for &(t, cf) in dated_flows {
            let denom = (1.0 + r).powf(t);
            if denom.is_finite() && denom > 1e-12 {
                sum += cf / denom;
            }
        }
        sum
    };

    let df = |r: f64| -> f64 {
        if r <= -0.9999 {
            return f64::MIN;
        }
        let mut sum = 0.0;
        for &(t, cf) in dated_flows {
            let denom = (1.0 + r).powf(t + 1.0);
            if denom.is_finite() && denom > 1e-12 {
                sum -= t * cf / denom;
            }
        }
        sum
    };

    // Phase 1: Newton-Raphson with initial guess r = 0.10 (10%)
    let mut r = 0.10;
    let max_iter = 100;
    let tol = 1e-7;

    for _ in 0..max_iter {
        let val = f(r);
        if val.abs() < tol {
            return Some(r);
        }
        let deriv = df(r);
        if deriv.abs() < 1e-12 {
            break; // Flat slope, switch to bisection
        }

        let step = val / deriv;
        let next_r = r - step;

        // Keep within plausible bounds [-0.95, 10.0]
        if next_r <= -0.95 || next_r >= 10.0 || (next_r - r).abs() < tol {
            if next_r > -0.95 && next_r < 10.0 {
                return Some(next_r);
            }
            break; // Step out of bounds, switch to bisection
        }
        r = next_r;
    }

    // Phase 2: Robust Bisection Fallback in [-0.90, 5.0]
    let mut low = -0.90;
    let mut high = 5.0;
    let f_low = f(low);
    let f_high = f(high);

    if f_low * f_high > 0.0 {
        // Signs do not bracket zero in the search interval
        return None;
    }

    for _ in 0..60 {
        let mid = 0.5 * (low + high);
        let f_mid = f(mid);

        if f_mid.abs() < tol || (high - low).abs() < tol {
            return Some(mid);
        }

        if f_low * f_mid < 0.0 {
            high = mid;
        } else {
            low = mid;
        }
    }

    Some(0.5 * (low + high))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Holder;
    use crate::routes::test_support::{asset as test_asset, state_with};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use std::collections::HashMap;
    use tower::ServiceExt as _;

    #[test]
    fn test_npv_known_cash_flows() {
        // Initial investment: -$1,000 at t=0
        // Year 1 cash flow: +$400 at t=1.0
        // Year 2 cash flow: +$800 at t=2.0
        // At 10% discount rate (0.10):
        // NPV = -1000 + 400/(1.1) + 800/(1.1^2) = -1000 + 363.636 + 661.157 = 24.793
        let flows = vec![(0.0, -1000.0), (1.0, 400.0), (2.0, 800.0)];
        let npv = calculate_npv(&flows, 0.10);
        assert!((npv - 24.793).abs() < 0.05);
    }

    #[test]
    fn test_irr_known_cash_flows() {
        // Initial: -$1,000 at t=0, +$1,100 at t=1.0 -> Exact IRR = 10% (0.10)
        let flows = vec![(0.0, -1000.0), (1.0, 1100.0)];
        let irr = calculate_irr(&flows).expect("IRR should converge");
        assert!((irr - 0.10).abs() < 1e-4);

        // Three period flow: -$1,000, +$500 at t=1, +$700 at t=2
        // NPV(r) = -1000 + 500/(1+r) + 700/(1+r)^2 = 0 => r approx 12.87%
        let flows2 = vec![(0.0, -1000.0), (1.0, 500.0), (2.0, 700.0)];
        let irr2 = calculate_irr(&flows2).expect("IRR should converge");
        assert!((irr2 - 0.1287).abs() < 0.01);
    }

    #[test]
    fn test_irr_unrealizable_cash_flows() {
        // All positive flows: no IRR solution
        let flows = vec![(0.0, 100.0), (1.0, 200.0)];
        assert!(calculate_irr(&flows).is_none());

        // All negative flows: no IRR solution
        let flows_neg = vec![(0.0, -100.0), (1.0, -200.0)];
        assert!(calculate_irr(&flows_neg).is_none());
    }

    #[test]
    fn test_time_weighted_return_calculation() {
        // Timeline with deposit and subsequent market gain
        let timeline = vec![
            TimelineEntry {
                timestamp_secs: 1000,
                timestamp_rfc3339: "2024-01-01T00:00:00Z".to_string(),
                ledger: 10,
                cash_inflow_usd: 1000.0,
                cash_outflow_usd: 0.0,
                dividend_usd: 0.0,
                portfolio_value_usd: 1000.0,
            },
            TimelineEntry {
                timestamp_secs: 2000,
                timestamp_rfc3339: "2024-02-01T00:00:00Z".to_string(),
                ledger: 20,
                cash_inflow_usd: 500.0,
                cash_outflow_usd: 0.0,
                dividend_usd: 0.0,
                portfolio_value_usd: 1700.0, // Grew to 1200, then +500 deposit = 1700 (+20% gain in period 1)
            },
            TimelineEntry {
                timestamp_secs: 3000,
                timestamp_rfc3339: "2024-03-01T00:00:00Z".to_string(),
                ledger: 30,
                cash_inflow_usd: 0.0,
                cash_outflow_usd: 0.0,
                dividend_usd: 0.0,
                portfolio_value_usd: 1870.0, // Grew from 1700 to 1870 (+10% gain in period 2)
            },
        ];

        let (twr, points) = compute_time_weighted_return(&timeline);
        // Period 1 return: (1700 - 500 - 1000) / 1000 = 200 / 1000 = 0.20
        // Period 2 return: (1870 - 1700) / 1700 = 170 / 1700 = 0.10
        // Compound TWR: (1 + 0.20) * (1 + 0.10) - 1 = 1.32 - 1 = 0.32 (32%)
        assert!((twr - 0.32).abs() < 1e-4);
        assert_eq!(points.len(), 3);
        assert_eq!(points[2].cumulative_twr_pct, 32.0);
    }

    #[tokio::test]
    async fn test_portfolio_endpoint_success() {
        let mut snap = crate::indexer::Snapshot::default();
        let mut a = test_asset(1);
        a.valuation_usd = 1_000_000.0;
        a.total_supply = "10000000000000".to_string(); // 1M tokens with 7 decimals
        a.decimals = 7;
        snap.assets.push(a);

        let mut holders_map = HashMap::new();
        holders_map.insert(
            1,
            vec![Holder {
                address: "GACCOUNT123".to_string(),
                balance: "1000000000000".to_string(), // 100k tokens (10% of supply)
                share_percent: 10.0,
            }],
        );
        snap.holders = holders_map;

        let state = state_with(snap);
        let app = Router::new()
            .route("/v1/holders/:address/portfolio", get(get_portfolio))
            .with_state(state);

        let request = Request::builder()
            .uri("/v1/holders/GACCOUNT123/portfolio")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["address"], "GACCOUNT123");
        assert_eq!(json["summary"]["total_assets_held"], 1);
        assert!((json["summary"]["total_valuation_usd"].as_f64().unwrap() - 100_000.0).abs() < 1.0);
        assert_eq!(json["holdings"][0]["asset_id"], 1);
        assert_eq!(json["holdings"][0]["share_percent"], 10.0);
    }

    #[tokio::test]
    async fn test_portfolio_endpoint_empty_address() {
        let snap = crate::indexer::Snapshot::default();
        let state = state_with(snap);
        let app = Router::new()
            .route("/v1/holders/:address/portfolio", get(get_portfolio))
            .with_state(state);

        let request = Request::builder()
            .uri("/v1/holders/GNONE/portfolio")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["address"], "GNONE");
        assert_eq!(json["summary"]["total_valuation_usd"], 0.0);
        assert_eq!(json["summary"]["total_assets_held"], 0);
        assert_eq!(json["holdings"].as_array().unwrap().len(), 0);
    }
}
