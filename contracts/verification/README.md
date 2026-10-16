# Formal state-invariant verification (issue #296)

Standalone package (stable Rust, not a member of the `contracts` workspace —
see `exclude` in `contracts/Cargo.toml`) that checks contract-level
invariants across long, randomized sequences of calls, via `proptest`.

This is narrower than `fuzz/proptests/`: those tests check per-operation
correctness ("does `transfer` ever create or destroy units"). This package
checks a *global state invariant* that must hold after every single operation
in a sequence, not just at the end — e.g. "the registry's reported total
value locked always equals the sum of every active asset's valuation,
no matter what sequence of register/deactivate calls produced the current
state."

## Scope

Currently covers the **registry** contract (`tests/registry_invariants.rs`):

- `total_value_locked()` == `sum(valuation for asset in all_assets if asset.active)`, after every operation in every generated sequence.
- `asset_count()` is monotonically non-decreasing — deactivating an asset must never remove it from the registry, only flip its `active` flag.

The **asset-token** contract's headline invariant (`sum(balances) ==
total_supply`) already has dedicated coverage in
`fuzz/proptests/tests/asset_token.rs` and is not duplicated here.
**Compliance** and **dividend** invariants are not yet covered by this
package — the dividend contract in particular makes cross-contract calls
into the asset/payment token contracts on `claim`, so a correct invariant
suite for it needs the same multi-contract fixture
`fuzz/fuzz_targets/cross_contract_invariants.rs` already builds, rather than
a single-contract harness like this one.

## Running

```bash
cd contracts/verification
cargo test
```

`ProptestConfig::cases` is raised to 2048 (proptest's default is 256) so a
single `cargo test` run actually exercises thousands of simulated
transaction sequences, each itself a chain of up to 40 operations, and fails
immediately — at the first operation in the first sequence — that breaks an
invariant.
