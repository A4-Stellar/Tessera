# Runbook: ShadowDivergence alert

The API runs a **shadow indexer** alongside the primary indexer. It reads a
different Soroban node (`RWA_SHADOW_RPC_URL`), owns a separate in-memory
snapshot, and every 60 seconds compares a random 10% sample of contracts with
the primary snapshot. A mismatch in holder balances, compliance states, or
asset metadata raises `ShadowDivergence` and writes the shadow snapshot to the
`forensics` RocksDB column family.

## Triage

1. Check the current state:
   ```sh
   curl -s "$API/v1/admin/health/shadow" | jq .
   ```
   Look at `shadow_lag_ledgers`, `last_verification_at`,
   `last_divergence_at`, and `divergences_total`.

2. Identify what diverged from the API logs — the `ShadowDivergence`
   error event lists the discrepancy kinds (`HolderBalance`,
   `ComplianceState`, `AssetMetadata`, `MissingAsset`), asset IDs, addresses,
   and both values.

3. Read the forensics copy of the snapshot. The RocksDB path defaults to
   `./data/shadow-forensics` (`RWA_SHADOW_FORENSICS_PATH`). Keys are
   `divergence:<timestamp>:<ledger>` in the `forensics` column family; values
   are JSON with `divergence` and `snapshot` fields.

4. Compare with an independent RPC read of the same contract:
   ```sh
   curl -s "$RPC" -H 'Content-Type: application/json' -d '{
     "jsonrpc":"2.0","id":1,"method":"simulateTransaction",
     "params":[{"transaction":"<base64 envelope>"}]
   }'
   ```

## Common causes

| Symptom | Likely cause | Mitigation |
|---|---|---|
| Balances differ by a factor of 2^32 | XDR `i128` high word silently dropped | Fix decoder, replay from forensics snapshot |
| Compliance counts differ | Missed `get_record` event or expired record | Verify RPC event stream, replay affected ledger |
| Metadata hash differs | Stale node or new contract version | Check contract hash, register new schema |
| Shadow lag > 50 ledgers | Shadow RPC node unhealthy | Point `RWA_SHADOW_RPC_URL` at a healthy node |

## Resolution

Fixes to decoding must be applied to both indexers. After a fix, redeploy,
confirm `rwa_shadow_divergence_total` stops increasing and that
`/v1/admin/health/shadow` reports `status: ok` with a fresh
`last_verification_at`.
