extern crate std;

use crate::{RegistryContract, RegistryContractClient};
use soroban_sdk::{testutils::Address as _, Address, Env, String};

fn seed(env: &Env, client: &RegistryContractClient<'_>, count: u32, issuer: &Address) {
    env.cost_estimate().budget().reset_unlimited();
    let name = String::from_str(env, "asset");
    for id in 0..count {
        client.register_asset(issuer, issuer, &name, &name, &(id as i128 + 1));
    }
}

fn cpu_cost<F: FnOnce()>(env: &Env, call: F) -> u64 {
    env.cost_estimate().budget().reset_unlimited();
    let before = env.cost_estimate().budget().cpu_instruction_cost();
    call();
    env.cost_estimate()
        .budget()
        .cpu_instruction_cost()
        .saturating_sub(before)
}

#[test]
#[ignore = "manual benchmark: seeds 1k/10k/100k assets"]
fn mpt_proof_vs_linear_scan() {
    for count in [1_000u32, 10_000, 100_000] {
        let env = Env::default();
        env.mock_all_auths();
        let issuer = Address::generate(&env);
        let id = env.register(RegistryContract, ());
        let client = RegistryContractClient::new(&env, &id);
        client.initialize(&issuer);
        seed(&env, &client, count, &issuer);

        let scan = cpu_cost(&env, || {
            client.get_all_assets();
        });
        let proof = cpu_cost(&env, || {
            client.get_proof(&((count - 1) as u64));
        });
        std::println!("assets={count} linear_scan_cpu={scan} mpt_proof_cpu={proof}");
        assert!(proof < scan);
    }
}
