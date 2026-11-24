# Tessera Multi-Party Computation (MPC) Key Management Service

A distributed Threshold Signature Scheme (TSS) service based on the **FROST** protocol (Flexible Round-Optimised Schnorr Threshold signatures) for executing Stellar admin and asset tokenization operations without any single node holding the full private key (Issue #46 / #427).

## Architecture

```
                    ┌────────────────────────┐
                    │     MPC Coordinator    │
                    └───────────┬────────────┘
                                │
        ┌───────────┬───────────┼───────────┬───────────┐
        │           │           │           │           │
 ┌──────▼──────┐ ┌──▼────────┐ ┌▼─────────┐ ┌▼─────────┐ ┌▼─────────┐
 │ Server Node1│ │Server Node│ │Server Nod│ │Server Nod│ │Server Nod│
 │ (Share x₁)  │ │ (Share x₂)│ │(Share x₃)│ │(Share x₄)│ │(Share x₅)│
 └─────────────┘ └───────────┘ └──────────┘ └──────────┘ └──────────┘
```

- **Configurable Threshold**: Default **3-of-5** ($t = 3, n = 5$) with no single-point-of-failure. Any 3 nodes can collaborate to sign.
- **Pedersen VSS DKG**: Key shares are generated using Pedersen Verifiable Secret Sharing (VSS) where each node verifies dealer polynomial evaluations against coefficient commitments.
- **FROST Protocol**:
  - **Round 1 (Nonces)**: Each signer generates hiding nonce $d_i$ and binding nonce $e_i$ and broadcasts commitments $(D_i = d_i \cdot B, E_i = e_i \cdot B)$.
  - **Deterministic Re-transmission**: Network layer detects message loss and re-transmits Round 1 packets up to configurable retry limits.
  - **Binding Factors**: Nonce binding factor $\rho_i = H(\text{"FROST-ED25519-RHO"} \,\|\, i \,\|\, m \,\|\, B)$. Group commitment $R = \sum_{j \in S} (D_j + \rho_j E_j)$.
  - **Round 2 (Share Generation)**: Signer $i$ computes partial signature share $z_i = d_i + (\rho_i \cdot e_i) + (\lambda_i \cdot c \cdot x_i) \pmod \ell$.
  - **Byzantine Detection**: Coordinator individually verifies $z_i \cdot B \stackrel{?}{=} (D_i + \rho_i E_i) + (\lambda_i \cdot c) \cdot Y_i$ to isolate corrupt/Byzantine nodes before aggregation.
  - **Aggregation**: Final Schnorr signature is $(R, s)$ where $s = \sum z_i \pmod \ell$.
- **RFC 8032 & Stellar Compatibility**: The aggregated signature verifies with standard Ed25519 verifiers (`ed25519_dalek::VerifyingKey::verify`), Soroban smart contracts, and Stellar transaction envelopes.

## Running Tests

```bash
cargo test
```
