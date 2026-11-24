//! # Storage Schema Registry (Issue #449 / #66)
//!
//! Enforces storage schema versioning and backwards compatibility across
//! Soroban contract upgrades in the Tessera RWA platform.
//!
//! ## Invariants
//! - **Additive Only**: Contract upgrades may add new storage keys or fields, but
//!   must never delete or repurpose existing storage key discriminants.
//! - **Version Monotonicity**: Schema versions must increment monotonically ($N+1 > N$).
//! - **Backward Compatibility**: All records written under schema version $N$ must
//!   remain valid and readable by schema version $N+1$.

#![no_std]

use soroban_sdk::{
    contracterror, contracttype, symbol_short, Address, BytesN, Env, IntoVal, String, Symbol, Val,
    Vec,
};

/// Error codes for storage schema registration and validation.
#[contracterror(export = false)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum SchemaCompatibilityError {
    /// Target schema version is not strictly greater than current version.
    VersionRegression = 6601,
    /// Breaking change detected: existing storage key was deleted or altered.
    BreakingKeyChange = 6602,
    /// Schema record for this contract is missing.
    SchemaNotFound = 6603,
    /// Maximum allowed schema version exceeded.
    VersionOverflow = 6604,
    /// Contract is not authorized to register schemas.
    Unauthorized = 6605,
}

/// Metadata record for a contract's storage schema.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageSchemaRecord {
    /// Symbol identifier for the contract (e.g. `symbol_short!("asset")`).
    pub contract_name: Symbol,
    /// Current storage schema version (starts at 1).
    pub version: u32,
    /// Bitmask or checksum of registered storage keys in this version.
    pub key_fingerprint: u64,
    /// Timestamp or ledger when this schema was registered.
    pub registered_at: u64,
}

/// Key used to store schema records in Soroban instance storage.
const SCHEMA_KEY_PREFIX: Symbol = symbol_short!("schema");

/// Manages and validates storage schemas in contracts.
pub struct StorageSchemaRegistry;

impl StorageSchemaRegistry {
    /// Formats the storage key for a contract's schema record.
    pub fn schema_key(contract_name: &Symbol) -> (Symbol, Symbol) {
        (SCHEMA_KEY_PREFIX, contract_name.clone())
    }

    /// Read the registered schema version for a contract, or default to 1 if unset.
    pub fn get_schema_version(env: &Env, contract_name: &Symbol) -> u32 {
        let key = Self::schema_key(contract_name);
        env.storage()
            .instance()
            .get::<_, StorageSchemaRecord>(&key)
            .map(|r| r.version)
            .unwrap_or(1)
    }

    /// Register or upgrade the storage schema for `contract_name`.
    ///
    /// Validates version monotonicity and checks that the new `key_fingerprint`
    /// preserves all bits from the previous version (additive compatibility).
    pub fn register_or_upgrade(
        env: &Env,
        contract_name: Symbol,
        new_version: u32,
        new_key_fingerprint: u64,
    ) -> Result<StorageSchemaRecord, SchemaCompatibilityError> {
        let key = Self::schema_key(&contract_name);
        let current_record = env.storage().instance().get::<_, StorageSchemaRecord>(&key);

        if let Some(ref current) = current_record {
            // 1. Version must be strictly greater
            if new_version <= current.version {
                return Err(SchemaCompatibilityError::VersionRegression);
            }

            // 2. Compatibility check: all previous keys must remain present
            // In bitmask representation, (current & new) == current ensures no removed keys
            if (current.key_fingerprint & new_key_fingerprint) != current.key_fingerprint {
                return Err(SchemaCompatibilityError::BreakingKeyChange);
            }
        }

        let record = StorageSchemaRecord {
            contract_name,
            version: new_version,
            key_fingerprint: new_key_fingerprint,
            registered_at: env.ledger().timestamp(),
        };

        env.storage().instance().set(&key, &record);
        Ok(record)
    }

    /// Pure validation helper for schema upgrade compatibility without touching storage.
    pub fn validate_compatibility(
        current_version: u32,
        current_fingerprint: u64,
        next_version: u32,
        next_fingerprint: u64,
    ) -> Result<(), SchemaCompatibilityError> {
        if next_version <= current_version {
            return Err(SchemaCompatibilityError::VersionRegression);
        }
        if (current_fingerprint & next_fingerprint) != current_fingerprint {
            return Err(SchemaCompatibilityError::BreakingKeyChange);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic_and_additive_schema_evolution() {
        let env = Env::default();
        let contract = symbol_short!("asset");

        // Initial schema v1: keys 0b0001 | 0b0010 = 0b0011
        let v1 = StorageSchemaRegistry::register_or_upgrade(&env, contract.clone(), 1, 0b0011).unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(StorageSchemaRegistry::get_schema_version(&env, &contract), 1);

        // Additive upgrade v2: keys 0b0011 | 0b0100 = 0b0111 (succeeds)
        let v2 = StorageSchemaRegistry::register_or_upgrade(&env, contract.clone(), 2, 0b0111).unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(StorageSchemaRegistry::get_schema_version(&env, &contract), 2);

        // Breaking change: removing key 0b0001 -> new fingerprint 0b0110 (fails)
        let err_breaking = StorageSchemaRegistry::register_or_upgrade(&env, contract.clone(), 3, 0b0110);
        assert_eq!(err_breaking, Err(SchemaCompatibilityError::BreakingKeyChange));

        // Version regression: v2 again (fails)
        let err_regression = StorageSchemaRegistry::register_or_upgrade(&env, contract.clone(), 2, 0b1111);
        assert_eq!(err_regression, Err(SchemaCompatibilityError::VersionRegression));
    }
}
