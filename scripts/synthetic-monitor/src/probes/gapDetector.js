/**
 * Indexer Gap & Freshness Analyzer.
 * Compares on-chain Soroban ledger progression with Tessera API snapshot timestamps to detect sync stalls.
 */

/**
 * Evaluate indexer synchronization gap.
 * @param {object} rpcHealthResult
 * @param {object} rpcLedgerResult
 * @param {object} apiHealthResult
 * @param {object} apiStatsResult
 * @param {object} thresholds
 * @returns {object}
 */
export function evaluateIndexerGap(
  rpcHealthResult,
  rpcLedgerResult,
  apiHealthResult,
  apiStatsResult,
  thresholds = { maxSnapshotAgeSeconds: 60 }
) {
  const probeName = 'indexer_gap_detection';

  // If RPC or API failed basic connectivity, defer failure to those probes
  if (!rpcLedgerResult?.success || !apiHealthResult?.success) {
    return {
      name: probeName,
      success: true,
      skipped: true,
      reason: 'Skipped gap evaluation due to upstream probe failure'
    };
  }

  const latestLedgerSeq = rpcLedgerResult.details?.sequence;
  const snapshotAgeSeconds = apiHealthResult.details?.snapshot_age_seconds;
  const maxAge = thresholds.maxSnapshotAgeSeconds || 60;

  // Check 1: Snapshot age threshold
  if (typeof snapshotAgeSeconds === 'number' && snapshotAgeSeconds > maxAge) {
    return {
      name: probeName,
      success: false,
      error: `Indexer snapshot is stale: age is ${snapshotAgeSeconds}s (threshold: ${maxAge}s)`,
      details: {
        snapshotAgeSeconds,
        thresholdSeconds: maxAge,
        latestLedgerSeq
      }
    };
  }

  // Check 2: API stats last_updated timestamp
  const lastUpdatedStr = apiStatsResult?.details?.lastUpdated;
  if (lastUpdatedStr) {
    const lastUpdatedTime = new Date(lastUpdatedStr).getTime();
    if (!isNaN(lastUpdatedTime)) {
      const statsAgeSeconds = Math.floor((Date.now() - lastUpdatedTime) / 1000);
      if (statsAgeSeconds > maxAge * 2) {
        return {
          name: probeName,
          success: false,
          error: `API stats timestamp is stale: ${statsAgeSeconds}s old (threshold: ${maxAge * 2}s)`,
          details: {
            statsAgeSeconds,
            lastUpdated: lastUpdatedStr,
            latestLedgerSeq
          }
        };
      }
    }
  }

  return {
    name: probeName,
    success: true,
    details: {
      latestLedgerSeq,
      snapshotAgeSeconds: snapshotAgeSeconds ?? 0,
      healthy: true
    }
  };
}
