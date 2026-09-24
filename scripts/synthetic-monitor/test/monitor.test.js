import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import { AlertStateManager } from '../src/alerting/stateManager.js';
import { loadConfig } from '../src/config.js';
import { evaluateIndexerGap } from '../src/probes/gapDetector.js';

describe('AlertStateManager', () => {
  test('does not alert on failure below threshold', () => {
    const manager = new AlertStateManager({ consecutiveFailureThreshold: 2 });

    const res1 = manager.update({ name: 'test_probe', success: false, error: 'fail 1' });
    assert.equal(res1.shouldAlert, false);
    assert.equal(res1.status, 'warning');
    assert.equal(res1.consecutiveFailures, 1);

    const res2 = manager.update({ name: 'test_probe', success: false, error: 'fail 2' });
    assert.equal(res2.shouldAlert, false);
    assert.equal(res2.status, 'warning');
    assert.equal(res2.consecutiveFailures, 2);
  });

  test('alerts once consecutive failures exceed threshold (> 2)', () => {
    const manager = new AlertStateManager({ consecutiveFailureThreshold: 2 });

    manager.update({ name: 'test_probe', success: false, error: 'fail 1' });
    manager.update({ name: 'test_probe', success: false, error: 'fail 2' });
    const res3 = manager.update({ name: 'test_probe', success: false, error: 'fail 3' });

    assert.equal(res3.shouldAlert, true);
    assert.equal(res3.status, 'critical');
    assert.equal(res3.consecutiveFailures, 3);

    // Subsequent failures do not re-trigger initial alert unless recovered
    const res4 = manager.update({ name: 'test_probe', success: false, error: 'fail 4' });
    assert.equal(res4.shouldAlert, false);
    assert.equal(res4.consecutiveFailures, 4);
  });

  test('fires recovery alert when previously alerting probe returns to healthy', () => {
    const manager = new AlertStateManager({ consecutiveFailureThreshold: 2 });

    manager.update({ name: 'test_probe', success: false, error: 'fail 1' });
    manager.update({ name: 'test_probe', success: false, error: 'fail 2' });
    manager.update({ name: 'test_probe', success: false, error: 'fail 3' });

    const recoveryRes = manager.update({ name: 'test_probe', success: true });
    assert.equal(recoveryRes.shouldAlert, true);
    assert.equal(recoveryRes.isRecovery, true);
    assert.equal(recoveryRes.status, 'recovered');
    assert.equal(recoveryRes.previousError, 'fail 3');

    // Subsequent success is normal healthy
    const nextSuccess = manager.update({ name: 'test_probe', success: true });
    assert.equal(nextSuccess.shouldAlert, false);
    assert.equal(nextSuccess.isRecovery, false);
    assert.equal(nextSuccess.status, 'healthy');
  });
});

describe('Config Loader', () => {
  test('uses sensible defaults when env vars are absent', () => {
    const config = loadConfig({});
    assert.equal(config.rpcUrl, 'https://soroban-testnet.stellar.org');
    assert.equal(config.apiUrl, 'http://localhost:8080');
    assert.equal(config.checkIntervalSeconds, 300);
    assert.equal(config.consecutiveFailureThreshold, 2);
  });

  test('overrides with custom environment variables', () => {
    const config = loadConfig({
      SOROBAN_RPC_URL: 'https://custom-rpc.example.com',
      TESSERA_API_URL: 'https://api.tessera.xyz/',
      CHECK_INTERVAL_SECONDS: '600',
      CONSECUTIVE_FAILURE_THRESHOLD: '3',
      DISCORD_WEBHOOK_URL: 'https://discord.com/api/webhooks/123/xyz'
    });

    assert.equal(config.rpcUrl, 'https://custom-rpc.example.com');
    assert.equal(config.apiUrl, 'https://api.tessera.xyz');
    assert.equal(config.checkIntervalSeconds, 600);
    assert.equal(config.consecutiveFailureThreshold, 3);
    assert.equal(config.discordWebhookUrl, 'https://discord.com/api/webhooks/123/xyz');
  });
});

describe('Indexer Gap Detector', () => {
  test('passes when snapshot is fresh', () => {
    const rpcHealth = { success: true };
    const rpcLedger = { success: true, details: { sequence: 100000 } };
    const apiHealth = { success: true, details: { snapshot_age_seconds: 15 } };
    const apiStats = { success: true, details: { lastUpdated: new Date().toISOString() } };

    const result = evaluateIndexerGap(rpcHealth, rpcLedger, apiHealth, apiStats, { maxSnapshotAgeSeconds: 60 });
    assert.equal(result.success, true);
    assert.equal(result.details.healthy, true);
  });

  test('flags failure when snapshot age exceeds threshold', () => {
    const rpcHealth = { success: true };
    const rpcLedger = { success: true, details: { sequence: 100000 } };
    const apiHealth = { success: true, details: { snapshot_age_seconds: 120 } };
    const apiStats = { success: true, details: { lastUpdated: new Date(Date.now() - 120000).toISOString() } };

    const result = evaluateIndexerGap(rpcHealth, rpcLedger, apiHealth, apiStats, { maxSnapshotAgeSeconds: 60 });
    assert.equal(result.success, false);
    assert.match(result.error, /stale/);
  });
});
