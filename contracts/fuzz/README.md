# Fuzzing and property tests

Fuzzing infrastructure for the Soroban contract suite. This directory is a
self-contained sub-tree of three independent Cargo packages, none of which are
members of the `contracts` workspace (see `exclude` in `contracts/Cargo.toml`):

| Package            | Toolchain | What it is                                                     |
| ------------------ | --------- | -------------------------------------------------------------- |
| `fuzz/`            | nightly   | 11 libFuzzer targets driven by `cargo fuzz`                     |
| `fuzz/harness/`    | stable    | Shared input decoding, extreme values, and the panic oracle     |
| `fuzz/proptests/`  | stable    | `proptest` properties that shrink to readable counterexamples   |

## Why both tools

They answer different questions and neither subsumes the other.

`cargo fuzz` is good at volume. It will run hundreds of millions of executions
and find the argument combination nobody imagined. It is bad at *explaining*: a
crash arrives as a 40-byte blob, and libFuzzer's minimisation is guided by
coverage rather than by meaning.

`proptest` is good at explanation. A failure prints `minimum failing input` in
the domain of the strategy — `transfer i128::MIN from holder 1 to holder 3` —
which is directly actionable. It explores far fewer cases.

So the CI runs the property tests on every pull request as a hard gate, and the
nightly 30-minute-per-target libFuzzer runs as the search. The nightly runs are
also a regression net: a corpus cached between runs means coverage accumulates
over weeks rather than restarting nightly.

## Running locally

Property tests (stable, fast):

```bash
cd contracts/fuzz/proptests
cargo test
```

A specific property, keeping the counterexample:

```bash
cargo test --release --test tax_withholding -- --nocapture
```

Fuzzing (needs nightly and `cargo-fuzz`):

```bash
cargo install cargo-fuzz
cd contracts/fuzz
cargo fuzz run transfer_extremes
```

A single target, time-boxed, with a minimized repro written on a finding:

```bash
cargo fuzz run bridge -- -max_total_time=300 -print_final_stats=1
```

`cargo fuzz` does not stop on its own — without `-max_total_time` it runs until
you interrupt it. Fuzz output lands in `fuzz/artifacts/<target>/`; commit
nothing from there, attach it to the issue instead.

## The panic oracle

The central design decision is in `harness/src/lib.rs`. In a native Soroban
test environment the host reports a bare `panic!` as `InvokeError::Abort`, and
an arithmetic overflow, a `.unwrap()` on `None`, and an out-of-bounds index all
look exactly the same. A fuzzer that only checked "did it return an error" would
therefore miss every overflow bug in this codebase, and would equally flag every
*deliberate* refusal as a crash.

So contracts declare their rejections with `#[contracterror]`, and the harness
separates the two channels:

- `succeeded(&res)` — `Ok(Ok(_))`. A value came back.
- `declared_error(&res)` — `Err(Ok(_))`. One of the contract's declared
  `#[contracterror]` variants. A deliberate refusal.
- `failed_with(&res, expected)` — refused with *that specific* error.
- `check_no_trap(&res, ctx)` — panics on `InvokeError::Abort` or on an
  undeclared error code, i.e. a genuine finding.

`failed_with` exists because `!succeeded()` is a weak oracle. An already-processed
message, an expired TTL and a paused contract will all refuse a call, so a
negative test written as `assert!(!succeeded(&res))` passes even when the check
under test never ran. Several targets in this directory were wrong in exactly
that way before being tightened; the comments at each site say so.

## Targets

| Target                        | Contract                       | Focus                                              |
| ----------------------------- | ------------------------------ | -------------------------------------------------- |
| `asset_token`                 | asset-token                    | Supply/balance conservation, lockups, pause, clawback |
| `transfer_extremes`           | asset-token                    | `i128::MIN`/`MAX` transfers, mints, burns, clawbacks |
| `compliance`                  | compliance                     | Allowlist, jurisdictions, hooks, expiry, layered gates |
| `registry`                    | registry                       | Dense ids, issuer/type partitioning, active TVL    |
| `dividend`                    | dividend + asset-token         | Escrow conservation, per-holder claim               |
| `tax_withholding`             | dividend/tax_withholding       | Rate boundaries, overflow, exemptions, custody      |
| `cap_table`                   | cap-table                      | Real Merkle roots and proofs                        |
| `debt_token`                  | debt-token                     | Senior/mezzanine/equity waterfall conservation      |
| `bridge`                      | bridge                         | Real Ed25519 multisig, replay, TTL, nonce           |
| `vaults`                      | vaults                         | Entry-point totality (see below)                    |
| `cross_contract_invariants`   | all of the above, wired together | End-to-end supply, escrow, compliance gating      |

`cross_contract_invariants` is the one worth understanding first. Its headline
property is that the sum of a token's holder balances plus everything clawed
back equals its total supply, checked across a fully deployed system with two
tokens, a registry, compliance, a dividend contract, a cap table and a debt
ledger. A divergence anywhere in the graph breaks it, and no single-contract
target can see it.

## Known scope limits

Stated here rather than buried, because a fuzz target that quietly cannot reach
the interesting state is worse than no target at all.

**`vaults` is a stub.** `claim_yield` and `reinvest_yield` are byte-for-byte
identical: both read the accrued reward into a discarded `_reward` binding and
zero it, so neither pays out nor compounds anything. `Token`, `TotalShares`,
`RewardPerShareStored`, `LastUpdateTime` and `UserRewardPerTokenPaid` are
declared and never touched, and there is no `initialize` and no admin, so
nothing can write a non-zero `Rewards` balance. The two entry points are
therefore only reachable in their zero state.

The target asserts what *is* well-defined today — that neither entry point can
trap for any holder, that both are total over storage, that they are idempotent
and holder-independent — but it cannot manufacture the non-zero state that would
make them interesting. Making them meaningful means designing a reward-accrual
model, a share price, and a token to pay out in. That is a design decision, not
a bug fix, so it is left for a human rather than invented here.

**`bridge`'s token lock/mint is also a stub.** The signature path is fully
implemented and genuinely tested with real Ed25519 keys, but
`lock_and_mint_request` only records the request and advances the nonce; it does
not move tokens.

## CI

`.github/workflows/fuzz.yml`:

- **nightly**, 03:17 UTC — 1800 s per target, 11 targets, `fail-fast: false` so
  one finding does not hide the other ten. Corpus cached per target between runs.
- **on pull request** — property tests as a hard gate, then 60 s per target as a
  build-and-smoke check. Fuzzing jobs are skipped on fork PRs.
- **`workflow_dispatch`** — `target` narrows the matrix to one target;
  `seconds` overrides the time box. Useful for reproducing a specific finding.

Corpus and `target/` are gitignored. `proptest-regressions/` is **not**, on
purpose: those files are the recorded counterexamples, and replaying them is how
a fixed bug is kept fixed.

## Adding a target

1. Write `fuzz_targets/<name>.rs` with `#![no_main]` and a `fuzz_target!` body.
2. Add the `[[bin]]` block to `Cargo.toml`. `test = false`/`doc = false`/
   `bench = false` because libFuzzer drives the binary directly.
3. Add the name to `ALL_TARGETS` in `.github/workflows/fuzz.yml`, or the target
   will build locally and never run in CI.
4. Prefer real cryptography and real structures over mocks. `bridge` signs with
   real Ed25519 keys and `cap_table` builds real Merkle trees, because a target
   that only feeds garbage can prove a function rejects garbage — it cannot
   prove the function accepts the right thing.
