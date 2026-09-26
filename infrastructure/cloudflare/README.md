# Tessera Cloudflare Worker

This Worker validates request shape and host headers, blocks common scanner and scraper user agents and SQL injection payloads, applies a per-IP Cloudflare Rate Limiting binding, and verifies supplied JWTs using HS256. It proxies requests to the configured API origin. Bearer tokens are validated when present so existing public API clients remain compatible; do not use this optional-token policy for routes that require authentication.

`GET /stats`, `GET /assets`, and their `/v1` forms are cached only when the request has no Authorization or Cookie header. The Worker serves fresh data for 30 seconds and serves stale data for up to 60 additional seconds while refreshing in the background. A failed refresh can fall back to cached data for up to five minutes. Cloudflare's Cache API does not implement `stale-while-revalidate` directly, so the Worker implements those windows itself.

## Configure and deploy

1. Install Node.js 20+, npm, and authenticate Wrangler with `npx wrangler login` (or set `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`).
2. Edit `wrangler.toml`: set `API_ORIGIN` to the API's direct origin hostname, set the API route and zone, and set `ALLOWED_HOSTS`. The origin must not resolve back through this Worker.
3. From this directory, run `npm run check:deploy` and `npm run deploy`. `deploy.ps1` runs both commands on Windows.
4. Set a strong shared HS256 signing secret with `npm run secret:jwt`. The Worker will reject bearer tokens until this secret is configured. Set `JWT_ISSUER` and `JWT_AUDIENCE` to match the issuer and audience in tokens your API accepts.
5. Run `npm test` for the local Worker checks.

The rate limit is 120 requests per IP per 60 seconds per Cloudflare location. Cloudflare's Worker Rate Limiting binding is intentionally approximate and local to each data center; retain the zone-level WAF/rate limiting rules for broader volumetric protection.

## Environment and secret

| Name | Type | Purpose |
| --- | --- | --- |
| `API_ORIGIN` | Wrangler variable | Direct upstream API origin |
| `ALLOWED_HOSTS` | Wrangler variable | Comma-separated accepted request hostnames |
| `JWT_ISSUER` / `JWT_AUDIENCE` | Wrangler variables | Optional JWT claim checks |
| `JWT_SECRET` | Wrangler secret | HS256 signing key; never commit it |
| `IP_RATE_LIMIT` | Rate limiting binding | Per-IP request limit |
