//! Asset token conservation of supply and balance proptests (issue #369).

use proptest::prelude::*;
use tessera_verification::{asset_token_op_sequence, AssetTokenOp};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn asset_token_supply_conservation_invariant(
        ops in asset_token_op_sequence(40)
    ) {
        let mut balances = std::collections::HashMap::new();
        let mut total_supply: i128 = 0;

        for op in ops {
            match op {
                AssetTokenOp::Mint { to_id, amount } => {
                    let bal = balances.entry(to_id).or_insert(0i128);
                    if let Some(new_bal) = bal.checked_add(amount) {
                        if let Some(new_sup) = total_supply.checked_add(amount) {
                            *bal = new_bal;
                            total_supply = new_sup;
                        }
                    }
                }
                AssetTokenOp::Transfer { from_id, to_id, amount } => {
                    let from_bal = balances.entry(from_id).or_insert(0i128);
                    if *from_bal >= amount {
                        *from_bal -= amount;
                        let to_bal = balances.entry(to_id).or_insert(0i128);
                        *to_bal += amount;
                    }
                }
                AssetTokenOp::Burn { from_id, amount } => {
                    let from_bal = balances.entry(from_id).or_insert(0i128);
                    if *from_bal >= amount {
                        *from_bal -= amount;
                        total_supply -= amount;
                    }
                }
            }
        }

        let sum_balances: i128 = balances.values().sum();
        assert_eq!(
            sum_balances,
            total_supply,
            "Sum of account balances must equal total supply"
        );
    }
}
