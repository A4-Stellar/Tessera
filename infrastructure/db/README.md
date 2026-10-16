# Multi-Cloud Active-Active Database Synchronization (AWS RDS <-> GCP Cloud SQL)

Module implementing continuous zero-downtime, bi-directional PostgreSQL logical replication between AWS RDS and GCP Cloud SQL (Issue #306).

## Key Features

1. **Bi-Directional Logical Replication Setup**:
   - Manages logical replication publications (`CREATE PUBLICATION tessera_active_pub`) and subscriptions (`CREATE SUBSCRIPTION`).
   - Configures `origin = none` and replication slot origin filtering to prevent cyclic ping-pong replication loops between AWS and GCP.

2. **Conflict-Free Resolution Rules**:
   - **Last-Write-Wins (LWW)** with microsecond hybrid logical timestamps (`updated_at_micros`).
   - **Deterministic Tie-Breaking**: when timestamps are identical, deterministic node priority rules decide the winner.
   - **Column-Level CRDT Merging**: non-conflicting field updates merge transparently.
   - **Tombstone Semantics**: delete tombstones cleanly override stale updates.

3. **Sub-Second Replication Latency (<1.0s SLA)**:
   - Real-time heartbeat probe measurements and WAL lag tracking.
   - Computes current lag, p95 lag, and alerts on SLA threshold breaches.

4. **Simulated Region Failover & Zero-Data-Loss Verification**:
   - Simulates regional cloud partition / failure.
   - Computes cryptographic SHA-256 row checksums across nodes to assert zero divergent records and zero data loss.

## Running Tests

```bash
cd infrastructure/db
cargo test
```
