/**
 * Configuration module for Tessera Synthetic Monitor.
 * Handles environment variables with validation and defaults.
 */

export function loadConfig(env = process.env) {
  const rpcUrl = env.SOROBAN_RPC_URL || env.RWA_RPC_URL || 'https://soroban-testnet.stellar.org';
  const apiUrl = (env.TESSERA_API_URL || env.API_BASE_URL || 'http://localhost:8080').replace(/\/+$/, '');

  const checkIntervalSeconds = Math.max(
    30,
    parseInt(env.CHECK_INTERVAL_SECONDS || '300', 10) // default 5 minutes
  );

  const timeoutMs = Math.max(
    1000,
    parseInt(env.PROBE_TIMEOUT_MS || '10000', 10) // default 10s per probe
  );

  const consecutiveFailureThreshold = Math.max(
    1,
    parseInt(env.CONSECUTIVE_FAILURE_THRESHOLD || '2', 10) // alert after > 2 consecutive failures
  );

  const maxLedgerAgeSeconds = Math.max(
    60,
    parseInt(env.MAX_LEDGER_AGE_SECONDS || '120', 10) // 2 minutes max ledger age
  );

  const maxSnapshotAgeSeconds = Math.max(
    30,
    parseInt(env.MAX_SNAPSHOT_AGE_SECONDS || '60', 10) // 1 minute max snapshot age
  );

  return {
    // Network & Endpoints
    rpcUrl,
    apiUrl,

    // Timing & Thresholds
    checkIntervalSeconds,
    checkIntervalMs: checkIntervalSeconds * 1000,
    timeoutMs,
    consecutiveFailureThreshold,
    maxLedgerAgeSeconds,
    maxSnapshotAgeSeconds,

    // Webhook Alerting
    discordWebhookUrl: env.DISCORD_WEBHOOK_URL || '',
    telegramBotToken: env.TELEGRAM_BOT_TOKEN || '',
    telegramChatId: env.TELEGRAM_CHAT_ID || '',

    // Contract IDs for contract simulation probes (Testnet defaults)
    contracts: {
      registryId: env.RWA_REGISTRY_ID || 'CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3',
      complianceId: env.RWA_COMPLIANCE_ID || 'CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU',
      dividendId: env.RWA_DIVIDEND_ID || 'CAR4XY3CEBQWFOL27JEWFW34KXSIZA7RFKDQMEIV7ZU723RWY37I2SYX',
      assetTokenId: env.RWA_ASSET_TOKEN_ID || 'CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ'
    },

    // Execution environment
    environment: env.NODE_ENV || 'production',
    logLevel: env.LOG_LEVEL || 'info'
  };
}

export const config = loadConfig();
