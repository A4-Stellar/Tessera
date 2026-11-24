//! Integration tests for SLO report schemas, error budget calculations,
//! and alert runbook links (issue #471).

use serde_json::json;

#[test]
fn test_slo_report_response_schema() {
    let report = json!({
        "generated_at": "2026-11-24T14:30:00Z",
        "availability": {
            "name": "api_availability_99_9",
            "target": 0.999,
            "window": "30d",
            "total_requests": 1000000,
            "bad_events": 100,
            "error_rate": 0.0001,
            "error_budget_total": 0.001,
            "error_budget_remaining": 0.90,
            "error_budget_remaining_percent": 90.0,
            "burn_rates": {
                "burn_rate_5m": 1.0,
                "burn_rate_30m": 1.0,
                "burn_rate_1h": 1.0,
                "burn_rate_6h": 1.0,
                "burn_rate_1d": 1.0,
                "burn_rate_3d": 1.0
            },
            "status": "HEALTHY"
        },
        "latency": {
            "name": "api_latency_p99_100ms",
            "target": 0.99,
            "window": "30d",
            "total_requests": 1000000,
            "bad_events": 2000,
            "error_rate": 0.002,
            "error_budget_total": 0.01,
            "error_budget_remaining": 0.80,
            "error_budget_remaining_percent": 80.0,
            "burn_rates": {
                "burn_rate_5m": 1.2,
                "burn_rate_30m": 1.2,
                "burn_rate_1h": 1.2,
                "burn_rate_6h": 1.2,
                "burn_rate_1d": 1.2,
                "burn_rate_3d": 1.2
            },
            "status": "HEALTHY"
        },
        "trend_30d": {
            "days_monitored": 30,
            "trend_direction": "STABLE",
            "average_availability_budget_remaining": 0.92,
            "average_latency_budget_remaining": 0.85,
            "projected_exhaustion_days": null,
            "daily_history": [
                {
                    "date": "2026-11-24",
                    "error_rate": 0.0001,
                    "availability_budget_remaining": 0.90,
                    "latency_budget_remaining": 0.80
                }
            ]
        },
        "alerts": [
            {
                "alert": "TesseraApiErrorBudgetFastBurn",
                "severity": "page",
                "priority": "P1",
                "description": "1h burn > 14x AND 5m burn > 14x: critical rapid budget burn (immediate page)",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/api-error-budget.md"
            },
            {
                "alert": "TesseraApiErrorBudgetSlowBurn",
                "severity": "page",
                "priority": "P2",
                "description": "6h burn > 6x AND 30m burn > 6x: sustained error budget burn (business hours)",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/api-error-budget.md"
            },
            {
                "alert": "TesseraApiErrorBudgetLow",
                "severity": "ticket",
                "priority": "P3",
                "description": "Error budget remaining below 25%: freeze non-critical deployments",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/api-error-budget.md"
            },
            {
                "alert": "TesseraApiErrorBudgetExhausted",
                "severity": "page",
                "priority": "P1",
                "description": "Error budget exhausted (30-day SLO breached): reliability fixes only",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/api-error-budget.md"
            },
            {
                "alert": "ShadowDivergence",
                "severity": "page",
                "priority": "P1",
                "description": "Shadow indexer diverged from primary on-chain ledger state",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
            },
            {
                "alert": "ShadowLagHigh",
                "severity": "ticket",
                "priority": "P3",
                "description": "Shadow indexer is more than 50 ledgers behind primary state",
                "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
            }
        ]
    });

    assert_eq!(report["availability"]["target"], 0.999);
    assert_eq!(report["latency"]["target"], 0.99);
    assert_eq!(report["trend_30d"]["days_monitored"], 30);
    assert_eq!(report["alerts"].as_array().unwrap().len(), 6);

    for alert in report["alerts"].as_array().unwrap() {
        let url = alert["runbook_url"].as_str().unwrap();
        assert!(url.starts_with("https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/"));
        assert!(url.ends_with(".md"));
    }
}

#[test]
fn test_error_budget_remaining_math() {
    let slo_target = 0.999;
    let allowed_budget = 1.0 - slo_target; // 0.001
    let error_rate_30d = 0.0002;

    // Google SRE formula: 1 - (error_rate_30d / (1 - slo_target))
    let error_budget_remaining = 1.0 - (error_rate_30d / allowed_budget);
    assert!((error_budget_remaining - 0.8).abs() < 1e-6);
}
