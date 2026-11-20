use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String};
use tessera_registry::{RegistryContract, RegistryContractClient};

proptest! {
    #[test]
    fn sparse_mpt_proofs_are_valid_only_for_committed_entries(
        valuations in prop::collection::vec(0i128..10_000, 1..8),
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let id = env.register(RegistryContract, ());
        let client = RegistryContractClient::new(&env, &id);
        client.initialize(&admin);

        let issuer = Address::generate(&env);
        let name = String::from_str(&env, "Asset");
        let asset_type = String::from_str(&env, "Equity");
        let mut ids = Vec::new();
        for valuation in &valuations {
            let token = Address::generate(&env);
            ids.push(client.register_asset(
                &issuer,
                &token,
                &name,
                &asset_type,
                valuation,
            ));
        }

        let root = client.get_mpt_root();
        prop_assert_ne!(root.clone(), BytesN::from_array(&env, &[0u8; 32]));

        for asset_id in &ids {
            let proof = client.get_proof(asset_id);
            prop_assert_eq!(proof.root.clone(), root.clone());
            prop_assert!(client.verify_mpt_proof(&proof));
        }

        let mut tampered = client.get_proof(&ids[0]);
        tampered.leaf.valuation += 1;
        prop_assert!(!client.verify_mpt_proof(&tampered));

        let mut wrong_root = client.get_proof(&ids[0]);
        wrong_root.root = BytesN::from_array(&env, &[0xffu8; 32]);
        prop_assert!(!client.verify_mpt_proof(&wrong_root));
    }
}
