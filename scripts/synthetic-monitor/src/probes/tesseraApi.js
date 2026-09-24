/**
 * Tessera REST API Synthetic Probes.
 * Validates HTTP endpoint availability, latency, response schemas, and indexer freshness.
 */

/**
 * Execute HTTP GET probe with timeout.
 * @param {string} url
 * @param {number} timeoutMs
 * @returns {Promise<{status: number, data: any, latencyMs: number}>}
 */
async function getJson(url, timeoutMs = 10000) {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);
  const startTime = Date.now();

  try {
    const response = await fetch(url, {
      method: 'GET',
      headers: {
        'Accept': 'application/json',
        'User-Agent': 'Tessera-Synthetic-Monitor/1.0'
      },
      signal: controller.signal
    });

    const latencyMs = Date.now() - startTime;
    const isJson = response.headers.get('content-type')?.includes('application/json');
    const data = isJson ? await response.json() : await response.text();

    return {
      status: response.status,
      ok: response.ok,
      data,
      latencyMs
    };
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * Probe API /health endpoint.
 * @param {string} apiUrl
 * @param {number} timeoutMs
 */
export async function probeApiHealth(apiUrl, timeoutMs = 10000) {
  const probeName = 'tessera_api_health';
  const url = `${apiUrl}/health`;

  try {
    const { status, ok, data, latencyMs } = await getJson(url, timeoutMs);

    if (!ok || status !== 200) {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Health check failed with status HTTP ${status}: ${typeof data === 'object' ? JSON.stringify(data) : data}`,
        details: data
      };
    }

    if (data?.status !== 'ok') {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `API reported degraded health: ${JSON.stringify(data)}`,
        details: data
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: data
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `API health probe timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe API /version endpoint.
 * @param {string} apiUrl
 * @param {number} timeoutMs
 */
export async function probeApiVersion(apiUrl, timeoutMs = 10000) {
  const probeName = 'tessera_api_version';
  const url = `${apiUrl}/version`;

  try {
    const { status, ok, data, latencyMs } = await getJson(url, timeoutMs);

    if (!ok || status !== 200 || !data?.version) {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Version check failed with status HTTP ${status}: ${JSON.stringify(data)}`
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: data
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `API version probe timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe API /v1/stats endpoint.
 * @param {string} apiUrl
 * @param {number} timeoutMs
 */
export async function probeApiStats(apiUrl, timeoutMs = 10000) {
  const probeName = 'tessera_api_stats';
  const url = `${apiUrl}/v1/stats`;

  try {
    const { status, ok, data, latencyMs } = await getJson(url, timeoutMs);

    if (!ok || status !== 200) {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Stats endpoint returned HTTP ${status}: ${JSON.stringify(data)}`
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        totalAssets: data.total_assets ?? data.totalAssets,
        totalHolders: data.total_holders ?? data.totalHolders,
        tvlUsd: data.tvl_usd ?? data.tvlUsd,
        lastUpdated: data.last_updated ?? data.lastUpdated
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `API stats probe timed out after ${timeoutMs}ms` : err.message
    };
  }
}

/**
 * Probe API /v1/assets endpoint.
 * @param {string} apiUrl
 * @param {number} timeoutMs
 */
export async function probeApiAssets(apiUrl, timeoutMs = 10000) {
  const probeName = 'tessera_api_assets';
  const url = `${apiUrl}/v1/assets`;

  try {
    const { status, ok, data, latencyMs } = await getJson(url, timeoutMs);

    if (!ok || status !== 200 || !Array.isArray(data)) {
      return {
        name: probeName,
        success: false,
        latencyMs,
        error: `Assets endpoint returned HTTP ${status} (expected JSON array): ${JSON.stringify(data)}`
      };
    }

    return {
      name: probeName,
      success: true,
      latencyMs,
      details: {
        assetCount: data.length
      }
    };
  } catch (err) {
    return {
      name: probeName,
      success: false,
      latencyMs: 0,
      error: err.name === 'AbortError' ? `API assets probe timed out after ${timeoutMs}ms` : err.message
    };
  }
}
