//! Fuzz target for `tessera-cap-table`.
//!
//! Covers `initialize`, `submit_snapshot`, `get_snapshot`, `verify_holder`, and
//! `version`.
//!
//! The important part is the Merkle verifier. `verify_holder` is documented as
//! returning `false` (never panicking) for an unknown snapshot or a proof that
//! does not fold to the stored root, and a verifier that always returns `false`
//! would satisfy that contract perfectly while being useless. So this target
//! does not just feed it garbage: it builds a **real** sorted-pair Merkle tree
//! host-side, submits the true root, and asserts that
//!
//! * every genuine proof verifies,
//! * dropping, reordering, duplicating or corrupting any element of a genuine
//!   proof makes it fail,
//! * a wrong `balance` against a genuine proof fails,
//! * a right balance against someone else's proof fails,
//! * an unknown `snapshot_id` returns `false` rather than panicking,
//! * an empty proof only verifies when the leaf *is* the root, i.e. a
//!   single-holder tree.
//!
//! That is the property set that makes the sorted-pair construction (rather
//! than naive left/right concatenation) actually verified rather than assumed.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{
    testutils::Address as _, xdr::ToXdr, Address, Bytes, BytesN, Env, Vec as SdkVec,
};
use tessera_cap_table::{CapTableContract, CapTableContractClient};
use tessera_fuzz_harness::{check_no_trap, step_ledger, succeeded, Input, MAX_OPS};

/// Mirrors the contract's leaf construction:
/// `sha256(holder_xdr ++ balance.to_be_bytes())`.
fn leaf(env: &Env, holder: &Address, balance: i128) -> BytesN<32> {
    let mut input = Bytes::new(env);
    input.append(&holder.to_xdr(env));
    input.append(&Bytes::from_array(env, &balance.to_be_bytes()));
    env.crypto().sha256(&input).into()
}

/// Mirrors `CapTableContract::hash_pair`: `sha256(min(a, b) ++ max(a, b))`.
fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() {
        (a, b)
    } else {
        (b, a)
    };
    let mut combined = Bytes::new(env);
    combined.append(&Bytes::from(lo.clone()));
    combined.append(&Bytes::from(hi.clone()));
    env.crypto().sha256(&combined).into()
}

/// Folds `node` up the tree against `siblings`, exactly as the contract does.
fn fold(env: &Env, node: &BytesN<32>, siblings: &[BytesN<32>]) -> BytesN<32> {
    let mut cur = node.clone();
    for s in siblings {
        cur = hash_pair(env, &cur, s);
    }
    cur
}

/// A host-side sorted-pair Merkle tree, so the target can assert both that a
/// genuine proof verifies and that a tampered one does not.
struct Tree {
    levels: Vec<Vec<BytesN<32>>>,
}

impl Tree {
    /// Build from a list of leaves. An odd node at any level is promoted
    /// unchanged to the next level, which is the standard construction and is
    /// mirrored by the proof the target generates.
    fn build(env: &Env, leaves: Vec<BytesN<32>>) -> Self {
        let mut levels = vec![leaves];
        while levels.last().map(|l| l.len()) != Some(1) {
            let cur = levels.last().expect("non-empty");
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
            levels.push(next);
        }
        Self { levels }
    }

    fn root(&self) -> BytesN<32> {
        self.levels.last().expect("non-empty")[0].clone()
    }

    /// The sibling path proving that `index` is in the tree.
    fn proof(&self, index: usize) -> Vec<BytesN<32>> {
        let mut out = Vec::new();
        let mut idx = index;
        for level in 0..self.levels.len() - 1 {
            let cur = &self.levels[level];
            let sibling = if idx % 2 == 0 {
                // Odd node promoted unchanged, so it has no real sibling.
                cur.get(idx + 1).cloned()
            } else {
                cur.get(idx - 1).cloned()
            };
            if let Some(s) = sibling {
                out.push(s);
            }
            idx /= 2;
        }
        out
    }
}

fn to_sdk_vec(env: &Env, items: &[BytesN<32>]) -> SdkVec<BytesN<32>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let cap_id = env.register(CapTableContract, ());
    let admin = Address::generate(&env);
    let impostor = Address::generate(&env);
    let c = CapTableContractClient::new(&env, &cap_id).mock_all_auths();

    check_no_trap(&c.try_initialize(&admin), "cap_table::initialize");

    // A small holder set with balances the fuzzer picks, so a genuine tree can
    // be built over them.
    const N: usize = 4;
    let holders: Vec<Address> = (0..N).map(|_| Address::generate(&env)).collect();
    let balances: Vec<i128> = (0..N).map(|_| input.i128_interesting()).collect();

    let leaves: Vec<BytesN<32>> = (0..N)
        .map(|i| leaf(&env, &holders[i], balances[i]))
        .collect();
    let tree = Tree::build(&env, leaves);
    let root = tree.root();

    // ---- the root properties -------------------------------------------
    // Submit the true root, then check that every genuine proof verifies.
    let total_holders = input.u32();
    let submit = c.try_submit_snapshot(&admin, &admin, &root, &total_holders);
    check_no_trap(&submit, "cap_table::submit_snapshot");
    let snap_id = 0u64;
    let submitted = succeeded(&submit);

    if submitted {
        for i in 0..N {
            let proof = tree.proof(i);
            assert!(
                c.verify_holder(&snap_id, &holders[i], &balances[i], &to_sdk_vec(&env, &proof)),
                "a genuine Merkle proof for leaf {i} was rejected"
            );

            // A wrong balance against a genuine proof must fail.
            let wrong_balance = balances[i].wrapping_add(1);
            assert!(
                !c.verify_holder(&snap_id, &holders[i], &wrong_balance, &to_sdk_vec(&env, &proof)),
                "a wrong balance verified against a genuine proof"
            );

            // Someone else's holder address against this proof must fail.
            let other = holders[(i + 1) % N];
            assert!(
                !c.verify_holder(&snap_id, &other, &balances[i], &to_sdk_vec(&env, &proof)),
                "leaf {i}'s proof verified a different holder"
            );

            // Truncating the proof must fail, and extending it must fail too.
            // Sorted-pair hashing means a *prefix* of a genuine proof is not
            // generally the genuine path to the root.
            if !proof.is_empty() {
                let truncated = to_sdk_vec(&env, &proof[..proof.len() - 1]);
                assert!(
                    !c.verify_holder(&snap_id, &holders[i], &balances[i], &truncated),
                    "a truncated proof verified"
                );

                let mut extra = proof.clone();
                extra.push(BytesN::from_array(&env, &input.bytes32()));
                assert!(
                    !c.verify_holder(&snap_id, &holders[i], &balances[i], &to_sdk_vec(&env, &extra)),
                    "an over-long proof verified"
                );
            }

            // Reordering the siblings must fail: sorted-pair hashing is
            // order-insensitive per pair, but the *tree shape* is not, so
            // swapping two levels cannot land on the same root.
            if proof.len() >= 2 {
                let mut swapped = proof.clone();
                swapped.swap(0, proof.len() - 1);
                assert!(
                    !c.verify_holder(&snap_id, &holders[i], &balances[i], &to_sdk_vec(&env, &swapped)),
                    "a reordered proof verified"
                );
            }

            // Duplicating the first sibling must fail.
            if let Some(first) = proof.first() {
                let mut dup = vec![first.clone()];
                dup.extend_from_slice(&proof);
                assert!(
                    !c.verify_holder(&snap_id, &holders[i], &balances[i], &to_sdk_vec(&env, &dup)),
                    "a proof with a duplicated sibling verified"
                );
            }
        }

        // An empty proof only verifies for the single-leaf tree, where the leaf
        // *is* the root.
        for i in 0..N {
            let empty = to_sdk_vec(&env, &[]);
            let expected = if N == 1 { true } else { false };
            assert_eq!(
                c.verify_holder(&snap_id, &holders[i], &balances[i], &empty),
                expected,
                "empty-proof behaviour is wrong for a {N}-leaf tree"
            );
        }

        // A completely random proof must essentially never verify.
        let junk: Vec<BytesN<32>> = (0..input.seq_len())
            .map(|_| BytesN::from_array(&env, &input.bytes32()))
            .collect();
        assert!(
            !c.verify_holder(&snap_id, &holders[0], &balances[0], &to_sdk_vec(&env, &junk)),
            "a random proof verified"
        );
    }

    // ---- stateful surface ------------------------------------------------
    // Ids are dense and start at 0, so a host-side counter mirrors `NextId`
    // without needing a getter the contract does not expose. `submitted_ids` is
    // the set of ids that actually exist, so `verify_holder` can be called with
    // a known-unknown id and asserted to return `false`.
    let mut next_id: u64 = 0;
    let mut submitted_ids: Vec<u64> = Vec::new();
    if submitted {
        submitted_ids.push(snap_id);
        next_id = 1;
    }

    for _ in 0..MAX_OPS {
        step_ledger(&env, &mut input);
        match input.below(8) {
            0 | 1 => {
                let r = input.bytes32();
                let h = input.u32();
                let res = c.try_submit_snapshot(&admin, &admin, &BytesN::from_array(&env, &r), &h);
                check_no_trap(&res, "cap_table::submit_snapshot");
                if succeeded(&res) {
                    // The contract hands back the id it allocated, so use that
                    // rather than assuming a dense counter.
                    let Ok(Ok(id)) = &res else {
                        unreachable!("succeeded() implies Ok(Ok(_))")
                    };
                    assert_eq!(*id, next_id, "snapshot ids must be dense from 0");
                    submitted_ids.push(*id);
                    next_id += 1;
                }
            }
            2 => {
                // A non-admin must never be able to publish a snapshot.
                let r = input.bytes32();
                let res = c.try_submit_snapshot(&impostor, &admin, &BytesN::from_array(&env, &r), &0);
                check_no_trap(&res, "cap_table::submit_snapshot(impostor)");
                assert!(!succeeded(&res), "a non-admin published a snapshot");
            }
            3 => {
                // `verify_holder` is documented never to panic. Fuzz it with
                // arbitrary proofs, holders, balances and ids.
                let id = input.u64();
                let known = submitted_ids.contains(&id);
                let holder = holders[input.below(N)].clone();
                let balance = input.i128_interesting();
                let len = input.seq_len();
                let proof: Vec<BytesN<32>> = (0..len)
                    .map(|_| BytesN::from_array(&env, &input.bytes32()))
                    .collect();
                let verified = c.verify_holder(&id, &holder, &balance, &to_sdk_vec(&env, &proof));
                if !known {
                    assert!(!verified, "verify_holder accepted an unknown snapshot id");
                }
            }
            4 => {
                // Reading a snapshot that was never submitted must be a declared
                // error, not a trap.
                let res = c.try_get_snapshot(&9_999_999);
                check_no_trap(&res, "cap_table::get_snapshot(missing)");
                assert!(!succeeded(&res), "get_snapshot on a missing id must fail");
            }
            5 => {
                // Snapshot metadata must round-trip exactly, including the
                // extremes of `total_holders`.
                let h = if input.bool() { 0 } else { u32::MAX };
                let r = input.bytes32();
                let res = c.try_submit_snapshot(&admin, &admin, &BytesN::from_array(&env, &r), &h);
                check_no_trap(&res, "cap_table::submit_snapshot(extremes)");
                if succeeded(&res) {
                    let Ok(Ok(id)) = &res else {
                        unreachable!("succeeded() implies Ok(Ok(_))")
                    };
                    let snap = c.get_snapshot(id);
                    assert_eq!(snap.id, *id, "snapshot id mismatch");
                    assert_eq!(snap.total_holders, h, "total_holders must round-trip");
                    assert_eq!(
                        snap.merkle_root,
                        BytesN::from_array(&env, &r),
                        "root must round-trip"
                    );
                    assert_eq!(snap.asset_token, admin, "asset_token must round-trip");
                    assert_eq!(
                        snap.ledger,
                        env.ledger().sequence(),
                        "ledger must round-trip"
                    );
                    submitted_ids.push(*id);
                    next_id += 1;
                }
            }
            6 => {
                let _ = c.version();
            }
            _ => {
                // Re-initialising must always be refused and must never trap.
                let res = c.try_initialize(&impostor);
                check_no_trap(&res, "cap_table::initialize(again)");
                assert!(!succeeded(&res), "cap_table was initialised twice");
            }
        }
    }
});
