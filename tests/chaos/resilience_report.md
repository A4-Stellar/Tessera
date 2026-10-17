# Tessera Chaos Engineering Resilience Report

**Target System:** `http://localhost:8080`  
**Overall Resilience Score:** `99.2 / 100` (A+)  
**Resilience Status:** `PASSED ✅`  

## Experiment Scenarios

| Scenario | Probes | Availability | p95 Latency | Fallback Rate | Score | Status |
|---|---|---|---|---|---|---|
| `baseline` | 20 | 100.0% | 12.0 ms | 0.0% | 100.0 | ✅ |
| `network_delay_200ms_2000ms` | 20 | 100.0% | 1892.0 ms | 0.0% | 95.0 | ✅ |
| `packet_drop_10pct` | 20 | 100.0% | 162.0 ms | 10.0% | 100.0 | ✅ |
| `pod_kill_api_instances` | 20 | 100.0% | 37.0 ms | 0.0% | 100.0 | ✅ |
| `rpc_outage_503_simulation` | 20 | 100.0% | 17.0 ms | 65.0% | 100.0 | ✅ |
| `db_master_crash_failover` | 20 | 100.0% | 57.0 ms | 100.0% | 100.0 | ✅ |

## Key Fallback Mechanisms Verified
- **In-Memory Snapshot Cache**: Served read endpoints (`/stats`, `/assets`) during active Soroban RPC outages.
- **Circuit Breaker**: Isolated failing RPC connections without service crashing.
- **Read-Replica Router**: Seamlessly diverted queries during simulated primary database crashes.
- **Redundant Replicas**: Absorbed traffic with zero dropped requests during pod-kill events.

## Recommendations
- Optimize timeout limits under network_delay_200ms_2000ms: p95 reached 1892.0ms
