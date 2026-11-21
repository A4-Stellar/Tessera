import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_FAILOVER_CONFIG,
  desiredRouting,
  evaluateHealth,
  initialHealthState,
  lbPayload,
  probePrimary,
  resolveFailoverConfig,
  runFailoverCheck,
  FailoverMonitor,
} from '../failover-worker.js';
import worker from '../failover-worker.js';

class FakeKv {
  constructor() {
    this.store = new Map();
  }

  async get(key, type) {
    const value = this.store.get(key);
    if (value === undefined) return null;
    return type === 'json' ? JSON.parse(value) : value;
  }

  async put(key, value) {
    this.store.set(key, value);
  }
}

const config = {
  ...DEFAULT_FAILOVER_CONFIG,
  primaryPoolId: 'primary-pool',
  standbyPoolId: 'standby-pool',
  zoneId: 'zone-1',
  loadBalancerId: 'lb-1',
  apiToken: 'token',
};

describe('resolveFailoverConfig', () => {
  test('applies safe defaults', () => {
    const resolved = resolveFailoverConfig({});
    assert.equal(resolved.intervalMs, 30_000);
    assert.equal(resolved.failureThreshold, 3);
    assert.equal(resolved.primaryHealthUrl, 'https://api.tessera.xyz/health');
  });

  test('reads 30s cadence from bindings', () => {
    const resolved = resolveFailoverConfig({ HEALTH_CHECK_INTERVAL_MS: '30000', FAILURE_THRESHOLD: '5' });
    assert.equal(resolved.intervalMs, 30_000);
    assert.equal(resolved.failureThreshold, 5);
  });
});

describe('evaluateHealth', () => {
  test('fails over after consecutive failures and back after consecutive successes', () => {
    let state = initialHealthState();
    state = evaluateHealth(state, false, config, 1_000);
    assert.equal(state.active, 'primary');
    state = evaluateHealth(state, false, config, 31_000);
    assert.equal(state.active, 'primary');
    state = evaluateHealth(state, false, config, 61_000);
    assert.equal(state.active, 'standby');
    assert.equal(state.previousActive, 'primary');
    assert.equal(state.lastTransitionMs, 61_000);

    state = evaluateHealth(state, true, config, 91_000);
    state = evaluateHealth(state, true, config, 121_000);
    assert.equal(state.active, 'standby');
    state = evaluateHealth(state, true, config, 151_000);
    assert.equal(state.active, 'primary');
  });

  test('a healthy probe resets the failure streak', () => {
    let state = evaluateHealth(initialHealthState(), false, config, 1_000);
    state = evaluateHealth(state, true, config, 31_000);
    assert.equal(state.consecutiveFailures, 0);
    assert.equal(state.active, 'primary');
  });
});

describe('routing', () => {
  test('primary routing keeps both pools weighted to primary', () => {
    const routing = desiredRouting(initialHealthState(), config);
    assert.equal(routing.active, 'primary');
    assert.deepEqual(routing.weights, { 'primary-pool': 100, 'standby-pool': 0 });
    assert.deepEqual(routing.defaultPoolIds, ['primary-pool', 'standby-pool']);
    assert.equal(lbPayload(routing, config).default_pool_ids[0], 'primary-pool');
  });

  test('standby routing sends all traffic to the warm standby', () => {
    const routing = desiredRouting({ ...initialHealthState(), active: 'standby' }, config);
    assert.deepEqual(routing.weights, { 'primary-pool': 0, 'standby-pool': 100 });
    assert.deepEqual(routing.defaultPoolIds, ['standby-pool']);
  });
});

describe('probePrimary', () => {
  test('accepts 2xx and rejects 5xx without throwing', async () => {
    const ok = await probePrimary('https://api/health', config, {
      fetch: async () => ({ ok: true, status: 200 }),
      now: 123,
    });
    const bad = await probePrimary('https://api/health', config, {
      fetch: async () => ({ ok: false, status: 503 }),
      now: 123,
    });
    const thrown = await probePrimary('https://api/health', config, {
      fetch: async () => { throw new Error('offline'); },
      now: 123,
    });
    assert.equal(ok, true);
    assert.equal(bad, false);
    assert.equal(thrown, false);
  });
});

describe('runFailoverCheck', () => {
  test('applies routing only on transition and mirrors it to Workers KV', async () => {
    const kv = new FakeKv();
    const patches = [];
    // Three consecutive failed probes (failureThreshold = 3).
    const failedProbes = [true, true, true];
    const env = {
      ...config,
      FAILOVER_KV: kv,
      CF_ZONE_ID: 'zone-1',
      CF_LOAD_BALANCER_ID: 'lb-1',
      CF_API_TOKEN: 'token',
      PRIMARY_POOL_ID: 'primary-pool',
      STANDBY_POOL_ID: 'standby-pool',
    };
    const deps = {
      fetch: async (url, init) => {
        if (init.method === 'PATCH') {
          patches.push(JSON.parse(init.body));
          return { ok: true, status: 200 };
        }
        return { ok: !failedProbes.shift(), status: 200 };
      },
    };

    await runFailoverCheck(env, { ...deps, now: 1_000 });
    await runFailoverCheck(env, { ...deps, now: 31_000 });
    const third = await runFailoverCheck(env, { ...deps, now: 61_000 });

    assert.equal(third.state.active, 'standby');
    assert.equal(patches.length, 1);
    assert.equal(patches[0].default_pool_ids[0], 'standby-pool');
    const stored = JSON.parse(await kv.get(config.routingKey));
    assert.equal(stored.active, 'standby');
    assert.deepEqual(stored.weights, { 'primary-pool': 0, 'standby-pool': 100 });
  });
});

describe('HTTP endpoints', () => {
  const env = { FAILOVER_ADMIN_TOKEN: 'secret-token' };
  const req = (path, token) =>
    new Request(`https://dr.example${path}`, token ? { headers: { authorization: `Bearer ${token}` } } : {});

  test('reject requests without the admin token', async () => {
    const res = await worker.fetch(req('/__failover/check'), env);
    assert.equal(res.status, 401);
    const wrong = await worker.fetch(req('/__failover/check', 'nope'), env);
    assert.equal(wrong.status, 401);
  });

  test('fail closed when no admin token is configured', async () => {
    const res = await worker.fetch(req('/__failover/state', 'anything'), {});
    assert.equal(res.status, 401);
  });

  test('accept the admin token', async () => {
    const res = await worker.fetch(req('/__failover/state', 'secret-token'), env);
    assert.equal(res.status, 200);
  });

  test('the monitor accepts the /alarm path used by /__failover/start', async () => {
    let alarmAt = null;
    const storage = { put: async () => {}, get: async () => null, setAlarm: async (t) => { alarmAt = t; } };
    const monitor = new FailoverMonitor({ storage }, { PRIMARY_HEALTH_URL: 'https://p.example/health' });
    const original = globalThis.fetch;
    globalThis.fetch = async () => ({ ok: true, status: 200 });
    try {
      const res = await monitor.fetch(new Request('https://failover.internal/alarm'));
      assert.equal(res.status, 200);
      assert.ok(alarmAt, 'next alarm was scheduled');
    } finally {
      globalThis.fetch = original;
    }
  });
});
