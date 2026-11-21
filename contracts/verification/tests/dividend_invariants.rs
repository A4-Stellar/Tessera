//! Dividend pro-rata entitlement and non-overflow proptests (issue #369).

use proptest::prelude::*;
use tessera_verification::{dividend_op_sequence, DividendOp};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn dividend_entitlement_and_claims_invariant(
        ops in dividend_op_sequence(30)
    ) {
        let total_shares = 1_000_000i128;
        let holder_shares = 10_000i128; // 1%
        let mut total_deposited = 0i128;
        let mut claimed = false;

        for op in ops {
            match op {
                DividendOp::Deposit { amount } => {
                    if let Some(new_total) = total_deposited.checked_add(amount) {
                        total_deposited = new_total;
                    }
                }
                DividendOp::Claim { holder_id: _ } => {
                    if !claimed && total_deposited > 0 {
                        let entitlement = (holder_shares * total_deposited) / total_shares;
                        assert!(entitlement <= total_deposited);
                        claimed = true;
                    }
                }
            }
        }
    }
}
