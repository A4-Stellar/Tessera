# Tessera FinOps Resource Right-Sizing & Optimization Report

**Observation Period:** `14.0 Days`  
**Total Workloads Analyzed:** `4`  
**Current Monthly Infrastructure Cost:** `$132.86`  
**Recommended Monthly Cost:** `$34.74`  
**Projected Monthly Cost Savings:** `$98.12 (73.9%)` 💰  

## Workload Provisioning Breakdown
- **Idle Workloads:** `1` (Safe to scale to zero or decommission)
- **Over-Provisioned:** `1` (Excess requests starving cluster)
- **Well-Sized:** `1` (Optimally aligned)
- **Under-Provisioned:** `1` (Requires limit increase)

## Container Recommendations

| Workload | Status | Current Req (CPU/RAM) | Recommended Req (CPU/RAM) | Current Cost | Rec Cost | Monthly Savings |
|---|---|---|---|---|---|---|
| `tessera-system/tessera-api` | `SEVERELY_OVERPROVISIONED` | 2000m / 4Gi | **270m / 624Mi** | $72.08 | $9.76 | **$62.32 (86%)** |
| `tessera-system/channel-node` | `WELL_SIZED` | 500m / 1Gi | **460m / 960Mi** | $18.02 | $16.61 | **$1.41 (8%)** |
| `tessera-system/mpc-signer` | `IDLE` | 1000m / 2Gi | **20m / 64Mi** | $36.04 | $0.79 | **$35.25 (98%)** |
| `tessera-system/synthetic-monitor` | `UNDERPROVISIONED` | 200m / 256Mi | **230m / 304Mi** | $6.72 | $7.58 | **$0.00 (0%)** |

## Next Steps & Application
Apply the generated Kubernetes patch definitions via `kubectl patch` or update the Helm values override files.