//! Issue #317: verify a real aggregated proof produced by
//! `tessera-zk-aggregator` at the production shape (32 subjects, depth 20),
//! Groth16 over BN254 with a single committed public input.
extern crate std;

use soroban_sdk::{testutils::Address as _, testutils::Ledger, Address, BytesN, Env, Vec};

use crate::{
    AggregatedProof, AggregationVerifyingKey, ComplianceContract, ComplianceContractClient, Error,
};

const FIXTURE: &str = include_str!("test_fixtures/zk_aggregate_32.json");

/// Soroban's per-transaction limits the 40% budget target is measured against.
const TX_MAX_CPU_INSTRUCTIONS: u64 = 100_000_000;
const TX_MAX_MEMORY_BYTES: u64 = 41_943_040;

fn bytes<const N: usize>(env: &Env, h: &serde_json::Value) -> BytesN<N> {
    let v = hex::decode(h.as_str().unwrap()).unwrap();
    BytesN::from_array(env, &v.try_into().unwrap())
}

struct Fixture<'a> {
    env: Env,
    admin: Address,
    issuer: Address,
    client: ComplianceContractClient<'a>,
    vk: AggregationVerifyingKey,
    root: BytesN<32>,
    subjects: Vec<Address>,
    valid_until: u32,
    proof: AggregatedProof,
}

fn ce(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

fn setup<'a>() -> Fixture<'a> {
    let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let id = env.register(ComplianceContract, ());
    let client = ComplianceContractClient::new(&env, &id);
    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    client.initialize(&admin);

    let vkj = &json["verifying_key"];
    let mut ic = Vec::new(&env);
    for p in vkj["ic"].as_array().unwrap() {
        ic.push_back(bytes::<64>(&env, p));
    }
    let vk = AggregationVerifyingKey {
        slots: vkj["slots"].as_u64().unwrap() as u32,
        alpha_g1: bytes(&env, &vkj["alpha_g1"]),
        beta_g2: bytes(&env, &vkj["beta_g2"]),
        gamma_g2: bytes(&env, &vkj["gamma_g2"]),
        delta_g2: bytes(&env, &vkj["delta_g2"]),
        ic,
    };
    let b = &json["bundle"];
    let mut subjects = Vec::new(&env);
    for s in b["subjects"].as_array().unwrap() {
        subjects.push_back(Address::from_str(&env, s.as_str().unwrap()));
    }
    let proof = AggregatedProof {
        a: bytes(&env, &b["proof"]["a"]),
        b: bytes(&env, &b["proof"]["b"]),
        c: bytes(&env, &b["proof"]["c"]),
    };
    let root = bytes::<32>(&env, &b["issuer_root"]);

    client.set_aggregation_vk(&admin, &vk);
    client.set_zk_issuer_root(&admin, &issuer, &root);

    Fixture {
        valid_until: b["valid_until"].as_u64().unwrap() as u32,
        env,
        admin,
        issuer,
        client,
        vk,
        root,
        subjects,
        proof,
    }
}

#[test]
fn real_32_subject_proof_verifies_and_accredits_everyone() {
    let f = setup();
    assert_eq!(f.subjects.len(), 32);
    assert!(f
        .client
        .verify_aggregated_kyc(&f.issuer, &f.subjects, &f.valid_until, &f.proof));

    let n = f
        .client
        .submit_aggregated_kyc(&f.issuer, &f.subjects, &f.valid_until, &f.proof);
    assert_eq!(n, 32);
    for s in f.subjects.iter() {
        let a = f.client.zk_accreditation(&s).expect("accredited");
        assert_eq!(a.issuer, f.issuer);
        assert_eq!(a.valid_until, f.valid_until);
    }

    // Accreditation lapses with the proof's validity window.
    f.env.ledger().set_sequence_number(f.valid_until);
    assert!(f
        .client
        .zk_accreditation(&f.subjects.get(0).unwrap())
        .is_none());
}

#[test]
fn verification_stays_under_40_percent_of_the_transaction_budget() {
    let f = setup();
    let mut budget = f.env.cost_estimate().budget();
    budget.reset_default();
    f.client
        .submit_aggregated_kyc(&f.issuer, &f.subjects, &f.valid_until, &f.proof);
    let cpu = budget.cpu_instruction_cost();
    let mem = budget.memory_bytes_cost();
    std::println!("aggregated 32-subject submission: cpu={cpu} mem={mem}");
    if std::env::var("ZK_BUDGET_BREAKDOWN").is_ok() {
        budget.print();
    }
    assert!(
        cpu * 100 < TX_MAX_CPU_INSTRUCTIONS * 40,
        "cpu {cpu} is not under 40% of {TX_MAX_CPU_INSTRUCTIONS}"
    );
    assert!(
        mem * 100 < TX_MAX_MEMORY_BYTES * 40,
        "memory {mem} is not under 40% of {TX_MAX_MEMORY_BYTES}"
    );
}

#[test]
fn any_change_to_the_statement_breaks_the_proof() {
    let f = setup();
    let ok = |subjects: &Vec<Address>, valid_until: u32| {
        f.client
            .verify_aggregated_kyc(&f.issuer, subjects, &valid_until, &f.proof)
    };

    // Reordered subjects.
    let mut swapped = f.subjects.clone();
    let first = swapped.get(0).unwrap();
    swapped.set(0, swapped.get(1).unwrap());
    swapped.set(1, first);
    assert!(!ok(&swapped, f.valid_until));

    // A subject the issuer never credentialed.
    let mut outsider = f.subjects.clone();
    outsider.set(5, Address::generate(&f.env));
    assert!(!ok(&outsider, f.valid_until));

    // A dropped subject (fewer than proven).
    let mut fewer = f.subjects.clone();
    fewer.pop_back();
    assert!(!ok(&fewer, f.valid_until));

    // A longer claimed validity than proven.
    assert!(!ok(&f.subjects, f.valid_until + 1));

    // Submitting a rejected proof fails and records nothing.
    assert_eq!(
        f.client
            .try_submit_aggregated_kyc(&f.issuer, &swapped, &f.valid_until, &f.proof),
        Err(Ok(ce(Error::ZkProofRejected)))
    );
    assert!(f
        .client
        .zk_accreditation(&f.subjects.get(0).unwrap())
        .is_none());
}

#[test]
fn issuer_root_rotation_revokes_old_proofs() {
    let f = setup();
    f.client
        .update_zk_issuer_root(&f.issuer, &BytesN::from_array(&f.env, &[1u8; 32]));
    assert!(!f
        .client
        .verify_aggregated_kyc(&f.issuer, &f.subjects, &f.valid_until, &f.proof));
    f.client.update_zk_issuer_root(&f.issuer, &f.root);
    assert!(f
        .client
        .verify_aggregated_kyc(&f.issuer, &f.subjects, &f.valid_until, &f.proof));
}

#[test]
fn structural_rejections() {
    let f = setup();
    let try_verify = |issuer: &Address, subjects: &Vec<Address>, valid_until: u32| {
        f.client
            .try_verify_aggregated_kyc(issuer, subjects, &valid_until, &f.proof)
    };

    let stranger = Address::generate(&f.env);
    assert_eq!(
        try_verify(&stranger, &f.subjects, f.valid_until),
        Err(Ok(ce(Error::ZkIssuerNotAuthorized)))
    );

    let mut dup = f.subjects.clone();
    dup.set(1, dup.get(0).unwrap());
    assert_eq!(
        try_verify(&f.issuer, &dup, f.valid_until),
        Err(Ok(ce(Error::ZkInvalidBatch)))
    );
    assert_eq!(
        try_verify(&f.issuer, &Vec::new(&f.env), f.valid_until),
        Err(Ok(ce(Error::ZkInvalidBatch)))
    );
    let mut too_many = f.subjects.clone();
    too_many.push_back(Address::generate(&f.env));
    assert_eq!(
        try_verify(&f.issuer, &too_many, f.valid_until),
        Err(Ok(ce(Error::ZkInvalidBatch)))
    );

    f.env.ledger().set_sequence_number(f.valid_until);
    assert_eq!(
        try_verify(&f.issuer, &f.subjects, f.valid_until),
        Err(Ok(ce(Error::ZkProofExpired)))
    );

    f.client.revoke_zk_issuer(&f.admin, &f.issuer);
    f.env.ledger().set_sequence_number(100);
    assert_eq!(
        try_verify(&f.issuer, &f.subjects, f.valid_until),
        Err(Ok(ce(Error::ZkIssuerNotAuthorized)))
    );
}

#[test]
fn verifying_key_must_be_set_and_well_formed() {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(ComplianceContract, ());
    let client = ComplianceContractClient::new(&env, &id);
    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    client.initialize(&admin);
    client.set_zk_issuer_root(&admin, &issuer, &BytesN::from_array(&env, &[0u8; 32]));

    let f = setup();
    let subjects = Vec::from_array(&env, [Address::generate(&env)]);
    let proof = AggregatedProof {
        a: BytesN::from_array(&env, &f.proof.a.to_array()),
        b: BytesN::from_array(&env, &f.proof.b.to_array()),
        c: BytesN::from_array(&env, &f.proof.c.to_array()),
    };
    assert_eq!(
        client.try_verify_aggregated_kyc(&issuer, &subjects, &10, &proof),
        Err(Ok(ce(Error::ZkVerifyingKeyNotSet)))
    );

    let mut short = AggregationVerifyingKey {
        slots: 32,
        alpha_g1: BytesN::from_array(&env, &f.vk.alpha_g1.to_array()),
        beta_g2: BytesN::from_array(&env, &f.vk.beta_g2.to_array()),
        gamma_g2: BytesN::from_array(&env, &f.vk.gamma_g2.to_array()),
        delta_g2: BytesN::from_array(&env, &f.vk.delta_g2.to_array()),
        ic: Vec::new(&env),
    };
    // The circuit has one public input, so exactly two IC points.
    for _ in 0..3 {
        short.ic.push_back(BytesN::from_array(&env, &[0u8; 64]));
    }
    assert_eq!(
        client.try_set_aggregation_vk(&admin, &short),
        Err(Ok(ce(Error::ZkInvalidVerifyingKey)))
    );
    short.ic.pop_back();
    short.slots = 33;
    assert_eq!(
        client.try_set_aggregation_vk(&admin, &short),
        Err(Ok(ce(Error::ZkInvalidVerifyingKey)))
    );
}
