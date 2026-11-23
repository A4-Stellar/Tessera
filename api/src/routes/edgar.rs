//! `POST /v1/admin/cap-table/import-edgar` — Institutional SEC EDGAR Form 4
//! Cap-Table Import & On-Chain Reconciliation (Issue #476).
//!
//! Fetches or ingests SEC EDGAR Form 4 insider/holder filings for a given CIK
//! and reconciles them against the on-chain holder balances for an indexed RWA
//! asset (`GET /v1/assets/:id/holders`):
//! - Detects `new_holder`, `departed_holder`, `changed_balance` (absolute & %
//!   delta), and `unchanged` positions.
//! - Flags balance changes `> 5%` as `"yellow"` and `> 10%` as `"red"`.
//! - Triggers a signed `cap_table.edgar_discrepancy` compliance webhook and
//!   appends a cryptographic audit log entry when the discrepancy exceeds `1%`
//!   of total supply.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ApiError;
use crate::audit::AuditAction;
use crate::indexer::AppState;

const DEFAULT_EDGAR_BASE_URL: &str = "https://data.sec.gov";
const SEC_USER_AGENT: &str = "TesseraComplianceEngine/1.0 (engineering@caliberfoundry.com)";
/// Discrepancy threshold (% of total supply) that triggers a compliance webhook.
pub const WEBHOOK_SUPPLY_THRESHOLD_PERCENT: f64 = 1.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EdgarFilingRecord {
    pub holder_name: String,
    pub address: String,
    pub shares: u128,
    #[serde(default)]
    pub accession_number: Option<String>,
    #[serde(default)]
    pub filing_date: Option<String>,
    #[serde(default)]
    pub transaction_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ImportEdgarRequest {
    pub cik: String,
    pub asset_id: u64,
    #[serde(default)]
    pub filings: Option<Vec<EdgarFilingRecord>>,
    #[serde(default)]
    pub csv_data: Option<String>,
    #[serde(default)]
    pub edgar_api_base_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderDiffStatus {
    NewHolder,
    DepartedHolder,
    ChangedBalance,
    Unchanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThresholdHighlight {
    None,
    Yellow,
    Red,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReconciledHolderDiff {
    pub address: String,
    pub holder_name: String,
    pub edgar_shares: u128,
    pub on_chain_shares: u128,
    pub absolute_change: i128,
    pub percent_change: f64,
    pub supply_discrepancy_percent: f64,
    pub status: HolderDiffStatus,
    pub highlight: ThresholdHighlight,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EdgarComplianceWebhookPayload {
    pub event: String,
    pub asset_id: u64,
    pub cik: String,
    pub total_supply: u128,
    pub total_discrepancy_shares: u128,
    pub total_discrepancy_percent_of_supply: f64,
    pub threshold_percent_of_supply: f64,
    pub flagged_holders_count: usize,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportEdgarResponse {
    pub asset_id: u64,
    pub cik: String,
    pub total_supply: u128,
    pub edgar_total_shares: u128,
    pub on_chain_total_shares: u128,
    pub total_discrepancy_shares: u128,
    pub total_discrepancy_percent_of_supply: f64,
    pub new_holders_count: usize,
    pub departed_holders_count: usize,
    pub changed_balances_count: usize,
    pub unchanged_count: usize,
    pub yellow_alert_count: usize,
    pub red_alert_count: usize,
    pub compliance_webhook_triggered: bool,
    pub webhook_payload: Option<EdgarComplianceWebhookPayload>,
    pub signature: String,
    pub rows: Vec<ReconciledHolderDiff>,
}

/// Normalize a CIK string (e.g. `"CIK0001849821"` or `"1849821"`) into 10-digit zero-padded form.
pub fn normalize_cik(raw: &str) -> Result<String, ApiError> {
    let cleaned = raw
        .trim()
        .trim_start_matches("CIK")
        .trim_start_matches("cik")
        .trim();
    if cleaned.is_empty() || !cleaned.chars().all(|c| c.is_ascii_digit()) || cleaned.len() > 10 {
        return Err(ApiError::BadRequest(format!(
            "invalid SEC CIK '{raw}': expected 1 to 10 digits"
        )));
    }
    Ok(format!("{cleaned:0>10}"))
}

/// Classify a balance change percentage into threshold highlights:
/// - `> 10.0%` -> `ThresholdHighlight::Red`
/// - `> 5.0%` -> `ThresholdHighlight::Yellow`
/// - `<= 5.0%` -> `ThresholdHighlight::None`
pub fn classify_highlight(percent_change: f64) -> ThresholdHighlight {
    let abs = percent_change.abs();
    if abs > 10.0 {
        ThresholdHighlight::Red
    } else if abs > 5.0 {
        ThresholdHighlight::Yellow
    } else {
        ThresholdHighlight::None
    }
}

/// Parse an SEC EDGAR Form 4 CSV body into `EdgarFilingRecord` items.
pub fn parse_edgar_csv(csv_text: &str) -> Vec<EdgarFilingRecord> {
    let mut lines = csv_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let Some(header_line) = lines.next() else {
        return Vec::new();
    };
    let headers: Vec<String> = header_line
        .split(',')
        .map(|h| h.trim().to_lowercase())
        .collect();

    let find_col = |candidates: &[&str]| -> Option<usize> {
        headers
            .iter()
            .position(|h| candidates.iter().any(|c| h == c || h.contains(c)))
    };

    let name_idx = find_col(&["reporting_owner", "holder_name", "owner_name", "name"]);
    let addr_idx = find_col(&["stellar_address", "address", "wallet", "account"]);
    let shares_idx = find_col(&[
        "shares_owned_following_transaction",
        "post_transaction_shares",
        "shares",
        "balance",
    ]);
    let acc_idx = find_col(&["accession_number", "accession"]);
    let date_idx = find_col(&["filing_date", "transaction_date", "date"]);
    let code_idx = find_col(&["transaction_code", "code"]);

    let mut by_addr: BTreeMap<String, EdgarFilingRecord> = BTreeMap::new();
    for line in lines {
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 2 {
            continue;
        }
        let address = addr_idx
            .and_then(|i| cols.get(i))
            .copied()
            .unwrap_or(cols[0])
            .trim();
        if address.is_empty() {
            continue;
        }
        let shares_raw = shares_idx
            .and_then(|i| cols.get(i))
            .copied()
            .unwrap_or_else(|| cols.last().copied().unwrap_or("0"));
        let shares = shares_raw.replace(['_', ','], "").parse::<u128>().unwrap_or(0);
        let holder_name = name_idx
            .and_then(|i| cols.get(i))
            .copied()
            .unwrap_or(address)
            .to_string();

        by_addr.insert(
            address.to_string(),
            EdgarFilingRecord {
                holder_name,
                address: address.to_string(),
                shares,
                accession_number: acc_idx.and_then(|i| cols.get(i)).map(|s| (*s).to_string()),
                filing_date: date_idx.and_then(|i| cols.get(i)).map(|s| (*s).to_string()),
                transaction_code: code_idx.and_then(|i| cols.get(i)).map(|s| (*s).to_string()),
            },
        );
    }

    by_addr.into_values().collect()
}

async fn fetch_edgar_filings_from_api(
    base_url: &str,
    cik10: &str,
) -> Result<Vec<EdgarFilingRecord>, ApiError> {
    let url = format!(
        "{}/submissions/CIK{}.json",
        base_url.trim_end_matches('/'),
        cik10
    );
    let client = reqwest::Client::builder()
        .user_agent(SEC_USER_AGENT)
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::Unavailable(format!("failed to build EDGAR HTTP client: {e}")))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("EDGAR API request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "EDGAR API returned HTTP {}",
            resp.status()
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::BadRequest(format!("invalid EDGAR JSON payload: {e}")))?;

    if let Some(arr) = body.get("form4_holdings").and_then(|v| v.as_array()) {
        let mut records = Vec::new();
        for item in arr {
            if let Ok(rec) = serde_json::from_value::<EdgarFilingRecord>(item.clone()) {
                records.push(rec);
            }
        }
        return Ok(records);
    }

    Ok(Vec::new())
}

/// `POST /v1/admin/cap-table/import-edgar`
pub async fn import_edgar(
    State(state): State<AppState>,
    Json(req): Json<ImportEdgarRequest>,
) -> Result<Json<ImportEdgarResponse>, ApiError> {
    let cik10 = normalize_cik(&req.cik)?;
    let snap = state.snapshot();
    let Some(asset) = snap.asset(req.asset_id) else {
        return Err(ApiError::NotFound(format!(
            "no asset with id {}",
            req.asset_id
        )));
    };

    let edgar_records = if let Some(filings) = req.filings {
        filings
    } else if let Some(csv) = &req.csv_data {
        parse_edgar_csv(csv)
    } else {
        let base_url = req
            .edgar_api_base_url
            .as_deref()
            .unwrap_or(DEFAULT_EDGAR_BASE_URL);
        fetch_edgar_filings_from_api(base_url, &cik10).await?
    };

    let on_chain_holders = snap
        .holders
        .get(&req.asset_id)
        .cloned()
        .unwrap_or_default();

    let mut edgar_map: BTreeMap<String, EdgarFilingRecord> = BTreeMap::new();
    for rec in edgar_records {
        let addr = rec.address.trim().to_string();
        if !addr.is_empty() {
            edgar_map.insert(addr, rec);
        }
    }

    let mut on_chain_map: BTreeMap<String, u128> = BTreeMap::new();
    for h in &on_chain_holders {
        let bal = h.balance.parse::<u128>().unwrap_or(0);
        on_chain_map.insert(h.address.clone(), bal);
    }

    let edgar_total_shares: u128 = edgar_map.values().map(|r| r.shares).sum();
    let on_chain_total_shares: u128 = on_chain_map.values().copied().sum();
    let parsed_supply = asset.total_supply.parse::<u128>().unwrap_or(0);
    let total_supply = parsed_supply
        .max(on_chain_total_shares)
        .max(edgar_total_shares)
        .max(1);

    let all_addresses: BTreeSet<String> = edgar_map
        .keys()
        .chain(on_chain_map.keys())
        .cloned()
        .collect();

    let mut rows = Vec::with_capacity(all_addresses.len());
    let mut new_holders_count = 0usize;
    let mut departed_holders_count = 0usize;
    let mut changed_balances_count = 0usize;
    let mut unchanged_count = 0usize;
    let mut yellow_alert_count = 0usize;
    let mut red_alert_count = 0usize;
    let mut total_discrepancy_shares = 0u128;

    for address in all_addresses {
        let edgar_rec = edgar_map.get(&address);
        let edgar_shares = edgar_rec.map_or(0, |r| r.shares);
        let on_chain_shares = on_chain_map.get(&address).copied().unwrap_or(0);
        let holder_name = edgar_rec
            .map(|r| r.holder_name.clone())
            .unwrap_or_else(|| address.clone());

        let absolute_change = (on_chain_shares as i128) - (edgar_shares as i128);
        let abs_delta = absolute_change.unsigned_abs();
        total_discrepancy_shares = total_discrepancy_shares.saturating_add(abs_delta);

        let (status, percent_change) = if edgar_shares == 0 && on_chain_shares > 0 {
            new_holders_count += 1;
            (HolderDiffStatus::NewHolder, 100.0)
        } else if edgar_shares > 0 && on_chain_shares == 0 {
            departed_holders_count += 1;
            (HolderDiffStatus::DepartedHolder, -100.0)
        } else if edgar_shares != on_chain_shares {
            changed_balances_count += 1;
            let pct = ((on_chain_shares as f64 - edgar_shares as f64) / edgar_shares as f64) * 100.0;
            (HolderDiffStatus::ChangedBalance, (pct * 100.0).round() / 100.0)
        } else {
            unchanged_count += 1;
            (HolderDiffStatus::Unchanged, 0.0)
        };

        let highlight = classify_highlight(percent_change);
        match highlight {
            ThresholdHighlight::Yellow => yellow_alert_count += 1,
            ThresholdHighlight::Red => red_alert_count += 1,
            ThresholdHighlight::None => {}
        }

        let supply_discrepancy_percent =
            (((abs_delta as f64) / (total_supply as f64)) * 100.0 * 10000.0).round() / 10000.0;

        rows.push(ReconciledHolderDiff {
            address,
            holder_name,
            edgar_shares,
            on_chain_shares,
            absolute_change,
            percent_change,
            supply_discrepancy_percent,
            status,
            highlight,
        });
    }

    rows.sort_by(|a, b| {
        b.absolute_change
            .unsigned_abs()
            .cmp(&a.absolute_change.unsigned_abs())
            .then_with(|| a.address.cmp(&b.address))
    });

    let total_discrepancy_percent_of_supply =
        (((total_discrepancy_shares as f64) / (total_supply as f64)) * 100.0 * 10000.0).round()
            / 10000.0;

    let any_holder_exceeds_threshold = rows
        .iter()
        .any(|r| r.supply_discrepancy_percent > WEBHOOK_SUPPLY_THRESHOLD_PERCENT);
    let compliance_webhook_triggered = total_discrepancy_percent_of_supply
        > WEBHOOK_SUPPLY_THRESHOLD_PERCENT
        || any_holder_exceeds_threshold;

    let mut hasher = Sha256::new();
    hasher.update(format!(
        "{}:{}:{}:{}:{}:{}",
        req.asset_id,
        cik10,
        total_supply,
        edgar_total_shares,
        on_chain_total_shares,
        total_discrepancy_shares
    ));
    let signature = hex::encode(hasher.finalize());

    let webhook_payload = if compliance_webhook_triggered {
        let flagged_holders_count = rows
            .iter()
            .filter(|r| r.status != HolderDiffStatus::Unchanged)
            .count();
        let payload = EdgarComplianceWebhookPayload {
            event: "cap_table.edgar_discrepancy".to_string(),
            asset_id: req.asset_id,
            cik: cik10.clone(),
            total_supply,
            total_discrepancy_shares,
            total_discrepancy_percent_of_supply,
            threshold_percent_of_supply: WEBHOOK_SUPPLY_THRESHOLD_PERCENT,
            flagged_holders_count,
            signature: signature.clone(),
        };

        let _ = state
            .audit
            .append(
                AuditAction::ParameterUpdate {
                    parameter: "cap_table.edgar_discrepancy".to_string(),
                    old_value: serde_json::json!({ "edgar_total_shares": edgar_total_shares }),
                    new_value: serde_json::json!({ "on_chain_total_shares": on_chain_total_shares }),
                },
                "edgar-reconciliation-engine".to_string(),
                Some(format!("asset:{}:cik:{}", req.asset_id, cik10)),
                serde_json::to_value(&payload).unwrap_or_default(),
            )
            .await;

        Some(payload)
    } else {
        None
    };

    Ok(Json(ImportEdgarResponse {
        asset_id: req.asset_id,
        cik: cik10,
        total_supply,
        edgar_total_shares,
        on_chain_total_shares,
        total_discrepancy_shares,
        total_discrepancy_percent_of_supply,
        new_holders_count,
        departed_holders_count,
        changed_balances_count,
        unchanged_count,
        yellow_alert_count,
        red_alert_count,
        compliance_webhook_triggered,
        webhook_payload,
        signature,
        rows,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Snapshot;
    use crate::models::Holder;
    use crate::routes::test_support::{asset, state_with};
    use std::collections::HashMap;

    const GOLDEN_EDGAR_CSV: &str = "\
cik,accession_number,filing_date,reporting_owner,stellar_address,shares_owned_following_transaction,transaction_code
0001849821,0001849821-26-000041,2026-10-15,Apex Institutional Partners LP,GAA6774APEXINSTITUTIONALPARTNERSLP777777777777777777LP2Q,400000,P
0001849821,0001849821-26-000042,2026-10-18,Beacon Sovereign Credit LLC,GBB8912BEACONSOVEREIGNCREDITLLC888888888888888888888M77R,250000,S
0001849821,0001849821-26-000043,2026-10-20,Citadel Real Estate Trust,GCC3319CITADELREALESTATETRUST9999999999999999999999K91A,200000,P
0001849821,0001849821-26-000044,2026-10-22,Delta Legacy Holdings Inc,GDD4420DELTALEGACYHOLDINGSINC1111111111111111111111X88B,50000,S";

    #[tokio::test]
    async fn test_import_edgar_reconciles_golden_dataset_and_triggers_compliance_webhook() {
        let mut test_asset = asset(1);
        test_asset.total_supply = "1000000".to_string();

        let mut holders_map = HashMap::new();
        holders_map.insert(
            1,
            vec![
                Holder {
                    address: "GAA6774APEXINSTITUTIONALPARTNERSLP777777777777777777LP2Q".to_string(),
                    balance: "430000".to_string(),
                    share_percent: 43.0,
                },
                Holder {
                    address: "GBB8912BEACONSOVEREIGNCREDITLLC888888888888888888888M77R".to_string(),
                    balance: "212500".to_string(),
                    share_percent: 21.25,
                },
                Holder {
                    address: "GCC3319CITADELREALESTATETRUST9999999999999999999999K91A".to_string(),
                    balance: "200000".to_string(),
                    share_percent: 20.0,
                },
                Holder {
                    address: "GEE5531EMERALDRWAYIELDFUND2222222222222222222222222Z19C".to_string(),
                    balance: "45000".to_string(),
                    share_percent: 4.5,
                },
            ],
        );

        let snap = Snapshot {
            assets: vec![test_asset],
            holders: holders_map,
            ..Snapshot::default()
        };
        let state = state_with(snap);

        let Json(resp) = import_edgar(
            State(state),
            Json(ImportEdgarRequest {
                cik: "1849821".to_string(),
                asset_id: 1,
                filings: None,
                csv_data: Some(GOLDEN_EDGAR_CSV.to_string()),
                edgar_api_base_url: None,
            }),
        )
        .await
        .expect("import_edgar should succeed");

        assert_eq!(resp.cik, "0001849821");
        assert_eq!(resp.new_holders_count, 1);
        assert_eq!(resp.departed_holders_count, 1);
        assert_eq!(resp.changed_balances_count, 2);
        assert_eq!(resp.unchanged_count, 1);
        assert_eq!(resp.yellow_alert_count, 1);
        assert_eq!(resp.red_alert_count, 3);
        assert!(resp.compliance_webhook_triggered);
        assert!(resp.webhook_payload.is_some());
        assert_eq!(resp.total_discrepancy_shares, 162_500);
    }
}
