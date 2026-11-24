# Changelog

All notable changes to this repository are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

- **Sandbox (infrastructure/dx):** isolated `tessera sandbox start` stack with
  Stellar quickstart, all contracts, a Postgres database seeded by a Rust
  datagen binary (Pareto balances, log-normal prices, Poisson dividend gaps),
  `X-Sandbox-Tenant-ID` row-level security, a sub-5-second `POST /sandbox/reset`,
  a relaxed 1,000 req/s bucket and examples for all seven SDKs.
- **Token economics (api/docs):** Rust WebAssembly agent-based simulator with
  institutional, retail and arbitrageur agents, a Web Worker with progress
  events, D3 charts, JSON scenario export/import, an MDX page and numerical
  stability tests.
- **EigenDA (api/contracts):** zstd-compressed state snapshots dispersed with an
  80% quorum, confirmation polling, Postgres proof storage,
  `GET /v1/assets/:id/da-proof`, an MMR↔DA anchor and a Soroban BLS verifier
  with stake-weighted quorum checks.
- **WASM fuzzing (contracts/fuzz):** mock host/storage oracle, structure-aware
  ABI mutation, contract-WASM execution, 300 s CI runs with coverage artifacts.
- **Fixed:** compile errors and missing admin guards in `debt-token` and
  `bridge`, uncovered by the new WASM harness.

## [0.1.0] — 2026-07-09

Initial release: the indexing REST API and the documentation site.

### API

- Rust/Axum service indexing tokenized RWA activity on Stellar.
- Soroban RPC indexer polling every 10 seconds, decoding `ScVal` returns to JSON
  and rebuilding an in-memory snapshot (no database).
- Endpoints: `GET /stats`, `/assets` (with `?asset_type` and `?active` filters),
  `/assets/:id`, `/assets/:id/holders`, `/assets/:id/compliance`,
  `/assets/:id/dividends`, `/health`.
- Non-PII compliance summaries (counts and jurisdiction breakdown only).
- Graceful shutdown, request tracing, CORS, and friendly error handling — no
  panics on transient RPC failures.
- Unit tests for `ScVal`→JSON decoding and formatting helpers.
- Multi-stage Dockerfile.

### Docs

- Next.js 14 + MDX documentation site.
- Getting Started; full contract references (asset-token, compliance, registry,
  dividend); API references (overview, assets, holders, compliance, dividends);
  and a thorough Compliance Guide, Web App Guide, and Integration Guide.
- Sidebar navigation, prev/next, callouts, API endpoint banners, robots and
  sitemap, and OpenGraph metadata.

### CI

- GitHub Actions for the API (fmt, clippy, test, release build) and the docs
  (Next.js build), each path-filtered to its own project.
