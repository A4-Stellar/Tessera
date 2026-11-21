/**
 * Tessera DR failover worker (issue #393).
 *
 * Health-checks the primary API every 30 seconds and, after a configurable
 * number of consecutive failures, shifts the weighted Cloudflare Load Balancer
 * to the warm ECS Fargate standby. Desired routing is mirrored into Workers KV
 * (`FAILOVER_KV`) so the drill tooling and the runbook can read exactly which
 * pool is active without calling the Cloudflare API.
 *
 * The 30 s cadence is implemented with a Durable Object alarm (Cloudflare Cron
 * Triggers have a 60 s minimum). `scheduled()` and the `/__failover/check`
 * endpoint are also provided for manual drills and the weekly workflow.
 *
 * Pure helpers are exported and dependency-injectable so `node --test` can
 * exercise the whole state machine without a Workers runtime.
 */

/* ------------------------------------------------------------------------- *
 * Configuration
 * ------------------------------------------------------------------------- */

export const DEFAULT_FAILOVER_CONFIG = Object.freeze({
  intervalMs: 30_000,
  probeTimeoutMs: 5_000,
  failureThreshold: 3,
  successThreshold: 3,
  stateKey: 'failover:state',
  routingKey: 'failover:routing',
});

function envString(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function envInt(value, fallback) {
  const parsed = Number.parseInt(envString(value), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

export function resolveFailoverConfig(env = {}) {
  const bindings = env && typeof env === 'object' ? env : {};
  return {
    zoneId: envString(bindings.CF_ZONE_ID),
    loadBalancerId: envString(bindings.CF_LOAD_BALANCER_ID),
    apiToken: envString(bindings.CF_API_TOKEN),
    primaryPoolId: envString(bindings.PRIMARY_POOL_ID),
    standbyPoolId: envString(bindings.STANDBY_POOL_ID),
    primaryHealthUrl: envString(bindings.PRIMARY_HEALTH_URL) || 'https://api.tessera.xyz/health',
    intervalMs: envInt(bindings.HEALTH_CHECK_INTERVAL_MS, DEFAULT_FAILOVER_CONFIG.intervalMs),
    probeTimeoutMs: envInt(bindings.PROBE_TIMEOUT_MS, DEFAULT_FAILOVER_CONFIG.probeTimeoutMs),
    failureThreshold: envInt(bindings.FAILURE_THRESHOLD, DEFAULT_FAILOVER_CONFIG.failureThreshold),
    successThreshold: envInt(bindings.SUCCESS_THRESHOLD, DEFAULT_FAILOVER_CONFIG.successThreshold),
    stateKey: envString(bindings.FAILOVER_STATE_KEY) || DEFAULT_FAILOVER_CONFIG.stateKey,
    routingKey: envString(bindings.FAILOVER_ROUTING_KEY) || DEFAULT_FAILOVER_CONFIG.routingKey,
  };
}

export function initialHealthState() {
  return {
    active: 'primary',
    consecutiveFailures: 0,
    consecutiveSuccesses: 0,
    lastHealthy: null,
    lastCheckMs: null,
    lastTransitionMs: null,
  };
}

/* ------------------------------------------------------------------------- *
 * State machine
 * ------------------------------------------------------------------------- */

/**
 * Fold one probe result into the failover state. Failures must be consecutive
 * before traffic is shifted; successes must be consecutive before it returns,
 * so a single flaky probe cannot cause a flapping DNS change.
 */
export function evaluateHealth(state, healthy, config, now) {
  const previous = { ...initialHealthState(), ...(state || {}) };
  const consecutiveFailures = healthy ? 0 : previous.consecutiveFailures + 1;
  const consecutiveSuccesses = healthy ? previous.consecutiveSuccesses + 1 : 0;
  let active = previous.active === 'standby' ? 'standby' : 'primary';

  if (active === 'primary' && consecutiveFailures >= config.failureThreshold) {
    active = 'standby';
  } else if (active === 'standby' && consecutiveSuccesses >= config.successThreshold) {
    active = 'primary';
  }

  return {
    ...previous,
    active,
    consecutiveFailures,
    consecutiveSuccesses,
    lastHealthy: healthy,
    lastCheckMs: now,
    lastTransitionMs:
      active !== previous.active ? now : previous.lastTransitionMs,
    previousActive: active !== previous.active ? previous.active : null,
  };
}

/** Weighted pool routing written to Workers KV and applied to the LB. */
export function desiredRouting(state, config) {
  const active = state.active === 'standby' ? 'standby' : 'primary';
  return {
    active,
    weights: active === 'primary'
      ? { [config.primaryPoolId]: 100, [config.standbyPoolId]: 0 }
      : { [config.primaryPoolId]: 0, [config.standbyPoolId]: 100 },
    defaultPoolIds: active === 'primary'
      ? [config.primaryPoolId, config.standbyPoolId]
      : [config.standbyPoolId],
    fallbackPoolId: config.standbyPoolId,
  };
}

/** Cloudflare Load Balancers API PATCH body. */
export function lbPayload(routing, config) {
  return {
    default_pool_ids: routing.defaultPoolIds,
    fallback_pool_id: routing.fallbackPoolId,
    description: `Tessera DR routing: ${routing.active} (weights ${JSON.stringify(routing.weights)})`,
  };
}

/* ------------------------------------------------------------------------- *
 * I/O — all network access goes through injectable dependencies
 * ------------------------------------------------------------------------- */

export async function probePrimary(url, config, deps = {}) {
  const fetchImpl = deps.fetch || globalThis.fetch;
  if (!fetchImpl) return false;
  const separator = url.includes('?') ? '&' : '?';
  const target = `${url}${separator}dr_probe=${deps.now ?? Date.now()}`;

  let timer;
  let signal;
  if (typeof AbortSignal !== 'undefined' && typeof AbortSignal.timeout === 'function') {
    signal = AbortSignal.timeout(config.probeTimeoutMs);
  } else {
    const controller = new AbortController();
    signal = controller.signal;
    timer = setTimeout(() => controller.abort(), config.probeTimeoutMs);
  }

  try {
    const response = await fetchImpl(target, {
      method: 'GET',
      signal,
      headers: { accept: 'application/json', 'user-agent': 'tessera-dr-failover' },
    });
    return Boolean(response && response.ok);
  } catch {
    return false;
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export async function persistRouting(env, routing, config, deps = {}) {
  const fetchImpl = deps.fetch || globalThis.fetch;
  const kv = env.FAILOVER_KV;
  const result = { kvWritten: false, apiApplied: false, error: null };

  if (kv && typeof kv.put === 'function') {
    try {
      await kv.put(config.routingKey, JSON.stringify({
        ...routing,
        updatedAt: deps.now ?? Date.now(),
      }));
      result.kvWritten = true;
    } catch (error) {
      result.error = `KV write failed: ${error.message}`;
    }
  }

  if (!config.zoneId || !config.loadBalancerId || !config.apiToken) {
    result.error = result.error || 'Cloudflare LB bindings are not configured';
    return result;
  }

  try {
    const response = await fetchImpl(
      `https://api.cloudflare.com/client/v4/zones/${config.zoneId}/load_balancers/${config.loadBalancerId}`,
      {
        method: 'PATCH',
        headers: {
          authorization: `Bearer ${config.apiToken}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify(lbPayload(routing, config)),
      },
    );
    result.apiApplied = Boolean(response && response.ok);
    if (!result.apiApplied) result.error = `Cloudflare LB API returned ${response && response.status}`;
  } catch (error) {
    result.error = `Cloudflare LB API call failed: ${error.message}`;
  }
  return result;
}

/** One complete 30-second check. Safe to call from cron, DO alarm or HTTP. */
export async function runFailoverCheck(env = {}, deps = {}) {
  const config = resolveFailoverConfig(env);
  const now = deps.now ?? Date.now();
  const kv = env.FAILOVER_KV;
  let previous = initialHealthState();

  if (kv && typeof kv.get === 'function') {
    try {
      previous = { ...previous, ...(await kv.get(config.stateKey, 'json')) };
    } catch {
      // A KV read failure degrades to a fresh observation, not a failover.
    }
  }

  const healthy = await probePrimary(config.primaryHealthUrl, config, { ...deps, now });
  const state = evaluateHealth(previous, healthy, config, now);
  const routing = desiredRouting(state, config);

  if (kv && typeof kv.put === 'function') {
    try {
      await kv.put(config.stateKey, JSON.stringify(state));
    } catch {
      // Best effort; the DO also keeps its own copy of the state.
    }
  }

  let applied = { kvWritten: false, apiApplied: false, error: null };
  if (routing.active !== previous.active) {
    applied = await persistRouting(env, routing, config, { ...deps, now });
  }
  return { state, routing, healthy, applied };
}

function jsonResponse(status, payload) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'content-type': 'application/json; charset=utf-8' },
  });
}

/* ------------------------------------------------------------------------- *
 * Durable Object — true 30-second cadence via alarms
 * ------------------------------------------------------------------------- */

export class FailoverMonitor {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/check') {
      const result = await runFailoverCheck(this.env, { fetch: globalThis.fetch });
      await this.state.storage.put('health', result.state);
      await this.scheduleNext();
      return jsonResponse(200, result);
    }
    if (path === '/state') {
      return jsonResponse(200, await this.state.storage.get('health') || initialHealthState());
    }
    return jsonResponse(404, { error: 'not_found' });
  }

  async alarm() {
    const result = await runFailoverCheck(this.env, { fetch: globalThis.fetch });
    await this.state.storage.put('health', result.state);
    await this.scheduleNext();
  }

  async scheduleNext() {
    const config = resolveFailoverConfig(this.env);
    await this.state.storage.setAlarm(Date.now() + config.intervalMs);
  }
}

/* ------------------------------------------------------------------------- *
 * Worker entry points
 * ------------------------------------------------------------------------- */

async function ensureMonitor(env) {
  if (!env.FAILOVER_MONITOR) return;
  const id = env.FAILOVER_MONITOR.idFromName('tessera-primary-health');
  const stub = env.FAILOVER_MONITOR.get(id);
  await stub.fetch('https://failover.internal/alarm');
}

export default {
  async fetch(request, env) {
    const path = new URL(request.url).pathname;
    if (path === '/__failover/start') {
      await ensureMonitor(env);
      return jsonResponse(200, { started: true, intervalMs: resolveFailoverConfig(env).intervalMs });
    }
    if (path === '/__failover/check') {
      return jsonResponse(200, await runFailoverCheck(env, { fetch: globalThis.fetch }));
    }
    if (path === '/__failover/state') {
      const config = resolveFailoverConfig(env);
      const kv = env.FAILOVER_KV;
      const state = kv && kv.get ? await kv.get(config.stateKey, 'json') : null;
      return jsonResponse(200, state || initialHealthState());
    }
    return jsonResponse(404, { error: 'not_found' });
  },

  async scheduled(_controller, env, ctx) {
    // Fallback for accounts without Durable Objects, and for manual drills.
    const check = runFailoverCheck(env, { fetch: globalThis.fetch });
    if (ctx && typeof ctx.waitUntil === 'function') ctx.waitUntil(check);
    else await check;
  },
};
