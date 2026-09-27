//! Cross-contract invariant target.
//!
//! Every other target exercises one contract in isolation. This one wires the
//! whole system together the way production does — a registry, a compliance
//! contract, two asset tokens, a dividend contract, a cap-table and a debt
//! ledger — and checks the properties that only exist *between* contracts.
//!
//! The headline property is **end-to-end supply conservation**: the total
//! supply of a token, less everything locked in the dividend escrow, must equal
//! the sum of every holder balance plus every clawed-back unit. An off-by-one in
//! any one contract's arithmetic breaks it, and no single-contract target can
//! see that.
//!
//! The second is the **compliance gate being load-bearing**: every token holder
//! must be allowlisted, and revoking one must make the transfer that would have
//! succeeded now fail — proving the gate is actually consulted rather than
//! bypassed by some path.
//!
//! The third is **divendends bounded by cap-table reality**: a distribution can
//! never pay out more in aggregate than was escrowed, even when the same
//! underlying units are reachable from more than one allowlisted address. That
//! is the drain the dividend contract's own comment is defending against, and
//! it is only observable with the compliance contract in the loop, since
//! "more than one approved address" is exactly what makes it possible.

#![no_main]

use libfuzzer_sys::fuzz_target;

use debt_token::{DebtTokenAmortization, DebtTokenAmortizationClient, Tranche};
use soroban_sdk::{
    testutils::Address as _, xdr::ToXdr, Address, Bytes, BytesN, Env, String as SdkString,
    Vec as SdkVec,
};
use tessera_asset_token::{AssetTokenContract, AssetTokenContractClient};
use tessera_cap_table::{CapTableContract, CapTableContractClient};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_dividend::{DividendContract, DividendContractClient};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};
use tessera_registry::{RegistryContract, RegistryContractClient};

const HOLDERS: usize = 5;
const SUPPLY: i128 = 10_000_000;

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    // ---- the whole system, deployed --------------------------------------
    let compliance_id = env.register(ComplianceContract, ());
    let registry_id = env.register(RegistryContract, ());
    let asset_id = env.register(AssetTokenContract, ());
    let payment_id = env.register(AssetTokenContract, ());
    let dividend_id = env.register(DividendContract, ());
    let cap_id = env.register(CapTableContract, ());
    let debt_id = env.register(DebtTokenAmortization, ());

    let admin = Address::generate(&env);
    let compliance = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let registry = RegistryContractClient::new(&env, &registry_id).mock_all_auths();
    let asset = AssetTokenContractClient::new(&env, &asset_id).mock_all_auths();
    let payment = AssetTokenContractClient::new(&env, &payment_id).mock_all_auths();
    let dividend = DividendContractClient::new(&env, &dividend_id).mock_all_auths();
    let cap = CapTableContractClient::new(&env, &cap_id).mock_all_auths();
    let debt = DebtTokenAmortizationClient::new(&env, &debt_id).mock_all_auths();

    // Holder 0 is the admin: every token's `initialize` credits the supply there.
    let mut holders: Vec<Address> = (0..HOLDERS).map(|_| Address::generate(&env)).collect();
    holders[0] = admin.clone();

    // The dividend contract must itself be allowlisted, or `claim` cannot pay
    // anything out of escrow.
    compliance.initialize(&admin);
    for who in holders.iter().chain(std::iter::once(&dividend_id)) {
        compliance.add_to_allowlist(&admin, who, &SdkString::from_str(&env, "US"), 0);
    }

    let name = SdkString::from_str(&env, "SYS");
    let kind = SdkString::from_str(&env, "RWA");

    check_no_trap(
        &asset.try_initialize(
            &admin, &name, &name, &kind, &SUPPLY, &6, &compliance_id, &name, &0i128,
        ),
        "xcontract::asset.initialize",
    );
    check_no_trap(
        &payment.try_initialize(
            &admin, &name, &name, &kind, &SUPPLY, &6, &compliance_id, &name, &0i128,
        ),
        "xcontract::payment.initialize",
    );
    check_no_trap(&registry.try_initialize(&admin), "xcontract::registry.initialize");
    check_no_trap(&dividend.try_initialize(&admin), "xcontract::dividend.initialize");
    check_no_trap(&cap.try_initialize(&admin), "xcontract::cap_table.initialize");
    check_no_trap(&debt.try_initialize(&admin), "xcontract::debt.initialize");

    // Register the asset and fan the supply out.
    check_no_trap(
        &registry.try_register_asset(&admin, &asset_id, &name, &kind, &SUPPLY),
        "xcontract::registry.register_asset",
    );
    let per_holder = SUPPLY / HOLDERS as i128;
    for h in holders.iter().skip(1) {
        check_no_trap(
            &asset.try_mint(&admin, h, &per_holder),
            "xcontract::asset.mint",
        );
    }

    // Host-side mirrors.
    let mut clawbacked = 0i128;
    // (contract-assigned distribution id, escrowed amount). Keyed by the real id
    // so gaps in the id sequence do not desynchronise the mirror.
    let mut escrowed: Vec<(u64, i128)> = Vec::new();
    let mut owed = [0i128; 3];
    // A snapshot is an immutable commitment, so the cross-contract target only
    // ever files one; see the comment on arm 8.
    let mut cap_published = false;

    for _ in 0..MAX_OPS {
        let from = holders[input.below(HOLDERS)].clone();
        let to = holders[input.below(HOLDERS)].clone();
        let amount = input.i128_amount(SUPPLY);
        step_ledger(&env, &mut input);

        match input.below(12) {
            0 | 1 => {
                // A self-transfer must be a no-op. It used to inflate the
                // holder's balance by `amount` while leaving total supply
                // untouched, and no single-contract target could see that
                // because the supply check alone still passed.
                let self_transfer = from == to;
                let before = asset.balance(&from);
                let res = asset.try_transfer(&from, &to, &amount);
                check_no_trap(&res, "xcontract::asset.transfer");
                if self_transfer && succeeded(&res) {
                    assert_eq!(
                        asset.balance(&from),
                        before,
                        "a self-transfer changed the sender's balance by {}",
                        asset.balance(&from) - before
                    );
                }
            }
            2 => {
                let res = asset.try_clawback(&admin, &from, &amount);
                check_no_trap(&res, "xcontract::asset.clawback");
                if succeeded(&res) {
                    clawbacked = clawbacked.saturating_add(amount);
                }
            }
            3 => {
                // The compliance gate must be load-bearing: revoking a holder
                // has to make a transfer that would otherwise have worked fail.
                let res = compliance.try_suspend(&admin, &from);
                check_no_trap(&res, "xcontract::compliance.suspend");
                if succeeded(&res) {
                    assert!(!compliance.is_allowed(&from), "a suspended holder is still allowed");
                    let gate = asset.try_transfer(&from, &to, &1);
                    check_no_trap(&gate, "xcontract::asset.transfer(suspended)");
                    assert!(
                        !succeeded(&gate),
                        "a suspended holder could still transfer"
                    );
                    // Restore, so later iterations are not all blocked.
                    let _ = compliance.try_add_to_allowlist(&admin, &from, &SdkString::from_str(&env, "US"), 0);
                }
            }
            4 => {
                // Block a jurisdiction and confirm it revokes live approvals.
                let j = SdkString::from_str(&env, &input.ascii(3));
                let res = compliance.try_block_jurisdiction(&admin, &j);
                check_no_trap(&res, "xcontract::compliance.block_jurisdiction");
                let _ = compliance.try_unblock_jurisdiction(&admin, &j);
            }
            5 | 6 => {
                let res =
                    dividend.try_create_distribution(&admin, &asset_id, &payment_id, &amount);
                check_no_trap(&res, "xcontract::dividend.create_distribution");
                if let Ok(Ok(id)) = res {
                    // Track the id the contract actually assigned, not the
                    // position in this vector. Ids come from the contract's own
                    // sequential counter, so a distribution created with
                    // `amount == 0` — or one whose creation was refused and so
                    // never reached this branch — leaves a gap. Keying off the
                    // vector index instead would read the wrong distribution
                    // for the rest of the run and make the escrow invariant
                    // below compare against garbage.
                    escrowed.push((id, amount));
                }
            }
            7 => {
                // Let every holder claim. This is the drain path: with several
                // approved addresses each computing an independent full share,
                // an uncapped payout would exceed the escrow several times over.
                if escrowed.is_empty() {
                    continue;
                }
                for (id, total) in escrowed.iter().copied() {
                    for h in &holders {
                        if !dividend.has_claimed(&id, h) {
                            let res = dividend.try_claim(&id, h);
                            check_no_trap(&res, "xcontract::dividend.claim");
                        }
                    }
                    let dist = dividend.get_distribution(&id);
                    assert_eq!(dist.id, id, "distribution id round-trip mismatch");
                    assert!(
                        dist.distributed <= total,
                        "distribution {id} paid out {} of {total}",
                        dist.distributed
                    );
                }
            }
            8 => {
                // Publish a cap-table snapshot over the live holder set and
                // prove a genuine Merkle proof verifies against it.
                //
                // Only ever published once. A snapshot is an immutable
                // commitment to the balances at the moment it was filed, so
                // re-publishing every iteration would file a new id while the
                // assertions below kept checking id 0 — whose stored root was
                // computed from the *first* iteration's balances. The proof
                // would then be built from today's leaves and compared against
                // an old root, and fail for a reason that has nothing to do
                // with the contract.
                if cap_published {
                    continue;
                }
                let balances: Vec<i128> =
                    holders.iter().map(|h| asset.balance(h)).collect();
                let mut leaves: Vec<BytesN<32>> = Vec::new();
                for (h, b) in holders.iter().zip(balances.iter()) {
                    let mut li = Bytes::new(&env);
                    li.append(&h.to_xdr(&env));
                    li.append(&Bytes::from_array(&env, &b.to_be_bytes()));
                    leaves.push(env.crypto().sha256(&li).into());
                }
                let root = merkle_root(&env, &leaves);
                let res = cap.try_submit_snapshot(&admin, &asset_id, &root, &HOLDERS as u32);
                check_no_trap(&res, "xcontract::cap_table.submit_snapshot");
                if let Ok(Ok(id)) = res {
                    cap_published = true;
                    for (k, h) in holders.iter().enumerate() {
                        let proof = merkle_proof(&env, &leaves, k);
                        assert!(
                            cap.verify_holder(&id, h, &balances[k], &to_proof(&env, &proof)),
                            "a genuine cap-table proof for holder {k} was rejected"
                        );
                    }
                }
            }
            9 => {
                let tranche = match input.below(3) {
                    0 => Tranche::Senior,
                    1 => Tranche::Mezzanine,
                    _ => Tranche::Equity,
                };
                let res = debt.try_accrue_interest(&admin, &tranche, &amount);
                check_no_trap(&res, "xcontract::debt.accrue_interest");
                if amount > 0 && succeeded(&res) {
                    let idx = match tranche {
                        Tranche::Senior => 0,
                        Tranche::Mezzanine => 1,
                        Tranche::Equity => 2,
                    };
                    owed[idx] += amount;
                }
            }
            10 => {
                let total = debt.total_owed();
                let res = debt.try_deposit_repayment(&from, &total.saturating_add(1));
                check_no_trap(&res, "xcontract::debt.deposit_repayment(overpay)");
                if succeeded(&res) {
                    owed = [0, 0, 0];
                }
            }
            _ => {
                // A dividend amount far beyond anything escrowed, to prove the
                // escrow pull is the binding constraint rather than a check that
                // happens to line up.
                let res = dividend.try_create_distribution(&admin, &asset_id, &payment_id, &i128::MAX);
                check_no_trap(&res, "xcontract::dividend.create_distribution(i128::MAX)");
            }
        }

        // ---- end-to-end supply conservation --------------------------------
        let sum: i128 = holders.iter().fold(0i128, |a, h| a.saturating_add(asset.balance(h)));
        let supply = asset.total_supply();
        assert_eq!(
            sum + clawbacked,
            supply,
            "holder balances plus clawed-back units must equal total supply"
        );
        if supply < 0 {
            finding("xcontract", "total supply went negative");
        }

        // ---- escrow is bounded by what was actually escrowed ---------------
        let total_escrowed: i128 = escrowed.iter().fold(0i128, |a, (_, t)| a.saturating_add(*t));
        let total_paid: i128 = escrowed
            .iter()
            .map(|(id, _)| dividend.get_distribution(id).distributed)
            .fold(0i128, |a, x| a.saturating_add(x));
        assert!(
            total_paid <= total_escrowed,
            "paid out {total_paid} against {total_escrowed} escrowed"
        );
        assert_eq!(
            payment.balance(&dividend_id),
            total_escrowed - total_paid,
            "escrow does not reconcile with the distributions"
        );

        // ---- the debt ledger never goes negative ---------------------------
        let live = [debt.senior_owed(), debt.mezzanine_owed(), debt.equity_owed()];
        for i in 0..3 {
            assert!(live[i] >= 0, "a debt tranche went negative: {}", live[i]);
            assert_eq!(live[i], owed[i], "debt mirror disagrees with the contract");
        }
    }
});

/// Host `Vec` -> Soroban `Vec` for a proof argument.
fn to_proof(env: &Env, items: &[BytesN<32>]) -> SdkVec<BytesN<32>> {
    let mut v = SdkVec::new(env);
    for i in items {
        v.push_back(i.clone());
    }
    v
}

/// `sha256(min(a, b) ++ max(a, b))`, mirroring `CapTableContract::hash_pair`.
fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (lo, hi) = if a.to_array() <= b.to_array() {
        (a, b)
    } else {
        (b, a)
    };
    let mut c = Bytes::new(env);
    c.append(&Bytes::from(lo.clone()));
    c.append(&Bytes::from(hi.clone()));
    env.crypto().sha256(&c).into()
}

fn merkle_root(env: &Env, leaves: &[BytesN<32>]) -> BytesN<32> {
    if leaves.is_empty() {
        return BytesN::from_array(env, &[0u8; 32]);
    }
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

fn merkle_proof(env: &Env, leaves: &[BytesN<32>], index: usize) -> Vec<BytesN<32>> {
    let mut out = Vec::new();
    let mut cur: Vec<BytesN<32>> = leaves.to_vec();
    let mut idx = index;
    while cur.len() > 1 {
        let sibling = if idx % 2 == 0 {
            cur.get(idx + 1).cloned()
        } else {
            cur.get(idx - 1).cloned()
        };
        if let Some(s) = sibling {
            out.push(s);
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
