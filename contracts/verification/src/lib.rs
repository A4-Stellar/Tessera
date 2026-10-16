//! Shared strategies for the formal state-invariant suite (issue #296).

use proptest::prelude::*;

/// One operation against the registry contract's public API, as a plain enum
/// rather than calling the contract directly, so a sequence of them can be
/// generated, shrunk, and replayed deterministically by proptest.
#[derive(Clone, Debug)]
pub enum RegistryOp {
    /// Register a new asset with this valuation under a freshly generated issuer.
    Register { valuation: i128 },
    /// Deactivate the asset at this index into the list of ids registered so
    /// far (taken `mod` the list length at apply time, so every generated
    /// value is in range once at least one asset exists).
    Deactivate { index_into_registered: usize },
}

/// A non-negative valuation, biased toward the boundary values that have
/// historically broken fold/sum accounting (`0`, `i128::MAX`, overflow-prone
/// sums of near-`i128::MAX` values).
pub fn valuation() -> impl Strategy<Value = i128> {
    prop_oneof![
        3 => Just(0i128),
        2 => Just(1i128),
        2 => Just(i128::MAX),
        2 => Just(i128::MAX / 2),
        1 => Just(i128::MAX - 1),
        6 => (0i128..1_000_000_000),
    ]
}

fn registry_op() -> impl Strategy<Value = RegistryOp> {
    prop_oneof![
        3 => valuation().prop_map(|valuation| RegistryOp::Register { valuation }),
        2 => any::<usize>()
            .prop_map(|index_into_registered| RegistryOp::Deactivate { index_into_registered }),
    ]
}

/// A sequence of registry operations, long enough that a real run exercises
/// "thousands of simulated transaction sequences" in aggregate once proptest
/// generates many such sequences (see the `ProptestConfig` in the test that
/// uses this).
pub fn registry_op_sequence(max_len: usize) -> impl Strategy<Value = Vec<RegistryOp>> {
    prop::collection::vec(registry_op(), 0..=max_len)
}
