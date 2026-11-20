use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, Env, String};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};

proptest! {
    #[test]
    fn velocity_accumulator_never_exceeds_cap(
        amounts in prop::collection::vec(1i128..50, 1..20),
        cap in 1i128..200,
        window in 1u32..10,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let id = env.register(ComplianceContract, ());
        let client = ComplianceContractClient::new(&env, &id);
        client.initialize(&admin);
        client.set_velocity_window(&admin, &window);
        client.set_default_velocity_cap(&admin, &cap);

        let account = Address::generate(&env);
        let jurisdiction = String::from_str(&env, "US");
        let mut accepted = 0i128;

        for amount in amounts {
            let before = client.get_velocity_state(&account, &jurisdiction);
            let allowed = client.check_velocity(&account, &amount, &jurisdiction);
            let after = client.get_velocity_state(&account, &jurisdiction);
            if allowed {
                accepted += amount;
            } else {
                prop_assert_eq!(after.clone(), before.clone());
            }
            prop_assert!(accepted <= cap);
            prop_assert_eq!(
                after.map(|state| state.used).unwrap_or(0),
                accepted
            );
        }
    }
}
