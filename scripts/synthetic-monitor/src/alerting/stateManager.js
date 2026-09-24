/**
 * In-Memory State Manager for Synthetic Probe Alerts.
 *
 * Space Complexity: O(P) where P is the number of monitored probe targets (constant, bounded).
 * Time Complexity: O(1) for state lookup and transition evaluations per probe.
 */

export class AlertStateManager {
  /**
   * @param {object} options
   * @param {number} options.consecutiveFailureThreshold Number of consecutive failures required before alerting (default: 2)
   */
  constructor({ consecutiveFailureThreshold = 2 } = {}) {
    this.failureThreshold = consecutiveFailureThreshold;
    // Map<probeName, { consecutiveFailures: number, isAlerting: boolean, lastAlertTime: number, lastError: string }>
    this.probeStates = new Map();
  }

  /**
   * Process a probe execution result.
   * @param {object} probeResult
   * @param {string} probeResult.name Probe identifier
   * @param {boolean} probeResult.success Whether the probe succeeded
   * @param {string} [probeResult.error] Error message if failed
   * @param {number} [probeResult.latencyMs] Latency in milliseconds
   * @param {object} [probeResult.details] Additional probe metrics
   * @returns {{
   *   shouldAlert: boolean,
   *   isRecovery: boolean,
   *   consecutiveFailures: number,
   *   previousError?: string,
   *   status: 'healthy' | 'warning' | 'critical' | 'recovered'
   * }}
   */
  update(probeResult) {
    const { name, success, error, latencyMs = 0, skipped = false } = probeResult;

    if (skipped) {
      return {
        shouldAlert: false,
        isRecovery: false,
        consecutiveFailures: 0,
        status: 'healthy'
      };
    }

    let state = this.probeStates.get(name);
    if (!state) {
      state = {
        consecutiveFailures: 0,
        isAlerting: false,
        lastAlertTime: 0,
        lastError: null,
        lastSuccessTime: Date.now()
      };
      this.probeStates.set(name, state);
    }

    if (success) {
      const wasAlerting = state.isAlerting;
      const prevFailures = state.consecutiveFailures;
      const prevError = state.lastError;

      // Reset failure state
      state.consecutiveFailures = 0;
      state.isAlerting = false;
      state.lastError = null;
      state.lastSuccessTime = Date.now();

      if (wasAlerting) {
        return {
          shouldAlert: true,
          isRecovery: true,
          consecutiveFailures: 0,
          previousError: prevError,
          status: 'recovered'
        };
      }

      return {
        shouldAlert: false,
        isRecovery: false,
        consecutiveFailures: 0,
        status: 'healthy'
      };
    }

    // Probe Failed
    state.consecutiveFailures += 1;
    state.lastError = error;

    // Trigger alert if failures exceed threshold (> 2 consecutive failures)
    // and we haven't already marked it as alerting (or if re-arming is desired)
    const thresholdExceeded = state.consecutiveFailures > this.failureThreshold;

    if (thresholdExceeded) {
      const isInitialTrigger = !state.isAlerting;
      state.isAlerting = true;
      state.lastAlertTime = Date.now();

      return {
        shouldAlert: isInitialTrigger,
        isRecovery: false,
        consecutiveFailures: state.consecutiveFailures,
        status: 'critical'
      };
    }

    return {
      shouldAlert: false,
      isRecovery: false,
      consecutiveFailures: state.consecutiveFailures,
      status: 'warning'
    };
  }

  /**
   * Retrieve all current probe states.
   */
  getAllStates() {
    const result = {};
    for (const [name, state] of this.probeStates.entries()) {
      result[name] = { ...state };
    }
    return result;
  }
}
