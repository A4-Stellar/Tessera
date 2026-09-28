# Tessera Status Page

A standalone, lightweight operational status dashboard for the Tessera platform.

Monitors:
- **Tessera REST API** — `https://api.tessera.finance`
- **Soroban Testnet RPC** — `https://soroban-testnet.stellar.org`
- **Tessera Documentation** — `https://tessera.finance`

## Features

- Real-time status probes every 60 seconds (auto-refresh)
- Per-service latency measurement
- 7-day, 30-day, and 90-day uptime percentage bars
- 90-day daily uptime history chart (color-coded: green/amber/red)
- Active incident banners
- Incident history log
- RSS/Atom incident feed at `/api/rss`
- Email & webhook subscription modal
- Fully dark-themed, accessible (WCAG 2.1 AA)

## Getting Started

```bash
cd status-page
npm install
cp .env.example .env.local   # fill in endpoint URLs
npm run dev                  # http://localhost:3001
```

## Environment Variables

Create a `.env.local` file:

```env
# Monitored endpoint URLs (defaults work for testnet)
TESSERA_API_URL=https://api.tessera.finance/health
SOROBAN_RPC_URL=https://soroban-testnet.stellar.org
TESSERA_DOCS_URL=https://tessera.finance

# Public base URL for RSS feed links
NEXT_PUBLIC_STATUS_URL=https://status.tessera.finance
```

## Build & Deploy

```bash
npm run build     # production build
npm start         # serve production build on :3001
```

### Deploy to Vercel

Import this subdirectory into Vercel:

- **Root Directory:** `status-page`
- **Framework Preset:** Next.js
- Set `NEXT_PUBLIC_STATUS_URL` to your deployed status page URL.

### Deploy to Docker

```bash
docker build -t tessera-status-page .
docker run -p 3001:3001 \
  -e TESSERA_API_URL=https://api.tessera.finance/health \
  -e SOROBAN_RPC_URL=https://soroban-testnet.stellar.org \
  tessera-status-page
```

## Architecture

```
status-page/
├── src/
│   ├── app/
│   │   ├── layout.tsx          Root layout (dark theme, RSS link)
│   │   ├── page.tsx            Main dashboard (client component, auto-refresh)
│   │   ├── globals.css         Tailwind base + custom animations
│   │   └── api/
│   │       ├── status/route.ts  Server-side probe endpoint (GET /api/status)
│   │       └── rss/route.ts     RSS 2.0 incident feed (GET /api/rss)
│   ├── components/
│   │   ├── StatusBadge.tsx     Status dot + overall banner
│   │   ├── UptimeGraph.tsx     90-day bar chart + uptime % pills
│   │   ├── IncidentBanner.tsx  Active incident alerts + history
│   │   └── SubscribeModal.tsx  Email/webhook subscription form
│   └── lib/
│       ├── monitor.ts          Probe logic, types, uptime calculation
│       └── notifications.ts   Webhook delivery, email formatting, RSS generation
├── package.json
├── tailwind.config.ts
└── next.config.mjs
```

## Integrating Notifications

### Webhooks

`POST /api/subscribe` (not yet implemented — stub in `SubscribeModal`) should
persist subscriber URLs and call `sendWebhookNotification` from `src/lib/notifications.ts`
when incidents are created or updated.

Webhook payload shape:

```json
{
  "event": "incident_created",
  "incident": { ... },
  "timestamp": "2026-09-28T00:00:00Z",
  "source": "tessera-status-page"
}
```

### Email

Pass a generated `EmailPayload` (from `generateEmailPayload()`) to your email provider
(SendGrid, AWS SES, Resend, etc.).

### RSS

Subscribe to the Atom-compatible RSS 2.0 feed at `/api/rss` in any feed reader.
Auto-discovered via `<link rel="alternate" ...>` in the page `<head>`.
