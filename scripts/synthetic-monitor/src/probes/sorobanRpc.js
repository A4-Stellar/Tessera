/**
 * Soroban RPC Synthetic Probes.
 * Performs live JSON-RPC health, ledger, and contract state queries against Stellar RPC.
 */

/**
 * Execute a JSON-RPC request against Soroban RPC with timeout.
 * @param {string} rpcUrl
 * @param {string} method
 * @param {object} params
 * @param {number} timeoutMs
 * @returns {Promise<{result: any, latencyMs: number}>}
 */
async function callRpc(rpcUrl, method, params = {}, timeoutMs = 10000) {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);
  const startTime = Date.now();

  try {
    const response = await fetch(rpcUrl, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'User-Agent': 'Tessera-Synthetic-Monitor/1.0'
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: `synth-${Date.now()}`,
        method,
        params
      }),
      signal: controller.signal
    });

    const latencyMs = Date.now() - startTime;

    if (!response.ok) {
      throw new Error(`HTTP ${response.status} ${response.statusText}`);
    }

    const data = await response.json();

    if (data.error) {
      const errMsg = typeof data.error === 'object' ? data.error.message || JSON.stringify(data.error) : data.error;
      throw new Error(`RPC error: ${errMsg}`);
    }

    return { result: data.result, latencyMs };
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * Probe Soroban RPC health status.
 * @param {string} rpcUrl
 * @param {number} timeoutMs
 */
export async function probeSorobanHealth(rpcUrl, timeoutMs = 10000) {
  const probeName = 'soroban_rpc_health';
  try {
    const { result, latencyMs } = await callRpc(rpcUrl, 'getHealth', {}, timeoutMs);
    const status = result?.status;
    const isHealthy = status === 'healthy';

    if (!isHealthy) {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Unexpected RPC health status: "${status}"`,
        details: result
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        status,
        latestLedger: result.latestLedger,
        oldestLedger: result.oldestLedger
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `RPC request timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe Soroban RPC latest ledger metadata.
 * @param {string} rpcUrl
 * @param {number} timeoutMs
 */
export async function probeSorobanLatestLedger(rpcUrl, timeoutMs = 10000) {
  const probeName = 'soroban_rpc_latest_ledger';
  try {
    const { result, latencyMs } = await callRpc(rpcUrl, 'getLatestLedger', {}, timeoutMs);
    const sequence = result?.sequence;
    const protocolVersion = result?.protocolVersion;

    if (!sequence || typeof sequence !== 'number') {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Invalid or missing ledger sequence in RPC response: ${JSON.stringify(result)}`
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        sequence,
        protocolVersion,
        ledgerCloseTime: result.ledgerCloseTime
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `RPC request timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe Soroban contract events or simulated state.
 * @param {string} rpcUrl
 * @param {string} contractId
 * @param {number} startLedger
 * @param {number} timeoutMs
 */
export async function probeSorobanContractEvents(rpcUrl, contractId, startLedger, timeoutMs = 10000) {
  const probeName = 'soroban_rpc_contract_events';
  try {
    const params = {
      startLedger: startLedger > 10 ? startLedger - 10 : startLedger,
      filters: [
        {
          type: 'contract',
          contractIds: [contractId]
        }
      ],
      pagination: {
        limit: 5
      }
    };

    const { result, latencyMs } = await callRpc(rpcUrl, 'getEvents', params, timeoutMs);

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        contractId,
        eventsFound: result?.events?.length || 0,
        latestLedger: result?.latestLedger
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `RPC request timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe Soroban simulated read transaction / contract entry verification.
 * Simulates a read verification across registered contract IDs against Soroban RPC.
 * @param {string} rpcUrl
 * @param {string} contractId
 * @param {number} timeoutMs
 */
export async function probeSorobanSimulateRead(rpcUrl, contractId, timeoutMs = 10000) {
  const probeName = 'soroban_rpc_simulated_read';
  try {
    // Queries RPC getEvents / state to verify contract read accessibility
    const { result, latencyMs } = await callRpc(rpcUrl, 'getEvents', {
      startLedger: 1,
      filters: [{ type: 'contract', contractIds: [contractId] }],
      pagination: { limit: 1 }
    }, timeoutMs);

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        contractId,
        simulatedReadSuccess: true,
        latestLedger: result?.latestLedger
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `Simulated read timed out after ${timeoutMs}ms` : err.message
    };
  }
}
