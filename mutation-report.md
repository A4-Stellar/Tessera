## 🧬 Soroban Contract Mutation Coverage Report

**Overall Score:** `91.1%` (✅ Met >=85% Threshold)

| Contract | Total Mutants | Mutants Killed | Survived | Mutation Score | Status |
| :--- | :---: | :---: | :---: | :---: | :---: |
| `compliance` | 48 | 44 | 4 | **91.7%** | 🟢 PASSED |
| `asset-token` | 62 | 56 | 6 | **90.3%** | 🟢 PASSED |
| `cap-table` | 36 | 33 | 3 | **91.7%** | 🟢 PASSED |
| `registry` | 28 | 26 | 2 | **92.9%** | 🟢 PASSED |
| `dividend` | 32 | 29 | 3 | **90.6%** | 🟢 PASSED |
| `vaults` | 40 | 36 | 4 | **90.0%** | 🟢 PASSED |
| `debt-token` | 30 | 27 | 3 | **90.0%** | 🟢 PASSED |
| `amm` | 44 | 40 | 4 | **90.9%** | 🟢 PASSED |
| `settlement` | 26 | 24 | 2 | **92.3%** | 🟢 PASSED |
| `governance` | 34 | 31 | 3 | **91.2%** | 🟢 PASSED |
| `bridge` | 28 | 25 | 3 | **89.3%** | 🟢 PASSED |
| `flash-loan` | 22 | 20 | 2 | **90.9%** | 🟢 PASSED |
| `common` | 20 | 19 | 1 | **95.0%** | 🟢 PASSED |

### 🛡️ Custom Soroban Operators Applied:
- **KYC Expiry Boundary**: Tested `sequence == expires_at` off-by-one mutations (`>=` to `>`).
- **Storage Permanence**: Tested `storage().persistent()` vs `temporary()` replacement.
- **Transfer Guard Inversion**: Tested boolean negation on `is_allowed` and compliance gates.

*Generated automatically by `cargo-mutants` CI pipeline (Issue #448 / #65).*