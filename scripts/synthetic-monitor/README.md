# 🤖 Tessera End-to-End Synthetic Monitoring & Alerting Bot

A 24/7 synthetic monitoring agent that continuously validates the health, liveness, and data integrity of deployed **Stellar Soroban Testnet contracts** and the **Tessera Indexing REST API**.

---

## 🎯 Features

- **Continuous Health Probes (every 5 mins)**:
  - **Soroban RPC**: Probes `getHealth` and `getLatestLedger` to ensure node availability, protocol status, and continuous ledger closing.
  - **Soroban Contract Checks**: Queries `getEvents` for registered Testnet contracts (`registry`, `compliance`, `dividend`, `asset-token`).
  - **Tessera REST API**: Probes `GET /health`, `GET /version`, `GET /v1/stats`, and `GET /v1/assets`.
- **Indexer Gap & Stale Snapshot Detection**: Compares on-chain ledger progression against Tessera API's indexed snapshot freshness to immediately catch indexer lag or stalled polling cycles.
- **Flap-Resistant Alerting Engine**:
  - Requires **>2 consecutive failures** (configurable) before raising a critical alarm to eliminate false positives from transient network blips.
  - Generates rich, actionable **Discord Webhook embeds** and **Telegram Bot messages**.
  - Automatically dispatches **recovery notifications** when failing systems return to health.
- **Production Architecture**:
  - Zero external runtime dependencies (utilizes Node 20+ native fetch).
  - $O(1)$ space and time complexity for state evaluations.
  - Fully parallelized non-blocking async probe execution via `Promise.allSettled`.

---

## 🚀 Quickstart

### 1. Local Setup

```bash
cd scripts/synthetic-monitor
cp .env.example .env
# Edit .env with your Discord / Telegram webhooks if desired
```

### 2. Run a One-Shot Health Probe

```bash
npm run monitor:once
```

### 3. Run as Continuous Daemon

```bash
npm start
```

### 4. Run Unit Tests

```bash
npm test
```

---

## 🐳 Docker Deployment

### Using Docker Compose

```bash
cd scripts/synthetic-monitor
docker compose up -d --build
```

View logs:
```bash
docker compose logs -f
```

---

## ⚙️ Configuration Reference

| Environment Variable | Default | Description |
|---|---|---|
| `SOROBAN_RPC_URL` | `https://soroban-testnet.stellar.org` | Soroban JSON-RPC endpoint |
| `TESSERA_API_URL` | `http://localhost:8080` | Tessera REST API base URL |
| `CHECK_INTERVAL_SECONDS` | `300` (5 minutes) | Delay between probe cycles |
| `CONSECUTIVE_FAILURE_THRESHOLD` | `2` | Number of consecutive failures before alerting |
| `PROBE_TIMEOUT_MS` | `10000` (10s) | HTTP/RPC request timeout |
| `MAX_SNAPSHOT_AGE_SECONDS` | `60` | Maximum age of API snapshot before flagging stale indexer |
| `DISCORD_WEBHOOK_URL` | *None* | Discord webhook URL for alerts |
| `TELEGRAM_BOT_TOKEN` | *None* | Telegram Bot API token |
| `TELEGRAM_CHAT_ID` | *None* | Telegram destination Chat ID |
| `RWA_REGISTRY_ID` | *Testnet ID* | Registry contract ID |
| `RWA_DIVIDEND_ID` | *Testnet ID* | Dividend contract ID |
| `RWA_COMPLIANCE_ID` | *Testnet ID* | Compliance contract ID |
| `RWA_ASSET_TOKEN_ID` | *Testnet ID* | Asset Token contract ID |
