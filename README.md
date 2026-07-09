# Stellar RWA — API + Docs

A combined repository with two projects for the **Stellar RWA Toolkit**:

- **`api/`** — a Rust/Axum REST API that indexes all tokenized real-world asset
  activity on Stellar (maintainer-only).
- **`docs/`** — a Next.js + MDX documentation site covering the whole platform
  (open to contributions — see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Sister repositories

- **Contracts:** https://github.com/your-org/stellar-rwa-contracts
- **Web app:** https://github.com/your-org/stellar-rwa-web

## API

A read-only REST service that indexes the four RWA contracts and serves their
state as JSON. It polls Soroban RPC every 10 seconds, rebuilds an in-memory
snapshot, and holds no keys — it never signs or submits transactions.

### Endpoints

| Method & path | Description |
|---------------|-------------|
| `GET /stats` | Platform stats: total assets, TVL, holders |
| `GET /assets` | All tokenized assets |
| `GET /assets/:id` | Full asset detail |
| `GET /assets/:id/holders` | Holder list with balances |
| `GET /assets/:id/compliance` | Allowlist summary (counts, no PII) |
| `GET /assets/:id/dividends` | Distribution history |
| `GET /health` | Liveness probe |

### Run it

```bash
cd api
cp .env.example .env      # Testnet defaults are pre-filled
cargo run                 # listens on 0.0.0.0:8080
```

```bash
curl http://localhost:8080/stats
curl http://localhost:8080/assets
```

### Tech stack

Rust · Axum · tokio · reqwest · serde · stellar-xdr · stellar-strkey.

## Docs

A documentation site built with Next.js 14 (app router) and MDX. Covers getting
started, full contract references, API references, and guides — including a
thorough **compliance guide**, the core differentiator of an RWA platform.

### Run it

```bash
cd docs
npm install
npm run dev               # http://localhost:3000
```

### Deploying to Vercel

The docs site lives in the `docs/` subdirectory. When importing this repo into
Vercel:

- **Root Directory:** `docs`
- **Framework Preset:** Next.js
- Build command and output are auto-detected.

Set `NEXT_PUBLIC_API_BASE_URL` to your deployed API URL (defaults to
`http://localhost:8080` for local development).

## Repository layout

```
api/           Rust REST API (maintainer-only)
  src/
    main.rs
    routes/    assets, holders, compliance, dividends, stats
    indexer/   Soroban RPC poller + XDR decoding + in-memory snapshot
    models/    serializable domain models
docs/          Next.js + MDX documentation site
  app/         landing + docs/** MDX pages
  components/  Sidebar, DocHeader, CodeBlock, CalloutBox, ApiEndpoint
CONTRIBUTING.md
README.md
```

## Contributing

Contributions are welcome **in `docs/` only**. The `api/` directory is
maintainer-only and PRs touching it will be closed — please open an issue instead.
See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0.
