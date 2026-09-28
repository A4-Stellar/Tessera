extern crate std;

use super::*;
use crate::bulletproofs::{self, ProofError};
use crate::pedersen::{fr_u64, Point};
use crate::prover;
use crate::Error;
use soroban_sdk::crypto::bls12_381::Fr;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{contract, contractimpl, contracttype, BytesN, Env};

/// Stand-in for `tessera-compliance` exposing the same `is_allowed(Address)
/// -> bool` entry point the token calls, with allowlist, suspension and
/// jurisdiction blocking. (The real crate cannot currently be linked into
/// tests: `tessera-common` does not compile against soroban-sdk 25.3.)
#[contracttype]
enum MockKey {
    Jurisdiction(Address),
    Suspended(Address),
    Blocked(String),
}

#[contract]
struct MockCompliance;

#[contractimpl]
impl MockCompliance {
    pub fn add_to_allowlist(env: Env, who: Address, jurisdiction: String) {
        env.storage()
            .persistent()
            .set(&MockKey::Jurisdiction(who.clone()), &jurisdiction);
        env.storage().persistent().remove(&MockKey::Suspended(who));
    }

    pub fn suspend(env: Env, who: Address) {
        env.storage()
            .persistent()
            .set(&MockKey::Suspended(who), &true);
    }

    pub fn block_jurisdiction(env: Env, jurisdiction: String) {
        env.storage()
            .persistent()
            .set(&MockKey::Blocked(jurisdiction), &true);
    }

    pub fn is_allowed(env: Env, address: Address) -> bool {
        let store = env.storage().persistent();
        let Some(jurisdiction) = store.get::<_, String>(&MockKey::Jurisdiction(address.clone()))
        else {
            return false;
        };
        !store.has(&MockKey::Suspended(address)) && !store.has(&MockKey::Blocked(jurisdiction))
    }
}

/// Off-chain view a holder keeps of their own commitment openings.
#[derive(Clone)]
struct Opening {
    value: u64,
    blinding: Fr,
}

struct Setup<'a> {
    env: Env,
    token: ConfidentialTokenContractClient<'a>,
    compliance: MockComplianceClient<'a>,
    admin: Address,
    seed: u32,
}

impl<'a> Setup<'a> {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.cost_estimate().budget().reset_unlimited();

        let admin = Address::generate(&env);
        let compliance_id = env.register(MockCompliance, ());
        let compliance = MockComplianceClient::new(&env, &compliance_id);
        compliance.add_to_allowlist(&admin, &String::from_str(&env, "US"));

        let token_id = env.register(ConfidentialTokenContract, ());
        let token = ConfidentialTokenContractClient::new(&env, &token_id);
        token.initialize(
            &admin,
            &String::from_str(&env, "Tessera Private Credit Fund"),
            &String::from_str(&env, "TPCF"),
            &7,
            &compliance_id,
        );
        // Two batches, as a deployment would to stay inside per-tx budgets.
        assert_eq!(token.extend_generators(&64), 64);
        assert!(!token.generators_ready());
        assert_eq!(token.extend_generators(&1000), GENERATOR_COUNT);
        assert!(token.generators_ready());

        Self {
            env,
            token,
            compliance,
            admin,
            seed: 0,
        }
    }

    fn investor(&self, jurisdiction: &str) -> Address {
        let who = Address::generate(&self.env);
        self.compliance
            .add_to_allowlist(&who, &String::from_str(&self.env, jurisdiction));
        who
    }

    fn fresh_seed(&mut self) -> BytesN<32> {
        self.seed += 1;
        let mut bytes = [7u8; 32];
        bytes[..4].copy_from_slice(&self.seed.to_be_bytes());
        BytesN::from_array(&self.env, &bytes)
    }

    fn scalar(&mut self) -> Fr {
        let seed = self.fresh_seed();
        Fr::from_bytes(self.env.crypto().sha256(&seed.into()).to_bytes())
    }

    fn gens(&self) -> bulletproofs::Generators {
        let mut g = Vec::new(&self.env);
        let mut h = Vec::new(&self.env);
        for i in 0..GENERATOR_COUNT {
            g.push_back(pedersen::vector_generator(&self.env, b'G', i).to_bytes());
            h.push_back(pedersen::vector_generator(&self.env, b'H', i).to_bytes());
        }
        bulletproofs::Generators { g, h }
    }

    fn commit(&self, o: &Opening) -> Point {
        pedersen::commit(&self.env, &fr_u64(&self.env, o.value), &o.blinding).to_bytes()
    }

    /// Build a transfer of `amount` from an account whose available balance
    /// opens to `balance`. Uses raw field values so dishonest (out-of-range)
    /// transfers can be attempted too.
    fn build_transfer(
        &mut self,
        from: &Address,
        to: &Address,
        balance: &Opening,
        amount: u64,
    ) -> (Point, RangeProof, Opening, Opening) {
        let bls = self.env.crypto().bls12_381();
        let amount_blinding = self.scalar();
        let amount_fr = fr_u64(&self.env, amount);
        let remaining_value = bls.fr_sub(&fr_u64(&self.env, balance.value), &amount_fr);
        let remaining_blinding = bls.fr_sub(&balance.blinding, &amount_blinding);

        let mut values = Vec::new(&self.env);
        values.push_back(amount_fr);
        values.push_back(remaining_value);
        let mut blindings = Vec::new(&self.env);
        blindings.push_back(amount_blinding.clone());
        blindings.push_back(remaining_blinding.clone());

        let context = self.token.transfer_context(from, to);
        let seed = self.fresh_seed();
        let (proof, commitments) = prover::prove(
            &self.env,
            &self.gens(),
            &values,
            &blindings,
            RANGE_BITS,
            &context,
            &seed,
        );
        (
            commitments.get_unchecked(0),
            proof,
            Opening {
                value: amount,
                blinding: amount_blinding,
            },
            Opening {
                value: balance.value.wrapping_sub(amount),
                blinding: remaining_blinding,
            },
        )
    }

    fn build_burn(&mut self, from: &Address, balance: &Opening, amount: u64) -> RangeProof {
        let bls = self.env.crypto().bls12_381();
        let mut values = Vec::new(&self.env);
        values.push_back(bls.fr_sub(
            &fr_u64(&self.env, balance.value),
            &fr_u64(&self.env, amount),
        ));
        let mut blindings = Vec::new(&self.env);
        blindings.push_back(balance.blinding.clone());
        let context = self.token.burn_context(from, &i128::from(amount));
        let seed = self.fresh_seed();
        prover::prove(
            &self.env,
            &self.gens(),
            &values,
            &blindings,
            RANGE_BITS,
            &context,
            &seed,
        )
        .0
    }

    /// Mint publicly and apply, returning the holder's opening.
    fn funded(&self, who: &Address, amount: u64) -> Opening {
        self.token.mint(&self.admin, who, &i128::from(amount));
        self.token.apply_pending(who);
        Opening {
            value: amount,
            blinding: fr_u64(&self.env, 0),
        }
    }
}

/// Contract error as surfaced by the generated `try_` client methods.
fn err(e: Error) -> soroban_sdk::Error {
    e.into()
}

fn memo(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"ciphertext-of-opening-for-recipient")
}

#[test]
fn pedersen_commitments_are_homomorphic_and_hiding() {
    let s = Setup::new();
    let env = &s.env;
    let bls = env.crypto().bls12_381();
    let (r1, r2) = (fr_u64(env, 11), fr_u64(env, 29));
    let c1 = pedersen::commit(env, &fr_u64(env, 300), &r1).to_bytes();
    let c2 = pedersen::commit(env, &fr_u64(env, 700), &r2).to_bytes();
    let sum = pedersen::commit(env, &fr_u64(env, 1000), &bls.fr_add(&r1, &r2)).to_bytes();
    assert_eq!(pedersen::add(env, &c1, &c2), sum);
    assert_eq!(pedersen::sub(env, &sum, &c2), c1);
    // Same value, different blinding: commitments are unlinkable.
    assert_ne!(c1, pedersen::commit(env, &fr_u64(env, 300), &r2).to_bytes());
    // Zero value with zero blinding is the identity (empty balance).
    assert_eq!(
        pedersen::commit(env, &fr_u64(env, 0), &fr_u64(env, 0)).to_bytes(),
        pedersen::identity(env)
    );
    assert_eq!(pedersen::sub(env, &c1, &c1), pedersen::identity(env));
}

#[test]
fn range_proof_round_trip_single_and_aggregated() {
    let mut s = Setup::new();
    let env = s.env.clone();
    let gens = s.gens();
    let ctx = Bytes::from_slice(&env, b"ctx");
    for values_u64 in [&[0u64][..], &[u64::MAX][..], &[42, 1 << 40][..]] {
        let mut values = Vec::new(&env);
        let mut blindings = Vec::new(&env);
        for v in values_u64 {
            values.push_back(fr_u64(&env, *v));
            blindings.push_back(s.scalar());
        }
        let seed = s.fresh_seed();
        let (proof, commitments) = prover::prove(&env, &gens, &values, &blindings, 64, &ctx, &seed);
        assert_eq!(
            bulletproofs::verify(&env, &gens, &commitments, &proof, 64, &ctx),
            Ok(())
        );
        // Bound to its context.
        let other = Bytes::from_slice(&env, b"other");
        assert_eq!(
            bulletproofs::verify(&env, &gens, &commitments, &proof, 64, &other),
            Err(ProofError::Rejected)
        );
    }
}

#[test]
fn range_proof_rejects_negative_value() {
    let mut s = Setup::new();
    let env = s.env.clone();
    let gens = s.gens();
    let ctx = Bytes::from_slice(&env, b"ctx");
    let mut values = Vec::new(&env);
    values.push_back(pedersen::fr_neg(&env, &fr_u64(&env, 5))); // "-5"
    let mut blindings = Vec::new(&env);
    blindings.push_back(s.scalar());
    let seed = s.fresh_seed();
    let (proof, commitments) = prover::prove(&env, &gens, &values, &blindings, 64, &ctx, &seed);
    assert_eq!(
        bulletproofs::verify(&env, &gens, &commitments, &proof, 64, &ctx),
        Err(ProofError::Rejected)
    );
}

#[test]
fn range_proof_rejects_malformed_proofs() {
    let mut s = Setup::new();
    let env = s.env.clone();
    let gens = s.gens();
    let ctx = Bytes::from_slice(&env, b"ctx");
    let mut values = Vec::new(&env);
    values.push_back(fr_u64(&env, 9));
    let mut blindings = Vec::new(&env);
    blindings.push_back(s.scalar());
    let seed = s.fresh_seed();
    let (proof, commitments) = prover::prove(&env, &gens, &values, &blindings, 64, &ctx, &seed);

    let mut bad = proof.clone();
    bad.t_x = fr_u64(&env, 1).to_bytes();
    assert_eq!(
        bulletproofs::verify(&env, &gens, &commitments, &bad, 64, &ctx),
        Err(ProofError::Rejected)
    );

    let mut bad = proof.clone();
    bad.ipp_a = BytesN::from_array(&env, &[0xff; 32]);
    assert_eq!(
        bulletproofs::verify(&env, &gens, &commitments, &bad, 64, &ctx),
        Err(ProofError::NonCanonicalScalar)
    );

    let mut bad = proof.clone();
    bad.l_vec.pop_back();
    assert_eq!(
        bulletproofs::verify(&env, &gens, &commitments, &bad, 64, &ctx),
        Err(ProofError::Shape)
    );

    let mut bad = proof.clone();
    bad.a = bad.s.clone();
    assert_eq!(
        bulletproofs::verify(&env, &gens, &commitments, &bad, 64, &ctx),
        Err(ProofError::Rejected)
    );
}

#[test]
fn confidential_transfer_moves_hidden_amount() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let alice_balance = s.funded(&alice, 1_000_000);
    assert_eq!(s.token.total_supply(), 1_000_000);
    assert_eq!(s.token.available_balance(&alice), s.commit(&alice_balance));

    let (c_amount, proof, amount_open, alice_after) =
        s.build_transfer(&alice, &bob, &alice_balance, 250_000);
    s.token
        .confidential_transfer(&alice, &bob, &c_amount, &proof, &memo(&s.env));

    // The contract's commitments match what each party can open locally.
    assert_eq!(s.token.available_balance(&alice), s.commit(&alice_after));
    assert_eq!(alice_after.value, 750_000);
    assert_eq!(s.token.pending_balance(&bob), s.commit(&amount_open));
    assert_eq!(s.token.available_balance(&bob), pedersen::identity(&s.env));
    assert_eq!(s.token.nonce(&alice), 2); // apply_pending + transfer

    s.token.apply_pending(&bob);
    assert_eq!(s.token.available_balance(&bob), s.commit(&amount_open));
    assert_eq!(s.token.pending_balance(&bob), pedersen::identity(&s.env));
    // Supply is untouched by private transfers.
    assert_eq!(s.token.total_supply(), 1_000_000);

    // Bob can spend what he received.
    let carol = s.investor("DE");
    let (c2, proof2, _, bob_after) = s.build_transfer(&bob, &carol, &amount_open, 100_000);
    s.token
        .confidential_transfer(&bob, &carol, &c2, &proof2, &memo(&s.env));
    assert_eq!(s.token.available_balance(&bob), s.commit(&bob_after));
    assert_eq!(bob_after.value, 150_000);
}

#[test]
fn overspend_is_rejected_by_range_proof() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 100);
    let (c_amount, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 150);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c_amount, &proof, &memo(&s.env)),
        Err(Ok(err(Error::InvalidProof)))
    );
    assert_eq!(s.token.available_balance(&alice), s.commit(&balance));
    assert_eq!(s.token.pending_balance(&bob), pedersen::identity(&s.env));
}

#[test]
fn proofs_cannot_be_replayed_or_redirected() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let mallory = s.investor("FR");
    let balance = s.funded(&alice, 1_000);
    let (c_amount, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 10);

    // Same proof, different recipient.
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &mallory, &c_amount, &proof, &memo(&s.env)),
        Err(Ok(err(Error::InvalidProof)))
    );
    s.token
        .confidential_transfer(&alice, &bob, &c_amount, &proof, &memo(&s.env));
    // Replay after the balance and nonce moved on.
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c_amount, &proof, &memo(&s.env)),
        Err(Ok(err(Error::InvalidProof)))
    );
}

#[test]
fn swapped_amount_commitment_is_rejected() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 1_000);
    let (_, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 10);
    // Claim a larger amount than the proof covers.
    let inflated = s.commit(&Opening {
        value: 900,
        blinding: fr_u64(&s.env, 1),
    });
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &inflated, &proof, &memo(&s.env)),
        Err(Ok(err(Error::InvalidProof)))
    );
}

#[test]
fn identity_allowlist_gates_both_parties() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let outsider = Address::generate(&s.env);
    let balance = s.funded(&alice, 1_000);

    let (c, proof, _, _) = s.build_transfer(&alice, &outsider, &balance, 10);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &outsider, &c, &proof, &memo(&s.env)),
        Err(Ok(err(Error::RecipientNotCompliant)))
    );
    assert_eq!(
        s.token.try_mint(&s.admin, &outsider, &5),
        Err(Ok(err(Error::RecipientNotCompliant)))
    );

    s.compliance.suspend(&alice);
    let (c, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 10);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c, &proof, &memo(&s.env)),
        Err(Ok(err(Error::SenderNotCompliant)))
    );

    // Blocking a jurisdiction in the compliance contract applies here too.
    s.compliance
        .add_to_allowlist(&alice, &String::from_str(&s.env, "US"));
    s.compliance
        .block_jurisdiction(&String::from_str(&s.env, "GB"));
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c, &proof, &memo(&s.env)),
        Err(Ok(err(Error::RecipientNotCompliant)))
    );
}

#[test]
fn burn_reveals_amount_and_proves_remainder() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let balance = s.funded(&alice, 500);

    let too_much = s.build_burn(&alice, &balance, 600);
    assert_eq!(
        s.token.try_burn(&alice, &600, &too_much),
        Err(Ok(err(Error::InvalidProof)))
    );

    let proof = s.build_burn(&alice, &balance, 200);
    s.token.burn(&alice, &200, &proof);
    assert_eq!(s.token.total_supply(), 300);
    let expected = Opening {
        value: 300,
        blinding: balance.blinding.clone(),
    };
    assert_eq!(s.token.available_balance(&alice), s.commit(&expected));
}

#[test]
fn supply_is_capped_to_proof_range() {
    let s = Setup::new();
    let alice = s.investor("US");
    s.token.mint(&s.admin, &alice, &i128::from(u64::MAX));
    assert_eq!(
        s.token.try_mint(&s.admin, &alice, &1),
        Err(Ok(err(Error::SupplyCapExceeded)))
    );
    assert_eq!(
        s.token.try_mint(&s.admin, &alice, &0),
        Err(Ok(err(Error::InvalidAmount)))
    );
}

#[test]
fn guards_on_setup_admin_and_pause() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    assert_eq!(
        s.token.try_mint(&alice, &alice, &5),
        Err(Ok(err(Error::Unauthorized)))
    );

    let balance = s.funded(&alice, 50);
    let (c, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 5);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &alice, &c, &proof, &memo(&s.env)),
        Err(Ok(err(Error::SelfTransfer)))
    );
    let big = Bytes::from_array(&s.env, &[0u8; 1025]);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c, &proof, &big),
        Err(Ok(err(Error::MemoTooLarge)))
    );
    s.token.pause(&s.admin);
    assert_eq!(
        s.token
            .try_confidential_transfer(&alice, &bob, &c, &proof, &memo(&s.env)),
        Err(Ok(err(Error::Paused)))
    );
    s.token.unpause(&s.admin);
    s.token
        .confidential_transfer(&alice, &bob, &c, &proof, &memo(&s.env));

    // A fresh token rejects transfers until generators are derived.
    let token_id = s.env.register(ConfidentialTokenContract, ());
    let fresh = ConfidentialTokenContractClient::new(&s.env, &token_id);
    fresh.initialize(
        &s.admin,
        &String::from_str(&s.env, "X"),
        &String::from_str(&s.env, "X"),
        &7,
        &s.compliance.address,
    );
    fresh.mint(&s.admin, &alice, &10);
    fresh.apply_pending(&alice);
    let proof = s.build_burn(&alice, &balance, 1);
    assert_eq!(
        fresh.try_burn(&alice, &1, &proof),
        Err(Ok(err(Error::GeneratorsNotReady)))
    );
}

/// Soroban's per-transaction CPU instruction limit on public networks.
const TX_CPU_LIMIT: u64 = 100_000_000;
/// MSM terms per `verify_step` transaction that stays under the limit.
const STEP_POINTS: u32 = 40;

fn measured<T>(env: &Env, f: impl FnOnce() -> T) -> (T, u64) {
    env.cost_estimate().budget().reset_unlimited();
    let out = f();
    (out, env.cost_estimate().budget().cpu_instruction_cost())
}

#[test]
fn session_transfer_fits_per_transaction_budget() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 1_000_000);
    let (c, proof, amount_open, alice_after) = s.build_transfer(&alice, &bob, &balance, 400_000);

    let (id, begin_cpu) = measured(&s.env, || {
        s.token
            .begin_transfer(&alice, &bob, &c, &proof, &memo(&s.env))
    });
    std::println!("begin_transfer: {begin_cpu} CPU instructions");
    assert!(begin_cpu < TX_CPU_LIMIT, "begin_transfer used {begin_cpu}");
    // Nothing moves until the session settles.
    assert_eq!(s.token.available_balance(&alice), s.commit(&balance));

    let mut steps = 0;
    loop {
        let (left, cpu) = measured(&s.env, || s.token.verify_step(&id, &STEP_POINTS));
        std::println!("verify_step: {cpu} CPU instructions");
        assert!(cpu < TX_CPU_LIMIT, "verify_step used {cpu}");
        steps += 1;
        if left == 0 {
            break;
        }
        assert_eq!(
            s.token.try_finish(&id),
            Err(Ok(err(Error::VerificationIncomplete)))
        );
    }
    let ((), finish_cpu) = measured(&s.env, || s.token.finish(&id));
    std::println!("finish: {finish_cpu} CPU instructions, {steps} verify steps");
    assert!(finish_cpu < TX_CPU_LIMIT, "finish used {finish_cpu}");

    assert_eq!(s.token.available_balance(&alice), s.commit(&alice_after));
    assert_eq!(s.token.pending_balance(&bob), s.commit(&amount_open));
    assert_eq!(s.token.get_session(&id), None);
    assert_eq!(
        s.token.try_finish(&id),
        Err(Ok(err(Error::SessionNotFound)))
    );
}

#[test]
fn session_with_invalid_proof_never_settles() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 100);
    // Overspend: the proof covers a negative remaining balance.
    let (c, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 101);
    let id = s
        .token
        .begin_transfer(&alice, &bob, &c, &proof, &memo(&s.env));
    while s.token.verify_step(&id, &STEP_POINTS) > 0 {}
    assert_eq!(s.token.try_finish(&id), Err(Ok(err(Error::InvalidProof))));
    assert_eq!(s.token.available_balance(&alice), s.commit(&balance));
}

#[test]
fn session_goes_stale_when_balance_moves() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 1_000);
    let (c1, proof1, _, _) = s.build_transfer(&alice, &bob, &balance, 600);
    let (c2, proof2, _, _) = s.build_transfer(&alice, &bob, &balance, 700);

    // Two sessions spending the same balance: only the first to settle wins.
    let first = s
        .token
        .begin_transfer(&alice, &bob, &c1, &proof1, &memo(&s.env));
    let second = s
        .token
        .begin_transfer(&alice, &bob, &c2, &proof2, &memo(&s.env));
    while s.token.verify_step(&first, &1000) > 0 {}
    while s.token.verify_step(&second, &1000) > 0 {}
    s.token.finish(&first);
    assert_eq!(
        s.token.try_finish(&second),
        Err(Ok(err(Error::StaleSession)))
    );
}

#[test]
fn session_burn_and_compliance_recheck_at_settlement() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 500);

    let proof = s.build_burn(&alice, &balance, 125);
    let id = s.token.begin_burn(&alice, &125, &proof);
    while s.token.verify_step(&id, &STEP_POINTS) > 0 {}
    s.token.finish(&id);
    assert_eq!(s.token.total_supply(), 375);

    let after_burn = Opening {
        value: 375,
        blinding: balance.blinding.clone(),
    };
    let (c, proof, _, _) = s.build_transfer(&alice, &bob, &after_burn, 5);
    let id = s
        .token
        .begin_transfer(&alice, &bob, &c, &proof, &memo(&s.env));
    while s.token.verify_step(&id, &STEP_POINTS) > 0 {}
    // Recipient loses allowlist status mid-session.
    s.compliance.suspend(&bob);
    assert_eq!(
        s.token.try_finish(&id),
        Err(Ok(err(Error::RecipientNotCompliant)))
    );
}

/// Records the host cost of one-shot transfer verification. Run with
/// `--nocapture` to see the numbers.
#[test]
fn one_shot_transfer_cost() {
    let mut s = Setup::new();
    let alice = s.investor("US");
    let bob = s.investor("GB");
    let balance = s.funded(&alice, 1_000);
    let (c, proof, _, _) = s.build_transfer(&alice, &bob, &balance, 1);
    let ((), cpu) = measured(&s.env, || {
        s.token
            .confidential_transfer(&alice, &bob, &c, &proof, &memo(&s.env))
    });
    std::println!("confidential_transfer (one-shot): {cpu} CPU instructions");
}
