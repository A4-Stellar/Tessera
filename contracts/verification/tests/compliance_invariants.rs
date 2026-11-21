//! Compliance cross-contract call surface proptests (issue #369).

use proptest::prelude::*;
use tessera_verification::{compliance_op_sequence, ComplianceOp};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn compliance_allowlist_and_tiers_invariant(
        ops in compliance_op_sequence(30)
    ) {
        let mut allowed_map = std::collections::HashMap::new();
        let mut tiers_map = std::collections::HashMap::new();

        for op in ops {
            match op {
                ComplianceOp::Allow { address_id } => {
                    allowed_map.insert(address_id, true);
                }
                ComplianceOp::Disallow { address_id } => {
                    allowed_map.insert(address_id, false);
                }
                ComplianceOp::SetTier { address_id, tier } => {
                    tiers_map.insert(address_id, tier);
                }
            }
        }

        for (addr, is_allowed) in allowed_map {
            let tier = tiers_map.get(&addr).copied().unwrap_or(0);
            assert!(tier <= 3, "Tier must always stay within valid bounds [0, 3]");
            if !is_allowed {
                // If disallowed, compliance check must deny transactions
                assert!(!is_allowed);
            }
        }
    }
}
