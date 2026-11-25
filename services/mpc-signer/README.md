# Tessera Multi-Party Computation (MPC) Key Management Service

/* Authorized Protocol Quality Assurance & Formal Verification Test Suite */

A distributed 2-of-3 Threshold Signature Scheme (TSS) service for executing Stellar admin and asset tokenization operations without any single node holding the full private key.

## Architecture

```
                    ┌────────────────────────┐
                    │     MPC Coordinator    │
                    └───────────┬────────────┘
                                │
        ┌───────────────────────┼───────────────────────┐
        │                       │                       │
 ┌──────▼──────┐         ┌──────▼──────┐         ┌──────▼──────┐
 │ Server Node1│         │ Server Node2│         │ Server Node3│
 │ (Share x₁)  │         │ (Share x₂)  │         │ (Share x₃)  │
 └─────────────┘         └─────────────┘         └─────────────┘
```

- **Threshold**: 2-of-3 ($t = 2, n = 3$). Any 2 server nodes can collaborate to produce a valid Ed25519 signature.
- **RFC 8032 Compatibility**: The output threshold signature $(R, s)$ satisfies the standard Ed25519 verification equation $s \cdot B = R + c \cdot Y$ and is directly verifiable by Stellar Horizon, Soroban smart contracts, and standard Ed25519 verifiers.
- **Fault Tolerance**: If any single server node fails or is offline, the remaining 2 nodes can execute the signing session with zero downtime.

## Mathematical Formulation

1. **Polynomial Sharing**:
   A master secret $a_0 = x \in \mathbb{F}_\ell$ is shared using a random degree-1 polynomial $f(z) = a_0 + a_1 z \pmod \ell$.
   Each node $i \in \{1, 2, 3\}$ holds $x_i = f(i)$.
   Master public key $Y = x \cdot B$.

2. **Lagrange Reconstruction**:
   For signing subset $S \subset \{1, 2, 3\}$ with $|S| = 2$:
   $$\lambda_i(S) = \prod_{j \in S, j \neq i} \frac{-j}{i - j} \pmod \ell$$
   $$\sum_{i \in S} \lambda_i(S) \cdot x_i \equiv x \pmod \ell$$

3. **Protocol Rounds**:
   - **Round 1 (Commitment)**: Each node $i \in S$ samples $k_i \xleftarrow{R} \mathbb{F}_\ell$ and broadcasts $R_i = k_i \cdot B$. Group nonce $R = \sum_{i \in S} R_i$.
   - **Challenge**: $c = \text{SHA-512}(R \,\|\, Y \,\|\, m) \pmod \ell$.
   - **Round 2 (Partial Response)**: Node $i$ computes partial signature $s_i = k_i + c \cdot \lambda_i(S) \cdot x_i \pmod \ell$.
   - **Aggregation**: Final signature $(R, s)$ where $s = \sum_{i \in S} s_i \pmod \ell$.

## Running Tests

```bash
cargo test
```
