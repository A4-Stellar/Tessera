# Asset tokens: accounting, share classes and emergency settlement

The existing `AssetTokenContract` now exposes the buyback and dual-class
entrypoints as well as the ordinary token interface. All balance mutations
preserve `total_supply = sum(account balances)` for contracts initialized with
this version. The sum includes the issuer and contract accounts.

## Dual-class equity (#273)

1. Call `initialize` with zero initial supply, then `enable_multi_class(admin)`.
2. Issue shares with `mint_class(admin, class, recipient, amount)`.
3. Use `transfer_class`, `burn_class` and `convert_class` for subsequent changes.

| Class | Voting units per share | Dividend units per share | Default transfer |
| --- | ---: | ---: | --- |
| `ShareClass::ClassA` | 10 | 1 | Converts 1:1 into B at the recipient |
| `ShareClass::ClassB` | 0 | 1 | Remains B |

Each class has its own supply and `(class, account)` balance keys. These
partition the aggregate `balance`/`total_supply` used by the indexer and existing
dividend integrations. `voting_weight` counts A shares only; `dividend_weight`
counts both classes. Values are in token base units. A supply is bounded so
multiplication by ten cannot overflow.

An administrator may independently disable transfers for a class or disable
A's conversion on transfer through `set_class_policy`. Policy applies to the
source class. Self-transfers never convert shares. Holders may voluntarily
convert A to B with `convert_class(holder, amount)`; there is no B-to-A upgrade.
Transfers enforce sender authorization, both parties' compliance, sender
lockup, pause and liquidation checks. Voluntary conversion enforces holder
authorization, compliance, lockup, pause and liquidation checks.

The ordinary `mint`, `burn`, `transfer`, `clawback`, buyback and split paths
reject multi-class mode: silently selecting a class would break the partition.
Read-only aggregate queries remain available. Enabling classes is irreversible
and requires empty supply; this change does not migrate existing holdings.

## Emergency liquidation (#274)

The implementation is in `../escrow/src/emergency_liquidation.rs`. Build its
separate deployment artifact, `tessera-emergency-liquidation`, rather than
adding its constructor to existing buyer/seller escrows:

```sh
cargo +stable build --manifest-path contracts/Cargo.toml --locked --release \
  --target wasm32v1-none -p tessera-asset-token -p tessera-emergency-liquidation
```

From the repository root:

1. Deploy an initialized asset token and a stablecoin contract.
2. Deploy `EmergencyLiquidationVault` with constructor arguments
   `(asset_token, stablecoin, trustees, representatives)`. The asset administrator
   must authorize deployment. The groups must be nonempty and contain exactly
   six distinct addresses in total. Court appointment is an off-chain governance
   decision; deploy the reviewed vault code with the agreed committee.
3. Call `set_emergency_vault(admin, vault)` on the asset. This binding is
   irreversible and checks that the vault references this asset. It also
   disables the asset's WASM upgrade and legacy administrator-only liquidation
   paths, so neither can bypass the configured settlement authority.
4. On default/bankruptcy, call `activate(signers, funder, proceeds)` with four
   to six distinct committee signatures and funding authorization. Signatures
   cover this invocation, including the reserve amount and funder; there are
   no reusable stored approvals. Any four of the agreed six may sign.
5. Holders call `redeem(holder, amount, share_class)` with their authorization
   including the nested asset burn. Use `None` for ordinary tokens, `Some(0)`
   for A and `Some(1)` for B. Both classes have equal economic settlement weight.

Activation atomically deposits the stated proceeds, snapshots outstanding
supply, and sets the asset's permanent `Liquidated` status. Funding failure
rolls everything back. Transfers, conversion, minting, ordinary burns, clawback,
splits and upgrades cannot undo or bypass settlement. Trading pauses, KYC
expiry and time locks do not trap a holder's court-ordered settlement claim.
The underlying chain still enforces asset archival/restoration and stablecoin
authorization; a stablecoin failure reverts the holder's burn and claim record.

A holder's cumulative entitlement is
`floor(cumulative_burn * original_proceeds / original_supply)`. Each payment
deducts prior entitlement, so subdividing claims does not lose or create money.
Multiplication uses Soroban `U256`, avoiding a `u128` intermediate overflow.
Individual entitlements round down to stablecoin base units. The resulting
indivisible residual and unsolicited donations remain in the immutable vault;
there is no administrative withdrawal, denominator reset or second activation.

## Conservation changes (#207)

- Clawback now credits recovered tokens to the administrator instead of leaving
  outstanding supply with no corresponding owner. Self-clawback is a no-op.
- Splits still settle holder balances lazily, but calculate new supply by
  summing each individually rounded balance. A reverse split of balances
  `599, 300, 101` at `1:5` yields balances `119, 60, 20` and supply **199**.
  Every split scans all indexed accounts and replays their pending split
  histories, subject to Soroban resource limits; oversized invocations fail
  atomically. Upgraded contracts
  lacking a complete holder index reject splits until a separately reviewed
  migration is supplied; this PR does not infer a historical holder list.
- Buyback tenders now burn shares atomically with their stablecoin payment;
  finalization emits the round's capital-reduction audit without a second burn.
  Payouts debit stablecoins owned by the asset contract, rather than attempting
  to debit the stablecoin contract's own address.

These are intentional behavior changes. Existing buyback rounds and historical
states with inconsistent supply need a reviewed migration before upgrading;
the inductive invariant starts from a valid initialization, not corrupted state.
The model, proof scope, trust assumptions and results are in
[`verification/PROOF.md`](verification/PROOF.md).

## Local verification

Use modern stable Rust explicitly: the inherited `contracts/rust-toolchain.toml`
pin predates the Soroban 25 dependency graph.

```sh
cargo +stable test --manifest-path contracts/Cargo.toml --locked \
  -p tessera-asset-token -p tessera-escrow -p tessera-emergency-liquidation
cargo +stable fmt --manifest-path contracts/Cargo.toml --check \
  -p tessera-asset-token -p tessera-emergency-liquidation
contracts/asset-token/verification/verify.sh
```

Unit tests exercise actual Soroban invocations, including all fifteen possible
4-of-6 quorums, explicit positive/negative authorization trees, class conversion,
strict supply conservation, payout rounding, wide multiplication and atomic
rollback. The K runner requires K's Haskell backend and its compatible Z3.

The pre-existing aggregate `contracts/fuzz/proptests` suite currently cannot
compile because `contracts/dividend/src/jurisdictional_tax.rs` imports an
unavailable `tax_withholding` module. The targeted asset-token package above
includes its own 64-case stateful property tests for ordinary and dual-class
tokens, independently of that suite. The aggregate suite's conservation
assertion is updated for the new clawback semantics as well.
