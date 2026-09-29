# Integration Guide for Tessera Fixes

## Issue #147: Debt Refinancing Integration

### Add to `contracts/debt-token/src/lib.rs`:
```rust
pub mod refinancing;
pub use refinancing::RefinancingEngine;
```

### Usage Example:
```rust
// Initiate refinancing from 8% debt to 5% debt
let proposal_id = RefinancingEngine::initiate_refinancing(
    env,
    old_8_percent_token,
    new_5_percent_token,
    100, // 1:1 conversion
    expiration_ledger,
)?;

// Holder converts their tokens
let new_shares = RefinancingEngine::execute_conversion(
    env,
    proposal_id,
    holder_address,
    1000, // old shares to convert
)?;
```

## Issue #148: Loss Waterfall Integration

### Add to `contracts/vaults/src/lib.rs`:
```rust
pub mod loss_waterfall;
pub use loss_waterfall::LossWaterfallEngine;
```

### Usage Example:
```rust
// Initialize tranches
LossWaterfallEngine::initialize_tranches(
    env,
    vault_id,
    equity_token,
    mezzanine_token,
    senior_token,
    10_000_000, // Initial par value (scaled by 1e7)
)?;

// Allocate capital loss
let log = LossWaterfallEngine::allocate_loss(
    env,
    vault_id,
    5_000_000, // Loss amount in stroops
)?;

// Audit log contains:
// - Total loss allocated
// - Loss per tranche (equity, mezzanine, senior)
// - New par values per tranche
```

## Issue #161: Schema Guard Integration

### Add to `api/src/db/mod.rs`:
```rust
pub mod schema_guard;
```

### Add to `api/src/main.rs`:
```rust
use crate::db::schema_guard::{verify_schema_compatibility, SchemaGuardConfig, abort_on_schema_error};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // ... database pool setup ...
    
    // Verify schema before accepting requests
    let config = SchemaGuardConfig::from_env();
    let validation_result = verify_schema_compatibility(&pool, config).await?;
    
    match validation_result {
        SchemaValidationResult::Valid | SchemaValidationResult::ValidationSkipped => {
            // Continue startup
        }
        other => {
            abort_on_schema_error(other);
        }
    }
    
    // ... rest of server startup ...
}
```

### Environment Variables:
```bash
# Skip schema check (development only)
SKIP_SCHEMA_CHECK=true

# Expected schema version (optional)
EXPECTED_SCHEMA_VERSION=12
```

### Command Line Flag:
```bash
# Run with schema check override
cargo run -- --skip-schema-check
```

## Testing

### Refinancing Tests:
```bash
cd contracts/debt-token
cargo test refinancing
```

### Loss Waterfall Tests:
```bash
cd contracts/vaults
cargo test loss_waterfall
```

### Schema Guard Tests:
```bash
cd api
cargo test schema_guard
```

## Next Steps

1. Integrate modules into respective `lib.rs` files
2. Add contract deployment scripts for refinancing and waterfall engines
3. Configure schema guard in production deployment
4. Add monitoring for refinancing events and loss allocations
5. Document API endpoints for querying refinancing proposals and loss history
