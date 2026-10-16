use ark_bn254::{Bn254, Fr};
use ark_groth16::Groth16;
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_snark::SNARK;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::credentials::CredentialError;
use crate::encode::{
    address_scval_xdr, fr_from_hex, fr_to_hex, g1_from_hex, g1_hex, g2_from_hex, g2_hex,
    subject_field,
};
use crate::{commitment, poseidon_config, Accumulator, AggregationCircuit, CredentialTree};

const SLOTS: usize = 3;
const DEPTH: usize = 4;

fn strkey(n: u8) -> String {
    stellar_strkey::ed25519::PublicKey([n; 32]).to_string()
}

fn tree() -> CredentialTree {
    let entries = (1..=5u8)
        .map(|n| (strkey(n), 1_000 + n as u32 * 10))
        .collect();
    CredentialTree::build(&poseidon_config(), DEPTH, entries).unwrap()
}

fn satisfied(circuit: AggregationCircuit) -> bool {
    let cs = ConstraintSystem::<Fr>::new_ref();
    circuit.generate_constraints(cs.clone()).unwrap();
    cs.is_satisfied().unwrap()
}

#[test]
fn every_issued_credential_leads_to_the_root() {
    let t = tree();
    for cred in t.credentials() {
        let root = crate::credentials::credential_root(&poseidon_config(), &cred).unwrap();
        assert_eq!(root, t.root());
        assert_eq!(cred.root, fr_to_hex(&t.root()));
    }
}

#[test]
fn honest_batch_satisfies_the_circuit_including_padding() {
    let t = tree();
    let creds = t.credentials();
    // Two of three slots live, one padded.
    let acc = Accumulator::build(
        &poseidon_config(),
        t.root(),
        DEPTH,
        SLOTS,
        creds[1..3].to_vec(),
        None,
    )
    .unwrap();
    assert_eq!(acc.valid_until, 1_019); // min expiry 1_020 - 1
    assert_eq!(acc.public_inputs().len(), 1);
    assert!(satisfied(AggregationCircuit::from_accumulator(
        poseidon_config(),
        DEPTH,
        &acc
    )));
}

/// Re-derive the public commitment from a (possibly tampered) witness, so a
/// test isolates the constraint it targets rather than tripping the hash.
fn recommit(mut c: AggregationCircuit) -> AggregationCircuit {
    c.commitment = commitment(&c.root, c.valid_until, c.count, &c.subjects, c.slots);
    c
}

#[test]
fn circuit_rejects_cheating_witnesses() {
    let t = tree();
    let acc = Accumulator::build(
        &poseidon_config(),
        t.root(),
        DEPTH,
        SLOTS,
        t.credentials()[..2].to_vec(),
        None,
    )
    .unwrap();
    let honest = AggregationCircuit::from_accumulator(poseidon_config(), DEPTH, &acc);
    assert!(
        satisfied(recommit(honest.clone())),
        "recommit is a no-op on an honest witness"
    );

    let mut c = honest.clone();
    c.commitment += Fr::from(1u64);
    assert!(
        !satisfied(c),
        "witness not matching the public commitment accepted"
    );

    let mut c = honest.clone();
    c.valid_until = 1_011; // first credential expires at 1_010
    assert!(!satisfied(recommit(c)), "expired credential accepted");

    let mut c = honest.clone();
    c.subjects[0] = subject_field(&strkey(9)).unwrap();
    assert!(!satisfied(recommit(c)), "subject not in the tree accepted");

    let mut c = honest.clone();
    c.count = 3;
    assert!(
        !satisfied(recommit(c)),
        "count larger than the live slots accepted"
    );

    let mut c = honest.clone();
    c.subjects[2] = subject_field(&strkey(3)).unwrap();
    assert!(
        !satisfied(recommit(c)),
        "non-zero subject in a padding slot accepted"
    );

    let mut c = honest;
    c.root = Fr::from(7u64);
    assert!(!satisfied(recommit(c)), "wrong root accepted");
}

#[test]
fn accumulator_checks_each_credential() {
    let t = tree();
    let creds = t.credentials();
    let config = poseidon_config();

    let mut forged = creds[0].clone();
    forged.expiry += 1;
    assert_eq!(
        Accumulator::build(&config, t.root(), DEPTH, SLOTS, vec![forged], None).unwrap_err(),
        CredentialError::BadPath(creds[0].subject.clone())
    );
    assert!(matches!(
        Accumulator::build(
            &config,
            t.root(),
            DEPTH,
            SLOTS,
            vec![creds[0].clone()],
            Some(1_010)
        ),
        Err(CredentialError::Expired(..))
    ));
    assert!(matches!(
        Accumulator::build(
            &config,
            t.root(),
            DEPTH,
            SLOTS,
            vec![creds[0].clone(), creds[0].clone()],
            None
        ),
        Err(CredentialError::Duplicate(_))
    ));
    assert_eq!(
        Accumulator::build(&config, t.root(), DEPTH, SLOTS, creds[..4].to_vec(), None).unwrap_err(),
        CredentialError::TooMany(4, SLOTS)
    );
}

#[test]
fn groth16_round_trip() {
    let t = tree();
    let acc = Accumulator::build(
        &poseidon_config(),
        t.root(),
        DEPTH,
        SLOTS,
        t.credentials()[2..5].to_vec(),
        None,
    )
    .unwrap();
    let mut rng = ChaCha20Rng::seed_from_u64(1);
    let (pk, vk) = Groth16::<Bn254>::circuit_specific_setup(
        AggregationCircuit::blank(poseidon_config(), SLOTS, DEPTH),
        &mut rng,
    )
    .unwrap();
    let proof = Groth16::<Bn254>::prove(
        &pk,
        AggregationCircuit::from_accumulator(poseidon_config(), DEPTH, &acc),
        &mut rng,
    )
    .unwrap();
    let inputs = acc.public_inputs();
    assert_eq!(inputs.len(), 1, "single public input");
    assert!(Groth16::<Bn254>::verify(&vk, &inputs, &proof).unwrap());

    // Any change to the committed statement changes the one public input.
    let mut swapped = acc.subject_fields.clone();
    swapped.swap(0, 1);
    let c = commitment(&acc.root, acc.valid_until, 3, &swapped, SLOTS);
    assert!(!Groth16::<Bn254>::verify(&vk, &[c], &proof).unwrap());
    let c = commitment(
        &acc.root,
        acc.valid_until + 1,
        3,
        &acc.subject_fields,
        SLOTS,
    );
    assert!(!Groth16::<Bn254>::verify(&vk, &[c], &proof).unwrap());

    // Soroban encodings round-trip.
    let back = crate::encode::SorobanProof::from(&proof)
        .to_arkworks()
        .unwrap();
    assert_eq!(back, proof);
    let svk = crate::encode::SorobanVerifyingKey::new(&vk, SLOTS);
    assert_eq!(svk.ic.len(), 2);
    assert_eq!(svk.to_arkworks().unwrap(), vk);
}

#[test]
fn encodings_match_the_contract() {
    // ScVal::Address(Account(Ed25519([1; 32]))): SCV_ADDRESS=18, ACCOUNT=0, ED25519=0.
    let xdr = address_scval_xdr(&strkey(1)).unwrap();
    assert_eq!(&xdr[..12], &[0, 0, 0, 18, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&xdr[12..], &[1u8; 32]);
    let contract = stellar_strkey::Contract([2; 32]).to_string();
    assert_eq!(
        &address_scval_xdr(&contract).unwrap()[..8],
        &[0, 0, 0, 18, 0, 0, 0, 1]
    );

    let s = subject_field(&strkey(1)).unwrap();
    assert_eq!(fr_to_hex(&s)[..2], *"00");
    assert_eq!(fr_from_hex(&fr_to_hex(&s)), Some(s));
    // The BN254 scalar modulus itself is not a canonical scalar.
    assert_eq!(
        fr_from_hex("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001"),
        None
    );

    // Generators in the Ethereum layout (EIP-196/197).
    use ark_ec::AffineRepr;
    let g1 = ark_bn254::G1Affine::generator();
    assert_eq!(
        g1_hex(&g1),
        "0000000000000000000000000000000000000000000000000000000000000001\
         0000000000000000000000000000000000000000000000000000000000000002"
    );
    assert_eq!(g1_from_hex(&g1_hex(&g1)), Some(g1));
    let g2 = ark_bn254::G2Affine::generator();
    // EIP-197 G2 generator x = (c1, c0) starts with x.c1 = 0x198e9393…
    assert!(
        g2_hex(&g2).starts_with("198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2")
    );
    assert_eq!(g2_from_hex(&g2_hex(&g2)), Some(g2));
    // (0, 1) is not on y^2 = x^3 + 3.
    assert_eq!(g1_from_hex(&format!("{}01", "00".repeat(63))), None);
}
