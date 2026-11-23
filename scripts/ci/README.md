# Soroban WASM build cache

`scripts/ci/check-wasm-cache.sh` is a content-addressed build cache for the
Soroban contracts in `contracts/`. It makes repeat contract builds finish in
seconds on a cache hit (instead of a full ~8 minute WASM compile) and can
verify that two independent runners produce **byte-identical** WASM.

The companion `scripts/ci/test_check_wasm_cache.sh` is a self-contained test
harness (fake `cargo` on `PATH`; no `aws`/`gsutil` required).

## How it works

1. **Content hash.** The script computes a single 64-hex SHA-256 over:
   - every contract source file (`*.rs`, `Cargo.toml`, `Cargo.lock`,
     `rust-toolchain.toml`) as a *(relative path, byte size, contents)* tuple,
     sorted byte-wise by path — so the hash is independent of filesystem
     ordering and of the absolute checkout location;
   - the output of `rustc --version`;
   - the exact `CARGO_FLAGS` string.

   The cache key is exactly `soroban-wasm-<hash>` by default (the prefix is
   configurable via `CACHE_KEY_PREFIX`). The key is printed on every run.

2. **Lookup.** The local cache at `<CARGO_TARGET_DIR>/.wasm-cache/<key>` is
   checked first (fast, works offline and is restored by `actions/cache`). If
   `CACHE_BUCKET` is set and the bucket tools are available, the bucket is
   consulted as a fallback via `aws s3 cp --recursive` / `gsutil -m cp -r`.

3. **Hit:** cached `*.wasm` files are copied into `WASM_OUT_DIR`, compilation is
   skipped, and a hit line is appended to the GitHub step summary.

4. **Miss:** `cargo build $CARGO_FLAGS --locked --manifest-path
   $CONTRACTS_DIR/Cargo.toml` runs (with a stable `CARGO_TARGET_DIR`), the
   resulting `*.wasm` files are mirrored into the local cache and pushed to the
   bucket with an expiry-date tag.

5. **Metrics:** every run appends one entry to the history log; a hit rate is
   computed and written to the metrics JSON and the step summary.

If bucket tools are missing or the bucket is unavailable, the script silently
degrades to the local cache — CI never fails just because the remote cache is
down.

## Usage

```bash
bash scripts/ci/check-wasm-cache.sh --help

# Local-only cache (no bucket configured)
bash scripts/ci/check-wasm-cache.sh

# With an S3 bucket and reproducibility verification
CACHE_BUCKET=s3://my-bucket/soroban-wasm WASM_REPRODUCIBILITY_CHECK=1 \
  bash scripts/ci/check-wasm-cache.sh
```

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `CONTRACTS_DIR` | `contracts` | Source root containing the workspace `Cargo.toml` |
| `CACHE_BUCKET` | empty (local only) | `s3://bucket/prefix` or `gs://bucket/prefix` |
| `CACHE_KEY_PREFIX` | `soroban-wasm` | Prefix of the cache key (`<prefix>-<hash>`) |
| `CARGO_FLAGS` | `--release --target wasm32-unknown-unknown` | Exact flags passed to `cargo build` (part of the hash) |
| `RETENTION_DAYS` | `30` | Days until a pushed object expires |
| `GITHUB_STEP_SUMMARY` | empty | File to append markdown summary lines to |
| `METRICS_FILE` | `.wasm-cache-metrics.json` | JSON metrics output path |
| `WASM_OUT_DIR` | `<CARGO_TARGET_DIR>/wasm32-unknown-unknown/release` | Where built/restored `.wasm` files live |
| `CARGO_TARGET_DIR` | `<CONTRACTS_DIR>/target` | Stable cargo target directory |
| `WASM_REPRODUCIBILITY_CHECK` | `0` | `1` to verify byte-identical rebuilds |
| `SOURCE_DATE_EPOCH` | `1` | Fixed timestamp for reproducible metadata |

### Selector mode

`WASM_CACHE_SELFTEST=1 bash scripts/ci/check-wasm-cache.sh` prints only the
cache key and exits. The test harness uses this to derive expected cache paths.

## Local cache, history, and `.gitignore`

The local cache, history log, and reproducibility build dirs all live under
`CARGO_TARGET_DIR` (by default `contracts/target`), which is already covered by
the `target/` / `**/target/` rules in the repository `.gitignore`:

- `contracts/target/.wasm-cache/<key>/` — cached `*.wasm`
- `contracts/target/.wasm-cache-history.jsonl` — append-only hit/miss log (last
  200 entries retained)
- `contracts/target-repro-a/`, `contracts/target-repro-b/` — temporary
  reproducibility build dirs

The first two are covered by the existing `target/` rules. The reproducibility
dirs are named `<CARGO_TARGET_DIR>-repro-{a,b}`, which with the defaults become
`contracts/target-repro-a` / `contracts/target-repro-b` — these are **not**
matched by a plain `target/` rule. Either leave `WASM_REPRODUCIBILITY_CHECK=0`
locally (the default) or add the following lines to `.gitignore` if you enable
it locally:

```gitignore
contracts/target-repro-a/
contracts/target-repro-b/
.wasm-cache-metrics.json
```

The script itself never edits `.gitignore`; this repository currently does not
need those extra rules because the feature runs in CI and the reproducibility
dirs are cleaned up there. Set `METRICS_FILE` to a path under `target/` (for
example `contracts/target/.wasm-cache-metrics.json`) to keep local metrics out
of the worktree too.

## Bucket setup

The bucket is optional. Configure it through the repository variable
`WASM_CACHE_BUCKET` (for example `s3://tessera-build-cache/soroban-wasm` or
`gs://tessera-build-cache/soroban-wasm`). Credentials are **never** hardcoded:
use OIDC (preferred) or the secrets `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY` / `AWS_REGION` (for S3). The CI job only exposes those
secrets when `WASM_CACHE_BUCKET` is non-empty.

### S3

```bash
aws s3 mb s3://tessera-build-cache
# Objects are written under s3://tessera-build-cache/soroban-wasm/<key>/.
# Grant the CI role/OIDC principal s3:GetObject, s3:PutObject, s3:ListBucket.
```

### GCS

```bash
gsutil mb gs://tessera-build-cache
# Objects are written under gs://tessera-build-cache/soroban-wasm/<key>/.
# Grant the CI service account roles/storage.objectAdmin on the bucket.
```

### 30-day expiry

`cache_push` tags every upload with `expiry-date=<now + RETENTION_DAYS>` (an S3
object tag and metadata; a `x-goog-meta-expiry-date` custom header on GCS).
Tags alone do not delete objects, so configure a **bucket lifecycle rule** to
enforce the 30-day retention. Example S3 lifecycle configuration:

```json
{
  "Rules": [
    {
      "ID": "expire-soroban-wasm-cache",
      "Status": "Enabled",
      "Filter": { "Prefix": "soroban-wasm/" },
      "Expiration": { "Days": 30 }
    }
  ]
}
```

```bash
aws s3api put-bucket-lifecycle-configuration \
  --bucket tessera-build-cache \
  --lifecycle-configuration file://lifecycle.json
```

For GCS:

```bash
gsutil lifecycle set lifecycle.json gs://tessera-build-cache
# {"rule":[{"action":{"type":"Delete"},"condition":{"age":30,
#    "matchesPrefix":["soroban-wasm/"]}}]}
```

`RETENTION_DAYS` controls the value of the `expiry-date` tag; keep the lifecycle
rule's `Days` in sync with it.

## Reproducibility (issue #47)

Set `WASM_REPRODUCIBILITY_CHECK=1` to enable verification:

- On a **miss**, after the primary build the script performs two additional
  *independent* builds in freshly cleaned `CARGO_TARGET_DIR`s
  (`<target>-repro-a` and `<target>-repro-b`) and `cmp`s every `.wasm` between
  them and against the primary build. Any byte difference fails the job with a
  clear message.
- On a **hit**, the restored artifact is compared byte-for-byte against a fresh
  rebuild.

All builds use `SOURCE_DATE_EPOCH=1` and `--locked`, and select
[`contracts/rust-toolchain.toml`](../../contracts/rust-toolchain.toml) as the
pinned toolchain. Reproducible WASM **requires** that pinned toolchain and the
locked dependency graph; a different compiler version changes the hash (so it
also produces a different cache key) and can change the emitted bytes. A
successful check appends a `🔬 Reproducibility PASS ...` line to the summary.

## Hit rate & metrics

Each run appends a compact JSON line to
`<CARGO_TARGET_DIR>/.wasm-cache-history.jsonl`, trims the log to the last 200
entries, and writes a metrics object to `METRICS_FILE` (default
`.wasm-cache-metrics.json`):

```json
{"result":"hit","key":"soroban-wasm-…","hash":"…","hit_rate":75,"hits":3,"total":4}
```

The summary gets a line like `📊 WASM cache hit rate: 75% (3 hits / 4 builds)`.
The `wasm-build` CI job has a final aggregation step that reads this file and
repeats the hit-rate line in the job summary even when the build step fails.

## CI job

`.github/workflows/ci.yml` defines the `wasm-build` job (gated by the existing
`changes` path filter, like the other Rust jobs). It restores the cargo
registry and the shared `contracts/target/.wasm-cache` directory with
`actions/cache@v4`, runs this script with `CARGO_FLAGS`,
`CACHE_BUCKET` (`vars.WASM_CACHE_BUCKET`) and
`WASM_REPRODUCIBILITY_CHECK` (`vars.WASM_REPRODUCIBILITY_CHECK`), uploads the
`.wasm` files as an artifact, and aggregates the metrics. The GitHub Actions
cache key is intentionally coarse (`hashFiles(...)`); the bucket-backed cache
uses the tighter content hash computed by the script.

## Tests

```bash
bash -n scripts/ci/check-wasm-cache.sh
bash -n scripts/ci/test_check_wasm_cache.sh
bash scripts/ci/test_check_wasm_cache.sh
```

The harness verifies hash determinism (identical trees in different
directories/creation orders hash equally; editing a file or `CARGO_FLAGS`
changes the hash), the key format, the HIT path (local cache pre-seeded, fake
`cargo` sentinel proves compilation was skipped, wasm restored, metrics
written), the MISS path (cargo called, local cache seeded), and the
reproducibility pass path.
