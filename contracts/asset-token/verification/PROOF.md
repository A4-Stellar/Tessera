# Supply conservation and mint/burn authorization (#207)

## Statement and scope

For every reachable **committed transaction state** initialized by this version,
let `b(a)` be an account's effective balance (including lazy splits) and let `S`
be `total_supply`. For the finite set of accounts `H`:

```text
I:  0 <= S <= 2^127 - 1
    for every a in H: b(a) >= 0
    S = sum(a in H, b(a))
```

An unsuccessful invocation leaves `S` and every balance unchanged. A successful
mint requires the stored administrator's authorization; its supply increase is
exactly the authorized positive amount. A burn requires the holder's
authorization and sufficient balance, decreasing balance and supply equally.
No unsigned or non-administrator mint increases supply.

The K definition is a transaction-level accounting semantics, **not a Rust or
Soroban WASM semantics**. The executable symbolic claims prove the modeled core
initialization/mint/burn transitions. The correspondence to Rust is reviewed
below and exercised by actual Soroban host tests. This does not constitute a
machine-checked refinement proof of the compiled contract or its dependencies.
The manual inductive argument covers the additional accounting paths and both
share classes. Randomized host tests check those paths, but are not formal proofs.

## Mathematical proof

### Base case

An authorized initialization with `0 <= n <= MAX` starts with `S = n`,
`b(admin) = n`, and all other balances zero. Therefore `sum(b) = n = S`.
Reinitialization is rejected and preserves the previous state.

### Mint

Let recipient `r` have balance `x`, taking a missing account as zero. The
administrator must authenticate; the token must be active and unpaused; `r`
must pass compliance; `n > 0`; and checked addition must succeed.

```text
b'(r) = x + n          b'(a) = b(a), a != r
S' = S + n
sum(b') = sum(b) - x + (x + n) = S + n = S'
```

The amount and balances stay nonnegative. The checked supply addition bounds
`S'` by `MAX`, and consequently every balance by `S'`. Authentication, identity,
compliance, pause, liquidation, amount or overflow failures are atomic
identities; they cannot create tokens. The explicit recipient checked addition
also rejects inconsistent legacy states rather than wrapping.

### Burn (including settlement and buyback tenders)

For a holder balance `x`, authorization, `n > 0`, `x >= n` and `S >= n` yield:

```text
b'(r) = x - n          b'(a) = b(a), a != r
S' = S - n
sum(b') = sum(b) - x + (x - n) = S - n = S'
```

Ordinary burns enforce lockups and reject liquidation. A settlement burn
additionally authenticates the immutable configured vault; a buyback tender
also checks trading restrictions and atomically pays the holder. A failed
downstream payment rolls back both accounting writes. Finalizing a buyback
does not burn again.

### Transfer and clawback

For distinct accounts with balances `x` and `y`, debit `n` from one and credit
`n` to the other: `(x - n) + (y + n) = x + y`. Supply is unchanged. Self-transfer
and self-clawback are identities. Clawback now credits the administrator;
formerly it discarded the balance while retaining supply, contradicting `I`.

### Share splits

For a positive ratio `p:q`, each new effective balance is
`b'(a) = floor(b(a) * p / q)`. The implementation explicitly computes
`S' = sum(a in H, b'(a))` with checked arithmetic before committing the split.
Thus `I` holds by construction. Rounding the aggregate instead is unsound:

```text
b = [599, 300, 101], S = 1000, ratio = 1:5
sum(floor(b / 5)) = 119 + 60 + 20 = 199 != floor(S / 5) = 200
```

The holder index is populated on every balance write from initialization.
Lazy reads replay the same per-holder floors used in the supply scan; writing
an effective balance records the current epoch and does not reapply a split.
Contracts upgraded from versions without a complete index reject splits.

### Dual-class partition

Let `b_A(a), b_B(a)` denote class balances, and `S_A, S_B` their supplies:

```text
b(a) = b_A(a) + b_B(a)
S_A = sum(b_A), S_B = sum(b_B), S = S_A + S_B
```

Classes can only be enabled on zero supply. Class mint/burn changes both the
selected class and the aggregate by the same amount. Same-class transfers
apply the transfer identity above. An A-to-B transfer/conversion subtracts `n`
from A's balance and supply and adds `n` to B's balance and supply. Consequently
the partition and aggregate supply stay unchanged. Ordinary mutations reject
class mode, so no unclassified units can be introduced. Class A voting supply
is additionally bounded by `MAX / 10`; Class B has zero voting weight. Both
classes have unit dividend and liquidation weight.

### Liquidation and reserve safety

Activation preserves every balance and supply, snapshots `S0 = S > 0` and
`P > 0`, and disables all market/supply-changing paths except settlement
burns. Four distinct authenticated members of the fixed six must authorize
activation, and the funder must authorize the reserve transfer. The reserve,
committee, denominator and activation cannot be reset.

For cumulative redeemed shares `r_h` of holder `h`, its entitlement is
`E_h = floor(r_h * P / S0)`. Each claim pays the increase in `E_h`, so payments
telescope regardless of chunking. Each burn is holder-authorized and reduces
that holder's actual balance; additionally, `sum(r_h) <= S0` is checked.
Therefore:

```text
sum(E_h) <= floor(sum(r_h) * P / S0) <= P
```

`U256` contains the full product of two nonnegative signed-128-bit quantities
(at most 254 bits), avoiding intermediate overflow. Indivisible rounding
residuals remain in the vault. Failed funding, burn or payout invocations roll
back the whole transaction. These vault properties have host regression tests
and this mathematical argument; they are not claims about the K mint/burn model.

## Rust/model correspondence and assumptions

| K transition/guard | Rust implementation |
| --- | --- |
| Authorized initialization, nonnegative supply | `lib.rs::initialize` |
| Stored administrator identity and `AUTH` | `require_admin`, `Address::require_auth` |
| `COMPLIANT`, paused and liquidated guards | `compliance_allows`, `require_not_paused`, `require_active` |
| Positive bounded mint, equal map/supply increment | `lib.rs::mint`, `stock_split::set_balance` |
| Holder auth, lockup, sufficient balance and equal decrement | `lib.rs::burn` |
| Finite map sum | Effective balances over the complete holder index |
| Rejected transition preserves all accounting | Soroban transaction rollback |

The model uses arbitrary integer account identifiers and finite maps, not a
bounded enumeration of holders. Claims assume a well-formed map with distinct
keys and start from valid states. Missing balances are zero. The host's
authentication, atomic rollback, storage restoration and integer operations
are trusted. Compliance is an external predicate. No theorem here establishes
the deployed compliance/stablecoin contracts' behavior, private-key custody,
legal committee appointment, fair market valuation or settlement liveness.

This work does not repair already inconsistent historical states or provide
an upgrade migration for active old buybacks. The invariant assumes valid
initialization (or a separately reviewed migration). Atomic transaction
boundaries are the observation points, rather than individual internal writes.

## Reproducing verification

Use K **7.1.337**, its Haskell backend and Z3 **4.8.15**. From the repository root:

```sh
contracts/asset-token/verification/verify.sh
cargo +stable test --manifest-path contracts/Cargo.toml --locked \
  -p tessera-asset-token -p tessera-escrow -p tessera-emergency-liquidation
```

The runner compiles `mint_burn.k` and proves the labeled reachability claims in
`mint_burn-spec.k`. Exit status must be zero and K's final result must be
`#Top`; compilation alone is not a successful proof. No unproved arithmetic
lemmas or externally assumed conservation axioms are supplied. The finite-map
sum is recursively defined in the semantics.

CI in `.github/workflows/token-security.yml` checks the pinned K package's
official SHA-256, runs the proof runner, runs host/property/authorization tests
and builds the three WASM artifacts.

## Recorded results (2026-10-19)

K 7.1.337 with the Haskell backend and Z3 4.8.15 compiled the definition and
proved **all 15 claims**, exiting **0** with output **`#Top`**:

| Claim | Result |
| --- | --- |
| `initialize-conservation` | Proved |
| `mint-existing-conservation` | Proved |
| `mint-new-conservation` | Proved |
| `burn-conservation` | Proved |
| `unauthorized-mint-is-identity` | Proved |
| `mint-overflow-is-identity` | Proved |
| `nonpositive-mint-is-identity` | Proved |
| `unauthorized-burn-is-identity` | Proved |
| `insufficient-balance-burn-is-identity` | Proved |
| `missing-account-burn-is-identity` | Proved |
| `paused-mint-is-identity` | Proved |
| `liquidated-mint-is-identity` | Proved |
| `locked-burn-is-identity` | Proved |
| `nonpositive-burn-is-identity` | Proved |
| `reinitialization-is-identity` | Proved |

**Negative control:** in a temporary copy only, change the existing-account
mint rule's recipient credit from `B +Int N` to `B +Int N +Int 1`, leaving
supply and all claims unchanged. Running the same verifier failed with a
nonzero exit and `WarnStuckClaimState` at `mint-existing-conservation`, exposing
the extra unit. The committed definition is the passing, unmodified model.
The `smtlib` declaration on `sumBalances` lets SMT treat an unchanged frame's
sum as an uninterpreted integer; the recursive K rule unfolds the touched map
entry. It does not assume conservation.

Rust 1.93.1 host checks: **44 tests passed** (22 asset-token, including two
64-case stateful properties; 16 emergency-vault; six existing escrow).
The new packages pass Clippy with warnings denied, allowing only the existing
SDK event deprecations and generated initialization argument-builder lint.
All three packages build to `wasm32v1-none` in the checked release profile.
Formatting, shell syntax and diff whitespace checks pass.

The separate pre-existing aggregate property suite is blocked by an unresolved
`tax_withholding` import in `contracts/dividend/src/jurisdictional_tax.rs`.
That is not a passing test result; the focused host properties above run
independently of it.

References: [K deductive verification tutorial](https://kframework.org/k-distribution/k-tutorial/1_basic/22_proofs/),
[K map builtins](https://kframework.org/k-distribution/include/kframework/builtin/domains/),
[K 7.1.337 release](https://github.com/runtimeverification/k/releases/tag/v7.1.337).
