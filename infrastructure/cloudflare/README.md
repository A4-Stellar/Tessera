# 🛡️ Tessera Edge Shield (Cloudflare Workers)

A self-contained [Cloudflare Worker](https://developers.cloudflare.com/workers/) that
sits in front of the Tessera REST API and stops abusive traffic at the edge, before
it ever reaches the origin (ECS/Cloud Run behind the ALB). It implements the edge
half of issue
[#72 — *Rate-Limiting & DDoS Shield Edge Integration*](https://github.com/A4-Stellar/Tessera/issues/72).

```
Client ──▶ Cloudflare ──▶ [ Tessera Edge Shield ] ──▶ ORIGIN_URL (Tessera API)
                                │
                                ├─ header + method validation
                                ├─ SQLi / traversal / scanner heuristics
                                ├─ edge JWT signature verification (WebCrypto)
                                ├─ per-IP sliding-window rate limit (Workers KV)
                                └─ Cache API + stale-while-revalidate
```

## What it does

| Capability | Implementation |
| --- | --- |
| **Header validation** | Method allow-list, blocked edge-bypass/smuggling headers, header count/size caps, optional required headers (`REQUIRED_HEADERS`). Only an explicit allow-list of headers is forwarded to the origin. |
| **Rate limiting by IP** | Weighted **sliding-window counter** per client IP in the `RATE_LIMIT` Workers KV namespace, with `X-RateLimit-Limit` / `-Remaining` / `-Reset` headers and `429` + `Retry-After` on abuse. |
| **JWT verification at the edge** | Compact-JWS signature checks with WebCrypto only. `HS256/384/512` via `JWT_SECRET`; `RS*` / `PS*` / `ES*` via a JWKS document at `JWT_JWKS_URL` (cached in-isolate, `JWKS_TTL_SECONDS`). Validates `exp`, `nbf`, `iss`, `aud` and rejects `alg: none`. |
| **SQLi / bot heuristics** | Named patterns over the decoded path + query string (union-select, tautologies, time delays, traversal, XSS, shell probes), plus a User-Agent block-list for known scanners (sqlmap, nuclei, nikto, …). |
| **Edge caching** | `GET /stats`, `GET /assets` (and their `/v1/...` forms) are served from the [Cache API](https://developers.cloudflare.com/workers/runtime-apis/cache/) with **stale-while-revalidate**: fresh hits are instant, stale hits return immediately while a background `ctx.waitUntil()` refreshes the entry. `ETag` / `If-None-Match` is honoured with a `304`. |
| **Deployment** | `wrangler.toml` + `deploy.sh` (KV bootstrap + tests + deploy) and npm scripts. |

Every response carries `x-request-id`, `x-tessera-edge` and (when the cache is
involved) `x-edge-cache: MISS | HIT | STALE`.

## Layout

```
infrastructure/cloudflare/
├── worker.js              # the module worker (also unit-testable outside Workers)
├── wrangler.toml          # bindings, vars and KV namespace declaration
├── deploy.sh              # KV bootstrap + tests + `wrangler deploy`
├── package.json           # npm scripts: test / dev / deploy
├── README.md              # this file
├── .dev.vars.example      # local secrets template for `wrangler dev`
└── test/worker.test.js    # 50 unit tests (node:test, no dependencies)
```

## Configuration

All bindings are optional except `ORIGIN_URL`; defaults match
[`docs/app/docs/api/rate-limits`](../../docs/app/docs/api/rate-limits/page.mdx)
(300 requests/minute per IP).

| Binding | Default | Purpose |
| --- | --- | --- |
| `ORIGIN_URL` | — (**required**) | Origin base URL the worker proxies to. |
| `RATE_LIMIT` (KV namespace) | — | Per-IP sliding-window counters. |
| `RATE_LIMIT_MAX` | `300` | Requests allowed per window, per IP. |
| `RATE_LIMIT_WINDOW_SECONDS` | `60` | Sliding-window length. |
| `RATE_LIMIT_FAIL_OPEN` | `true` | Allow traffic if KV is unavailable (`false` → `429`). |
| `ALLOWED_METHODS` | `GET,HEAD,POST,OPTIONS` | Methods the edge forwards. |
| `REQUIRED_HEADERS` | *(empty)* | Headers every request must carry. |
| `BLOCKED_BOTS` | scanner list | Override the User-Agent block-list. |
| `BOT_PROTECTION` | `true` | Toggle injection/bot heuristics. |
| `CACHE_PATHS` | `/stats,/assets,/v1/stats,/v1/assets` | Paths that use edge caching. |
| `CACHE_TTL_SECONDS` | `30` | Fresh window for a cached response. |
| `CACHE_STALE_SECONDS` | `120` | Extra window served stale while revalidating. |
| `CACHE_MAX_BYTES` | `524288` | Skip caching bodies larger than this. |
| `JWT_ALGORITHMS` | `RS256,ES256,HS256` | Algorithms the edge will verify. |
| `JWT_JWKS_URL` | — | JWKS endpoint for `RS*` / `PS*` / `ES*`. |
| `JWT_SECRET` | — | Shared secret for `HS*` (use a **secret**, not a var). |
| `JWT_ISSUER` / `JWT_AUDIENCE` | — | Required `iss` / `aud` claim values. |
| `JWT_REQUIRED_PATHS` | *(empty)* | Paths that must present a token; trailing `*` = prefix. |
| `JWT_CLOCK_SKEW_SECONDS` | `30` | Leeway applied to `exp` / `nbf`. |
| `JWT_REQUIRE_EXP` | `true` | Reject tokens without an `exp` claim. |
| `JWKS_TTL_SECONDS` | `300` | In-isolate JWKS cache lifetime. |
| `MAX_HEADER_COUNT` / `MAX_HEADER_BYTES` | `100` / `8192` | Header flood guards. |
| `MAX_URL_LENGTH` | `2048` | Reject oversized URLs with `414`. |

Secrets are never committed. Set them out-of-band:

```bash
npx wrangler secret put JWT_SECRET      # HS256 signing key
npx wrangler secret put JWT_JWKS_URL    # if you'd rather not keep it in [vars]
```

## Deploy

```bash
cd infrastructure/cloudflare

export CLOUDFLARE_API_TOKEN=...          # Workers Scripts:Edit + Workers KV:Edit
export CLOUDFLARE_ACCOUNT_ID=...         # only for multi-account tokens

./deploy.sh            # first run creates RATE_LIMIT KV, runs tests, deploys
./deploy.sh --dry-run  # bundle + validate, no account changes
./deploy.sh --skip-kv  # deploy against an already-populated KV id
```

`deploy.sh` replaces the `REPLACE_WITH_KV_NAMESPACE_ID` placeholder in
`wrangler.toml` with the real namespace id the first time it runs. If you prefer
to manage that yourself, pass `RATE_LIMIT_KV_ID=<id>` or just run:

```bash
npx wrangler kv namespace create RATE_LIMIT   # paste the id into wrangler.toml
npm test
npm run deploy
```

### Local development

```bash
cp .dev.vars.example .dev.vars   # local JWT_SECRET / ORIGIN_URL
npm run dev                      # wrangler dev on http://localhost:8787
```

### Tests

```bash
npm test        # node --test test/*.test.js — no install required
```

The 50 tests cover configuration parsing, header validation, injection/bot
heuristics, the sliding-window limiter (including window rollover and fail-open),
HS256/RS256/ES256 verification via WebCrypto, and the full request pipeline
(cache MISS/HIT/STALE, `304`, `429`, `401`, `403`, `405`, proxying). They run on
Node ≥ 20 using only `node:test`, so no Workers runtime or npm install is needed.

## Notes and limits

- **KV is eventually consistent.** The per-IP counter is enforced per edge
  location and converges globally within seconds; it is a fast shield, not an
  accountant. The account-level Cloudflare Rate Limiting ruleset in
  [`infrastructure/terraform/modules/cloudflare`](../terraform/modules/cloudflare/main.tf)
  remains the global, authoritative layer.
- **Authenticated responses are never cached.** Any request with an
  `Authorization` header bypasses the edge cache.
- **The worker fails open by default** if KV is unreachable so a KV incident
  cannot take the API down. Set `RATE_LIMIT_FAIL_OPEN=false` to fail closed.
- Terraform in `../terraform/modules/cloudflare` manages DNS/TLS/WAF; this worker
  is deployed with `wrangler` because Workers scripts are not modelled by the
  Terraform provider used here.
