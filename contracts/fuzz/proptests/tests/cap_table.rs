//! Property: a cap-table Merkle root and proof are consistent.
//!
//! The property is the one a shareholder registry actually needs: **a genuine
//! proof verifies, and a tampered one does not.** The old single `get_holder`
//! entry point forced the contract to scan the whole holder set on every read,
//! so the only implementation left was to drop the commitment and the proof
//! together, or to accept whatever the caller supplied. The Merkle structure
//! makes that impossible: changing a leaf changes the root, and a proof for a
//! different leaf does not hash to it.
//!
//! The odd-node handling is the part most likely to be subtly wrong, so it is
//! exercised across tree sizes from 1 to 9, which covers the promote-an-odd-last
//! -node case at every level.

use proptest::prelude::*;
use soroban_sdk::{
    testutils::Address as _, xdr::ToXdr, Address, Bytes, BytesN, Env,
    Vec as SdkVec,
};
use tessera_cap_table::{CapTableContract, CapTableContractClient, Error as CapError};

type Try<T, E> =
    Result<Result<T, soroban_sdk::ConversionError>, Result<E, soroban_sdk::InvokeError>>;

fn is_ok<T, E>(r: &Try<T, E>) -> bool {
    matches!(r, Ok(Ok(_)))
}

fn is_err<T, E>(r: &Try<T, E>, e: E) -> bool
where
    E: PartialEq + core::fmt::Debug,
{
    matches!(r, Err(Ok(actual)) if *actual == e)
}

/// `sha256(min(a, b) ++ max(a, b))`, mirroring `CapTableContract::hash_pair`.
/// The sort is what makes a proof order-independent: a prover can present the
/// pair in either order and still verify.
fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() { (a, b) } else { (b, a) };
    let mut c = Bytes::new(env);
    c.append(&Bytes::from(lo.clone()));
    c.append(&Bytes::from(hi.clone()));
    env.crypto().sha256(&c).into()
}

fn leaf(env: &Env, holder: &Address, balance: i128) -> BytesN<32> {
    let mut b = Bytes::new(env);
    b.append(&holder.to_xdr(env));
    b.append(&Bytes::from_array(env, &balance.to_be_bytes()));
    env.crypto().sha256(&b).into()
}

fn fold(env: &Env, mut node: BytesN<32>, proof: &[BytesN<32>]) -> BytesN<32> {
    for s in proof {
        node = hash_pair(env, &node, s);
    }
    node
}

/// The root, promoting an odd trailing node unchanged.
fn root(env: &Env, leaves: &[BytesN<32>]) -> BytesN<32> {
    assert!(!leaves.is_empty());
    let mut cur: Vec<BytesN<32>> = leaves.to_vec();
    while cur.len() > 1 {
        let mut next = Vec::new();
        let mut i = 0;
        while i < cur.len() {
            if i + 1 < cur.len() {
                next.push(hash_pair(env, &cur[i], &cur[i + 1]));
            } else {
                next.push(cur[i].clone());
            }
            i += 2;
        }
        cur = next;
    }
    cur[0].clone()
}

/// The sibling path for `index`, promoting an odd trailing node.
fn proof(env: &Env, leaves: &[BytesN<32>], index: usize) -> Vec<BytesN<32>> {
    let mut out = Vec::new();
    let mut cur: Vec<BytesN<32>> = leaves.to_vec();
    let mut idx = index;
    while cur.len() > 1 {
        let sib = if idx % 2 == 0 { cur.get(idx + 1) } else { cur.get(idx - 1) };
        if let Some(s) = sib {
            out.push(s.clone());
        }
        let mut next = Vec::new();
        let mut i = 0;
        while i < cur.len() {
            if i + 1 < cur.len() {
                next.push(hash_pair(env, &cur[i], &cur[i + 1]));
            } else {
                next.push(cur[i].clone());
            }
            i += 2;
        }
        cur = next;
        idx /= 2;
    }
    out
}

fn to_proof(env: &Env, items: &[BytesN<32>]) -> SdkVec<BytesN<32>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

struct Fixture {
    env: Env,
    cap: CapTableContractClient<'static>,
    admin: Address,
    holders: Vec<Address>,
}

fn setup(n: usize) -> Fixture {
    let env = Env::default();
    let id = env.register(CapTableContract, ());
    let admin = Address::generate(&env);
    let holders: Vec<Address> = (0..n).map(|_| Address::generate(&env)).collect();
    let cap = CapTableContractClient::new(&env, &id).mock_all_auths();
    let r = cap.try_initialize(&admin);
    assert!(is_ok(&r), "setup: initialize failed: {r:?}");
    Fixture { env, cap, admin, holders }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// For a published snapshot, every holder's genuine proof verifies.
    #[test]
    fn genuine_proofs_verify(size in 1usize..=9) {
        let f = setup(size);
        let balances: Vec<i128> = (0..size as i128).map(|i| (i + 1) * 1_000).collect();
        let leaves: Vec<BytesN<32>> = f
            .holders
            .iter()
            .zip(balances.iter())
            .map(|(h, b)| leaf(&f.env, h, *b))
            .collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed for size {size}: {r:?}");

        for k in 0..size {
            let p = proof(&f.env, &leaves, k);
            prop_assert!(
                f.cap.verify_holder(&0, &f.holders[k], &balances[k], &to_proof(&f.env, &p)),
                "a genuine proof for holder {k} of {size} failed to verify"
            );
        }
    }

    /// A proof for the wrong balance fails. This is the property that makes the
    /// root a *commitment*: without it, the snapshot is just a list.
    #[test]
    fn wrong_balance_fails(size in 2usize..=9, index in 0usize..9, delta in 1i64..1000) {
        let size = size.min(9);
        let index = index.min(size - 1);
        let f = setup(size);
        let balances: Vec<i128> = (0..size as i128).map(|i| (i + 1) * 1_000).collect();
        let leaves: Vec<BytesN<32>> = f
            .holders
            .iter()
            .zip(balances.iter())
            .map(|(h, b)| leaf(&f.env, h, *b))
            .collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed: {r:?}");

        let p = proof(&f.env, &leaves, index);
        let wrong = balances[index] + delta;
        prop_assert!(
            !f.cap.verify_holder(&0, &f.holders[index], &wrong, &to_proof(&f.env, &p)),
            "a proof verified for a balance {delta} higher than the real one"
        );
    }

    /// A proof for the wrong holder fails.
    #[test]
    fn wrong_holder_fails(size in 2usize..=9) {
        let f = setup(size);
        let balances: Vec<i128> = (0..size as i128).map(|i| (i + 1) * 1_000).collect();
        let leaves: Vec<BytesN<32>> = f
            .holders
            .iter()
            .zip(balances.iter())
            .map(|(h, b)| leaf(&f.env, h, *b))
            .collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed: {r:?}");

        let stranger = Address::generate(&f.env);
        for k in 0..size {
            let p = proof(&f.env, &leaves, k);
            prop_assert!(
                !f.cap.verify_holder(&0, &stranger, &balances[k], &to_proof(&f.env, &p)),
                "a proof verified for an address that is not in the cap table"
            );
        }
    }

    /// A tampered sibling breaks verification, so a prover cannot inflate a
    /// leaf while keeping the same proof.
    #[test]
    fn tampered_sibling_fails(size in 2usize..=9) {
        let f = setup(size);
        let balances: Vec<i128> = (0..size as i128).map(|i| (i + 1) * 1_000).collect();
        let leaves: Vec<BytesN<32>> = f
            .holders
            .iter()
            .zip(balances.iter())
            .map(|(h, b)| leaf(&f.env, h, *b))
            .collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed: {r:?}");

        for k in 0..size {
            let mut p = proof(&f.env, &leaves, k);
            if p.is_empty() {
                continue;
            }
            p[0] = BytesN::from_array(&f.env, &[0xde, 0xad, 0xbe, 0xef]);
            prop_assert!(
                !f.cap.verify_holder(&0, &f.holders[k], &balances[k], &to_proof(&f.env, &p)),
                "a proof with a tampered first sibling still verified (holder {k})"
            );
        }
    }

    /// An empty or truncated proof fails rather than being treated as a
    /// single-leaf tree.
    #[test]
    fn empty_proof_fails(size in 2usize..=9) {
        let f = setup(size);
        let balances: Vec<i128> = (0..size as i128).map(|i| (i + 1) * 1_000).collect();
        let leaves: Vec<BytesN<32>> = f
            .holders
            .iter()
            .zip(balances.iter())
            .map(|(h, b)| leaf(&f.env, h, *b))
            .collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed: {r:?}");

        for k in 0..size {
            prop_assert!(
                !f.cap.verify_holder(&0, &f.holders[k], &balances[k], &SdkVec::new(&f.env)),
                "an empty proof verified for holder {k} of {size}"
            );
        }
    }

    /// Only the admin may publish a snapshot, `initialize` cannot be replayed,
    /// and an unknown snapshot id does not verify anything.
    #[test]
    fn snapshot_administration(size in 1usize..=5, unknown_id in 1u64..100) {
        let f = setup(size);
        let leaves: Vec<BytesN<32>> =
            f.holders.iter().enumerate().map(|(i, h)| leaf(&f.env, h, (i + 1) as i128)).collect();
        let r = f.cap.try_submit_snapshot(&f.admin, &f.holders[0], &root(&f.env, &leaves), &size as u32);
        prop_assert!(is_ok(&r), "submit_snapshot failed: {r:?}");

        let stranger = Address::generate(&f.env);
        let rogue = f.cap.try_submit_snapshot(&stranger, &f.holders[0], &BytesN::from_array(&f.env, &[7u8; 32]), &1);
        prop_assert!(!is_ok(&rogue), "a non-admin published a snapshot");
        prop_assert!(is_err(&rogue, CapError::Unauthorized), "expected Unauthorized, got {rogue:?}");

        let replay = f.cap.try_initialize(&f.admin);
        prop_assert!(!is_ok(&replay), "initialize was accepted twice");
        prop_assert!(is_err(&replay, CapError::AlreadyInitialized), "expected AlreadyInitialized, got {replay:?}");

        let p = proof(&f.env, &leaves, 0);
        prop_assert!(
            !f.cap.verify_holder(&unknown_id, &f.holders[0], &1, &to_proof(&f.env, &p)),
            "a proof verified against an unknown snapshot id {unknown_id}"
        );
    }
}
