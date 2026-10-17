//! Constant-size identity revocation accumulator (Issue #301).
//!
//! Revoking a compromised KYC attestation must not grow on-chain state. The
//! contract keeps one fixed-size accumulator instead of a list:
//!
//! ```text
//! root_n = SHA256("TESSERA_REVOCATION_ACC_V1" || root_{n-1} || id_n || epoch_n)
//! ```
//!
//! * **Update**: O(1) hashes and one storage write, regardless of how many
//!   identities were ever revoked.
//! * **Storage**: one `RevocationAccumulator` record (32-byte root plus a `u32`
//!   epoch), well under 100 bytes of ledger entry data.
//! * **Ordering**: the root commits to the full revocation history in order, so
//!   any reordering or omission changes it and is caught by off-chain replay.
//!
//! Exclusion proofs for an individual identity stay with the sparse Merkle
//! non-revocation proofs in `attestation.rs`; this accumulator is the value
//! those proofs are checked against and is what the transfer path reads.

use soroban_sdk::{contracttype, Bytes, BytesN, Env};

use crate::DataKey;

const ACCUMULATOR_DOMAIN: &[u8] = b"TESSERA_REVOCATION_ACC_V1";

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevocationAccumulator {
    /// Commitment to every identity revoked so far, in revocation order.
    pub root: BytesN<32>,
    /// Number of revocations applied. Strictly increases on every update.
    pub epoch: u32,
}

/// Returns the current accumulator. An empty accumulator is all-zero with
/// epoch 0, so the first revocation's preimage is fully determined.
pub(crate) fn current(env: &Env) -> RevocationAccumulator {
    env.storage()
        .instance()
        .get(&DataKey::RevocationAccumulator)
        .unwrap_or(RevocationAccumulator {
            root: BytesN::from_array(env, &[0u8; 32]),
            epoch: 0,
        })
}

/// Folds one revoked identity into the accumulator and persists the result.
pub(crate) fn revoke(env: &Env, id: &BytesN<32>) -> RevocationAccumulator {
    let prev = current(env);
    let next_epoch = prev
        .epoch
        .checked_add(1)
        .expect("revocation epoch overflow");

    let mut preimage = Bytes::from_slice(env, ACCUMULATOR_DOMAIN);
    preimage.extend_from_array(&prev.root.to_array());
    preimage.extend_from_array(&id.to_array());
    preimage.extend_from_array(&next_epoch.to_be_bytes());

    let next = RevocationAccumulator {
        root: env.crypto().sha256(&preimage).into(),
        epoch: next_epoch,
    };
    env.storage()
        .instance()
        .set(&DataKey::RevocationAccumulator, &next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl};

    #[contract]
    struct Harness;

    #[contractimpl]
    impl Harness {}

    fn id(env: &Env, byte: u8) -> BytesN<32> {
        BytesN::from_array(env, &[byte; 32])
    }

    #[test]
    fn empty_accumulator_starts_at_zero_epoch() {
        let env = Env::default();
        let contract_id = env.register(Harness, ());
        env.as_contract(&contract_id, || {
            let acc = current(&env);
            assert_eq!(acc.epoch, 0);
            assert_eq!(acc.root, BytesN::from_array(&env, &[0u8; 32]));
        });
    }

    #[test]
    fn each_revocation_advances_epoch_and_changes_root() {
        let env = Env::default();
        let contract_id = env.register(Harness, ());
        env.as_contract(&contract_id, || {
            let a = revoke(&env, &id(&env, 1));
            let b = revoke(&env, &id(&env, 2));
            assert_eq!(a.epoch, 1);
            assert_eq!(b.epoch, 2);
            assert_ne!(a.root, b.root);
            assert_eq!(current(&env), b);
        });
    }

    #[test]
    fn revocation_order_changes_the_root() {
        let env = Env::default();
        let first = env.register(Harness, ());
        let second = env.register(Harness, ());
        let ab = env.as_contract(&first, || {
            revoke(&env, &id(&env, 1));
            revoke(&env, &id(&env, 2)).root
        });
        let ba = env.as_contract(&second, || {
            revoke(&env, &id(&env, 2));
            revoke(&env, &id(&env, 1)).root
        });
        assert_ne!(ab, ba);
    }
}
