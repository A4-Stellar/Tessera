# tessera-zk-aggregator

Off-chain KYC proof aggregation for the compliance contract (issue #317).
It batches up to 32 investor credentials into **one** Groth16 proof over
BN254, which the compliance contract verifies in a single transaction
(`submit_aggregated_kyc`).

This is a standalone package, like `contracts/fuzz` and
`contracts/verification`. Its arkworks prover stack is native-only and is
kept out of the wasm contracts workspace.

## What one proof says

> For every slot `i < count`: the prover knows an expiry `e_i` and a Merkle
> path showing `Poseidon(s_i, e_i)` is a leaf of the issuer's credential tree
> with root `R`, and `e_i > valid_until`.

The circuit's only public input is the commitment
`C = SHA-256(R ‖ valid_until ‖ count ‖ s_0 ‖ … ‖ s_31)` with byte 0 cleared.
The contract recomputes `C` from its own arguments: the subject addresses,
the issuer's stored root, and `valid_until`. Before verifying, it checks that
`valid_until` is after the current ledger and that every subject is distinct.
`s_i` is SHA-256 of the address's `ScVal` XDR with byte 0 cleared.

| Piece | Choice |
|-------|--------|
| Proof system | Groth16 (arkworks 0.4) |
| Curve | BN254, verified with Soroban's native BN254 host functions |
| Credential tree | depth-20 Poseidon Merkle tree (t = 3, x⁵, 8 full + 57 partial rounds) |
| Batch | up to 32 subjects (fixed slots, zero-padded) |
| On-chain cost (32 subjects) | 32.95M CPU instructions (33 % of the 100M per-transaction limit) and 0.98 MB memory (2.3 % of 40 MiB), measured by the compliance contract's `verification_stays_under_40_percent_of_the_transaction_budget` test |

### Scope compared with the issue

The issue asks for recursive Halo2/Plonk aggregation, meaning a circuit
that verifies 32 inner SNARKs. This crate doesn't do that. Soroban has native
BN254 and BLS12-381 pairings but nothing for Halo2's IPA commitments or the
Pasta curves, and a recursive verifier circuit is a much larger, separately
audited project.

Instead, the aggregation circuit checks the 32 underlying credentials
directly: Merkle membership in the issuer's tree, plus expiry. It gives the
same on-chain result: N KYC claims, one constant-size proof, one
transaction, flat verification cost. Only BN254 Groth16 is cheap enough on
Soroban today, so a future recursive pipeline would still finish with a
BN254 Groth16 "wrap" proof verified by this same contract method.

## Workflow

```bash
cargo build --release
B=target/release/tessera-zk-aggregator

# 1. Keys (once per circuit shape). Writes proving_key.bin, verifying_key.json, circuit.json.
$B setup --out keys            # defaults: --slots 32 --depth 20

# 2. Issuer: credentials.csv has `G…address,expiry_ledger` lines. Prints the tree root.
ROOT=$($B issue --input credentials.csv --queue queue)

# 3. Aggregate up to 32 queued credentials into one proof.
$B aggregate --keys keys --queue queue --root "$ROOT" --out bundle.json

# 4. Optional: re-check the bundle off-chain exactly as the contract will.
$B verify --keys keys --bundle bundle.json
```

On-chain, once per deployment and issuer:

- the admin calls `set_aggregation_vk(admin, verifying_key.json)`;
- the admin calls `set_zk_issuer_root(admin, issuer, ROOT)`.

Then anyone may call `submit_aggregated_kyc(issuer, bundle.subjects,
bundle.valid_until, bundle.proof)`. Each subject gets a `ZkAccreditation`
until `valid_until`. To revoke credentials, the issuer rebuilds its tree
without them and calls `update_zk_issuer_root`; proofs against the old root
then stop verifying.

### Queue handling

`aggregate` reads `*.json` credentials from the queue in file-name order.
It checks each one on its own (path leads to `--root`, not expired, not a
duplicate) and moves failures to `queue/rejected/`. The first 32 that pass
are proven, then moved to `queue/done/`. `valid_until` defaults to the
earliest expiry in the batch minus one.

Measured on a laptop at the default shape:

| Step | Time |
|------|------|
| `setup` | 2 min 7 s (360 MB proving key) |
| `aggregate` (32 subjects) | 61 s |
| `verify` | under 0.1 s |

## Security notes (read before production use)

- **Trusted setup.** `setup` is a single-party Groth16 setup: whoever runs it
  could forge proofs. Production keys must come from a multi-party ceremony
  for this exact circuit, for example a Powers of Tau phase 1 plus a
  circuit-specific phase 2.
- **`proving_key.bin`** is written uncompressed and loaded without point
  validation, so `aggregate` starts in seconds. Treat it as a trusted local
  artifact. Verifying keys and proofs read from JSON are always validated.
- **Privacy.** The proof hides each investor's expiry and tree position, but
  the aggregator sees the credentials it batches. Run it inside the issuer's
  or operator's trust boundary.
- **Audit.** The circuit hasn't been audited.

## Tests

```bash
cargo test   # circuit soundness checks, Groth16 round trip, Soroban encodings
```

The on-chain counterpart is `contracts/compliance/src/zk_aggregator_test.rs`.
It verifies a real 32-subject proof generated by this tool
(`contracts/compliance/src/test_fixtures/zk_aggregate_32.json`) and measures
its cost against Soroban's limits.
