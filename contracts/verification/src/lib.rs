//! Shared strategies and Kani proofs for the formal state-invariant suite (issues #296, #369).

pub mod kani_proofs;

use proptest::prelude::*;

// -------------------------------------------------------------------------
// REGISTRY STRATEGIES
// -------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum RegistryOp {
    Register { valuation: i128 },
    Deactivate { index_into_registered: usize },
}

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

pub fn registry_op_sequence(max_len: usize) -> impl Strategy<Value = Vec<RegistryOp>> {
    prop::collection::vec(registry_op(), 0..=max_len)
}

// -------------------------------------------------------------------------
// COMPLIANCE STRATEGIES
// -------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum ComplianceOp {
    Allow { address_id: u32 },
    Disallow { address_id: u32 },
    SetTier { address_id: u32, tier: u32 },
}

pub fn compliance_op() -> impl Strategy<Value = ComplianceOp> {
    prop_oneof![
        3 => (0u32..50).prop_map(|address_id| ComplianceOp::Allow { address_id }),
        2 => (0u32..50).prop_map(|address_id| ComplianceOp::Disallow { address_id }),
        2 => (0u32..50, 0u32..4).prop_map(|(address_id, tier)| ComplianceOp::SetTier { address_id, tier }),
    ]
}

pub fn compliance_op_sequence(max_len: usize) -> impl Strategy<Value = Vec<ComplianceOp>> {
    prop::collection::vec(compliance_op(), 0..=max_len)
}

// -------------------------------------------------------------------------
// ASSET-TOKEN STRATEGIES
// -------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum AssetTokenOp {
    Mint { to_id: u32, amount: i128 },
    Transfer { from_id: u32, to_id: u32, amount: i128 },
    Burn { from_id: u32, amount: i128 },
}

pub fn asset_token_op() -> impl Strategy<Value = AssetTokenOp> {
    prop_oneof![
        3 => (0u32..10, 1i128..1_000_000).prop_map(|(to_id, amount)| AssetTokenOp::Mint { to_id, amount }),
        4 => (0u32..10, 0u32..10, 1i128..500_000).prop_map(|(from_id, to_id, amount)| AssetTokenOp::Transfer { from_id, to_id, amount }),
        2 => (0u32..10, 1i128..200_000).prop_map(|(from_id, amount)| AssetTokenOp::Burn { from_id, amount }),
    ]
}

pub fn asset_token_op_sequence(max_len: usize) -> impl Strategy<Value = Vec<AssetTokenOp>> {
    prop::collection::vec(asset_token_op(), 0..=max_len)
}

// -------------------------------------------------------------------------
// DIVIDEND STRATEGIES
// -------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum DividendOp {
    Deposit { amount: i128 },
    Claim { holder_id: u32 },
}

pub fn dividend_op() -> impl Strategy<Value = DividendOp> {
    prop_oneof![
        3 => (1i128..10_000_000).prop_map(|amount| DividendOp::Deposit { amount }),
        4 => (0u32..20).prop_map(|holder_id| DividendOp::Claim { holder_id }),
    ]
}

pub fn dividend_op_sequence(max_len: usize) -> impl Strategy<Value = Vec<DividendOp>> {
    prop::collection::vec(dividend_op(), 0..=max_len)
}
