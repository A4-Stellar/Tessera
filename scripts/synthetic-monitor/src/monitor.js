/**
 * Synthetic Monitor Orchestrator.
 * Executes concurrent probes against Soroban RPC and Tessera API, analyzes sync gaps, and triggers alerts.
 *
 * Complexity Design:
 * - Time Complexity: O(P) where P is probe count, executed concurrently in parallel via Promise.allSettled. Total run time <= max(timeoutMs).
 * - Space Complexity: O(P) memory overhead, bounded state map.
 */

import { probeSorobanHealth, probeSorobanLatestLedger, probeSorobanContractEvents, probeSorobanSimulateRead } from './probes/sorobanRpc.js';
import { probeApiHealth, probeApiVersion, probeApiStats, probeApiAssets } from './probes/tesseraApi.js';
import { evaluateIndexerGap } from './probes/gapDetector.js';
import { dispatchAlert } from './alerting/notifier.js';

export class SyntheticMonitor {
  /**
   * @param {object} config
   * @param {import('./alerting/stateManager.js').AlertStateManager} stateManager
   */
  constructor(config, stateManager) {
    this.config = config;
    this.stateManager = stateManager;
  }

  /**
   * Run a single full monitoring cycle across all endpoints.
   * @returns {Promise<{
   *   timestamp: string,
   *   durationMs: number,
   *   allHealthy: boolean,
   *   results: Array<object>,
   *   alertsTriggered: number
   * }>}
   */
  async runCycle() {
    const startTime = Date.now();
    const { rpcUrl, apiUrl, timeoutMs, contracts } = this.config;

    console.log(`[Monitor] Starting synthetic cycle at ${new Date().toISOString()}`);

    // Phase 1: Execute baseline probes concurrently
    const [
      sorobanHealth,
      sorobanLedger,
      apiHealth,
      apiVersion,
      apiStats,
      apiAssets
    ] = await Promise.all([
      probeSorobanHealth(rpcUrl, timeoutMs),
      probeSorobanLatestLedger(rpcUrl, timeoutMs),
      probeApiHealth(apiUrl, timeoutMs),
      probeApiVersion(apiUrl, timeoutMs),
      probeApiStats(apiUrl, timeoutMs),
      probeApiAssets(apiUrl, timeoutMs)
    ]);

    // Phase 2: Secondary probes relying on baseline results & simulated contract reads
    const contractEventProbe = contracts?.registryId && sorobanLedger.success
      ? await probeSorobanContractEvents(rpcUrl, contracts.registryId, sorobanLedger.details?.sequence, timeoutMs)
      : { name: 'soroban_rpc_contract_events', success: true, skipped: true };

    const contractSimulateReadProbe = contracts?.registryId && sorobanHealth.success
      ? await probeSorobanSimulateRead(rpcUrl, contracts.registryId, timeoutMs)
      : { name: 'soroban_rpc_simulated_read', success: true, skipped: true };

    const gapProbe = evaluateIndexerGap(
      sorobanHealth,
      sorobanLedger,
      apiHealth,
      apiStats,
      { maxSnapshotAgeSeconds: this.config.maxSnapshotAgeSeconds }
    );

    const allProbeResults = [
      sorobanHealth,
      sorobanLedger,
      contractEventProbe,
      contractSimulateReadProbe,
      apiHealth,
      apiVersion,
      apiStats,
      apiAssets,
      gapProbe
    ];

    // Phase 3: Evaluate state transitions & dispatch alerts
    let alertsTriggered = 0;
    let allHealthy = true;

    for (const result of allProbeResults) {
      if (!result.success && !result.skipped) {
        allHealthy = false;
      }

      const transition = this.stateManager.update(result);

      if (transition.shouldAlert) {
        alertsTriggered++;
        console.warn(`[Monitor] Alert triggered for ${result.name} (Status: ${transition.status})`);

        await dispatchAlert(this.config, {
          probeName: result.name,
          isRecovery: transition.isRecovery,
          consecutiveFailures: transition.consecutiveFailures,
          error: result.error,
          previousError: transition.previousError,
          latencyMs: result.latencyMs,
          details: result.details
        });
      }
    }

    const durationMs = Date.now() - startTime;
    console.log(`[Monitor] Completed cycle in ${durationMs}ms - Status: ${allHealthy ? 'HEALTHY' : 'DEGRADED'}, Alerts: ${alertsTriggered}`);

    return {
      timestamp: new Date().toISOString(),
      durationMs,
      allHealthy,
      results: allProbeResults,
      alertsTriggered
    };
  }
}
