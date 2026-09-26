//! Indexer → DB persistence bridge (issue #42).
//!
//! After each successful Soroban RPC poll cycle the indexer calls
//! [`persist_snapshot`] to durably write the current in-memory snapshot into
//! the TimescaleDB-backed tables.  All writes happen in a single async task so
//! they never block the HTTP handlers.
//!
//! # Write strategy
//!
//! | Table              | Strategy                                          |
//! |--------------------|---------------------------------------------------|
//! | `assets`           | Upsert on `token_contract` (idempotent)           |
//! | `holders`          | Upsert on `(asset_id, address)` (idempotent)      |
//! | `events`           | Append-only bulk insert (UNNEST)                  |
//! | `snapshot_history` | Append-only; one row per address per ledger cycle |
//!
//! Events are deduplicated upstream by the indexer (each Soroban event has a
//! stable `(ledger, tx_index, event_index)` triple); this layer trusts that
//! deduplication and inserts everything it receives.

use chrono::Utc;
use tracing::{error, info, warn};

use crate::db::{Db, DbError, InsertEvent, InsertSnapshotHistory};
use crate::models::{Asset, Event, Holder};

/// Persist a complete indexer snapshot to the database.
///
/// Called by the indexer background task after every successful poll cycle.
/// Errors are logged but never propagated — a DB write failure must not crash
/// the indexer or degrade the read API.
pub async fn persist_snapshot(
    db: &Db,
    assets: &[Asset],
    holders_by_asset: &[(u64, Vec<Holder>)],
    new_events: &[Event],
    ledger_sequence: u32,
) {
    // ── Assets ────────────────────────────────────────────────────────────
    for asset in assets {
        let row = crate::db::InsertAsset {
            token_contract: asset.token_contract.clone(),
            issuer: asset.issuer.clone(),
            name: asset.name.clone(),
            symbol: asset.symbol.clone(),
            asset_type: asset.asset_type.clone(),
            description: asset.description.clone(),
            valuation_cents: asset
                .valuation_cents
                .parse::<i64>()
                .unwrap_or(0),
            decimals: asset.decimals as i16,
            total_supply: asset.total_supply.clone(),
            active: asset.active,
            paused: asset.paused,
            compliance_contract: asset.compliance_contract.clone(),
            created_at_ledger: asset.created_at_ledger as i64,
            indexed_at_ledger: asset.indexed_at_ledger as i64,
            index_error: asset.index_error.clone(),
        };

        match db.upsert_asset(row).await {
            Ok(db_id) => {
                // ── Holders for this asset ─────────────────────────────
                let holders = holders_by_asset
                    .iter()
                    .find(|(id, _)| *id == asset.id)
                    .map(|(_, h)| h.as_slice())
                    .unwrap_or(&[]);

                for holder in holders {
                    if let Err(e) = db
                        .upsert_holder(
                            db_id,
                            &holder.address,
                            &holder.balance,
                            holder.share_percent,
                            ledger_sequence as i64,
                        )
                        .await
                    {
                        warn!(
                            asset_id = asset.id,
                            address = %holder.address,
                            error = %e,
                            "failed to upsert holder"
                        );
                    }

                    // ── Snapshot history row ───────────────────────────
                    let snap = InsertSnapshotHistory {
                        asset_id: db_id,
                        address: holder.address.clone(),
                        balance: holder.balance.clone(),
                        ledger_sequence: ledger_sequence as i64,
                        indexed_at: Utc::now(),
                    };
                    if let Err(e) = db.insert_snapshot(snap).await {
                        warn!(
                            asset_id = asset.id,
                            address = %holder.address,
                            error = %e,
                            "failed to insert snapshot history"
                        );
                    }
                }
            }
            Err(e) => {
                error!(
                    token_contract = %asset.token_contract,
                    error = %e,
                    "failed to upsert asset"
                );
            }
        }
    }

    // ── Events ────────────────────────────────────────────────────────────
    if !new_events.is_empty() {
        let rows: Vec<InsertEvent> = new_events
            .iter()
            .map(|ev| InsertEvent {
                contract: ev.contract.clone(),
                event_type: ev.event_type.clone(),
                ledger_sequence: ev.ledger as i64,
                occurred_at: ev
                    .timestamp
                    .as_deref()
                    .and_then(|ts| ts.parse().ok())
                    .unwrap_or_else(Utc::now),
                data: ev.data.clone(),
            })
            .collect();

        match db.insert_events(&rows).await {
            Ok(n) => info!(count = n, ledger = ledger_sequence, "events persisted"),
            Err(e) => error!(error = %e, "failed to bulk-insert events"),
        }
    }
}

/// Return historical balance snapshots for an address.
///
/// Backs `GET /v1/holders/:address/history`. Returns at most `limit` entries,
/// newest first.
pub async fn holder_history(
    db: &Db,
    address: &str,
    limit: i64,
) -> Result<Vec<crate::db::SnapshotHistoryRow>, DbError> {
    db.get_holder_history(address, limit).await
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Event;

    #[test]
    fn event_timestamp_fallback() {
        // When `timestamp` is None the occurred_at field should default to now
        // without panicking.
        let ev = Event {
            id: 1,
            contract: "CBMCWLSQ".into(),
            event_type: "transfer".into(),
            ledger: 100,
            timestamp: None,
            data: serde_json::json!({}),
        };
        let occurred_at = ev
            .timestamp
            .as_deref()
            .and_then(|ts| ts.parse::<chrono::DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);
        // Just assert it didn't panic and is a sane timestamp.
        assert!(occurred_at.timestamp() > 0);
    }

    #[test]
    fn event_timestamp_parsed() {
        let ev = Event {
            id: 2,
            contract: "CBMCWLSQ".into(),
            event_type: "mint".into(),
            ledger: 200,
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            data: serde_json::json!({}),
        };
        let parsed = ev
            .timestamp
            .as_deref()
            .and_then(|ts| ts.parse::<chrono::DateTime<Utc>>().ok());
        assert!(parsed.is_some());
    }
}
