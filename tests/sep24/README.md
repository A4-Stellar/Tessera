# SEP-24 compliance test harness

This directory holds the configuration and smoke checks for the Tessera
implementation of **SEP-24 (Hosted Deposit and Withdrawal)**.

> The full [Stellar `sep-tests`](https://github.com/stellar/stellar-protocol/tree/master/ecosystem/sep-0024)
> conformance suite is an **external** suite that is cloned at runtime. This
> harness does not vendor it: it provides the config (`sep24-config.toml`) and
> a dependency-free smoke test (`smoke.test.mjs`) that verifies the endpoints
> the external suite will exercise. Run `sep-tests` separately against the same
> local API once the smoke test passes.

## Start the API in test mode

From the repository root:

```bash
export SEP24_TEST_MODE=1                 # stub the compliance allowlist hook
export SEP24_NETWORK=testnet
export SEP24_DOCS_URL=http://localhost:3000
export SEP24_SIGNING_SERVICE_URL=        # optional: leave unset for placeholder XDR
export SEP24_REVERIFY_INTERVAL_DAYS=365
cargo run --manifest-path api/Cargo.toml --bin tessera-api
```

`SEP24_TEST_MODE=1` makes `register_allowlist_entry` return `registered`
without calling an external compliance service. It exists only for this local
harness — never enable it in production. Without it, KYC falls back to
`needs_review` (or calls `COMPLIANCE_SERVICE_URL`, when configured).

## Environment variables

| Variable | Purpose |
|----------|---------|
| `SEP24_BASE_URL` | API base URL for the smoke test. **When unset the test skips.** |
| `SEP24_TEST_ACCOUNT` | Stellar `G...` account used by the smoke test (defaults to the API's demo key). |
| `SEP24_TEST_ASSET` | Asset code (default `USDC`). |
| `SEP24_TEST_MODE` | Set to `1` on the API to stub compliance registration. |
| `SEP24_DOCS_URL` | Base URL used to build interactive deposit links. |
| `SEP24_SIGNING_SERVICE_URL` | Optional external signer returning `{"signed_transaction": "<base64 XDR>"}`. |
| `COMPLIANCE_SERVICE_URL` | Optional `add_to_allowlist` HTTP hook. |
| `SEP24_REVERIFY_INTERVAL_DAYS` | KYC re-verification age threshold (default `365`). |

`sep24-config.toml` mirrors these values for the external harness. Update the
`[asset].issuer` placeholder with a real testnet issuer before a live run.

## Run the smoke test

```bash
SEP24_BASE_URL=http://localhost:8080 node --test tests/sep24/smoke.test.mjs
# syntax only:
node --check tests/sep24/smoke.test.mjs
```

Node 18+ is required (`node:test` + global `fetch`). No npm install is needed.

### What is covered

- `GET /.well-known/stellar.toml` — content type and required SEP-24 fields.
- `POST /sep24/transactions/deposit/interactive` — interactive `url`/`type`,
  one-time token, and initial `pending_user_transfer_start` status.
- `POST /sep24/transactions/:id/kyc` — KYC storage and allowlist hook outcome.
- `POST /sep24/transactions/:id/confirm_external` — transition to
  `pending_external`.
- `POST /sep24/transactions/:id/complete` — transition to `completed` and
  attached `signed_transaction` (placeholder by default).
- `GET /sep24/transaction` — token check (`401` on a bad token) and envelope.
- `GET /sep24/transactions` — per-account listing.
- `GET /sep24/admin/reverify/due` and `POST /sep24/admin/reverify` — issue #77
  hook.
- Validation — a malformed account returns `400`.

## Caveats

The Rust API's transaction store is **in-memory and volatile**. A restart
clears all deposits, so run the smoke suite against a fresh API process and do
not use this harness for durability or load testing. The default signer returns
`BASE64_XDR_PLACEHOLDER`, which is not valid XDR; the UI deliberately never
submits it to Stellar.
