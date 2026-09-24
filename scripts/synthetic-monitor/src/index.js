/**
 * Tessera Synthetic Monitoring & Alerting Bot
 * Entry Point for Standalone Daemon or One-Shot Probe Runs.
 */

import { loadConfig } from './config.js';
import { AlertStateManager } from './alerting/stateManager.js';
import { SyntheticMonitor } from './monitor.js';

async function main() {
  const config = loadConfig();
  const stateManager = new AlertStateManager({
    consecutiveFailureThreshold: config.consecutiveFailureThreshold
  });
  const monitor = new SyntheticMonitor(config, stateManager);

  const args = process.argv.slice(2);
  const isOnce = args.includes('--once') || args.includes('-1');

  console.log(`====================================================`);
  console.log(`  Tessera Synthetic Health & Alerting Bot (v1.0.0)  `);
  console.log(`====================================================`);
  console.log(`Soroban RPC URL:     ${config.rpcUrl}`);
  console.log(`Tessera API URL:     ${config.apiUrl}`);
  console.log(`Check Interval:      ${config.checkIntervalSeconds}s`);
  console.log(`Failure Threshold:   > ${config.consecutiveFailureThreshold} consecutive failures`);
  console.log(`Discord Webhook:     ${config.discordWebhookUrl ? 'Configured (Active)' : 'Disabled'}`);
  console.log(`Telegram Bot:        ${config.telegramBotToken ? 'Configured (Active)' : 'Disabled'}`);
  console.log(`Mode:                ${isOnce ? 'One-Shot Execution' : 'Continuous Daemon'}`);
  console.log(`----------------------------------------------------`);

  if (isOnce) {
    const cycle = await monitor.runCycle();
    console.log(`Summary:`, JSON.stringify(cycle, null, 2));
    process.exit(cycle.allHealthy ? 0 : 1);
  }

  // Continuous monitoring loop
  let isRunning = true;

  const shutdown = () => {
    console.log('\n[Sentinel] Received shutdown signal. Halting monitoring loop...');
    isRunning = false;
    process.exit(0);
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);

  // Initial immediate run
  await monitor.runCycle();

  while (isRunning) {
    await new Promise((resolve) => setTimeout(resolve, config.checkIntervalMs));
    if (!isRunning) break;
    await monitor.runCycle();
  }
}

main().catch((err) => {
  console.error('[Sentinel] Fatal monitor crash:', err);
  process.exit(1);
});
