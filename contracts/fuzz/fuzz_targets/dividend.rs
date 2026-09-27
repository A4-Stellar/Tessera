//! Stateful fuzz target for `tessera-dividend`.
//!
//! Deploys the full dependency graph the contract actually talks to — a
//! compliance contract, an asset token (which `claimable` reads `balance` and
//! `total_supply` from) and a second asset token acting as the payment token —
//! so that `create_distribution`'s escrow pull and `claim`'s payout are real
//! cross-contract transfers rather than stubs.
//!
//! The central property is **escrow conservation**. The payment-token balance
//! held by the dividend contract must always equal
//! `sum(total_amount) - sum(distributed)`, and the admin's must always equal
//! `payment_supply - sum(total_amount)`. Those two together are what bound the
//! aggregate payout, and they are what catches the attack the contract's own
//! comment is defending against: the same escrowed tokens paid out more than
//! once because each hop recomputes an independent full share from a live
//! balance. A `claimable` that ignores the remaining escrow, or a `distributed`
//! that does not advance in lockstep with the transfer, breaks the first
//! identity immediately.
//!
//! Also asserted:
//!
//! * `claimable` is never negative and never exceeds the remaining escrow;
//! * a successful `claim` advances `distributed` by exactly the `claimable`
//!   value observed *before* the call, and sets `has_claimed`;
//! * a second claim by the same holder pays nothing and moves nothing;
//! * a *rejected* claim moves neither `distributed` nor `has_claimed`;
//! * `distributed <= total_amount` and `completed == (distributed >= total_amount)`;
//! * `get_distributions_for_asset` returns exactly the matching set, and reads
//!   against a nonexistent id are total rather than trapping.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String as SdkString};
use tessera_asset_token::{AssetTokenContract, AssetTokenContractClient};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_dividend::{DividendContract, DividendContractClient};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};

const HOLDERS: usize = 4;
const ASSET_SUPPLY: i128 = 1_000_000;

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    // ---- deployment graph -------------------------------------------------
    let compliance_id = env.register(ComplianceContract, ());
    let asset_id = env.register(AssetTokenContract, ());
    let payment_id = env.register(AssetTokenContract, ());
    let dividend_id = env.register(DividendContract, ());

    let admin = Address::generate(&env);
    let compliance = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let asset = AssetTokenContractClient::new(&env, &asset_id).mock_all_auths();
    let payment = AssetTokenContractClient::new(&env, &payment_id).mock_all_auths();
    let d = DividendContractClient::new(&env, &dividend_id).mock_all_auths();

    // Holder 0 is the admin, because `create_distribution` pulls the escrow
    // from the admin and each token's `initialize` credits the supply there.
    let mut holders: Vec<Address> = (0..HOLDERS).map(|_| Address::generate(&env)).collect();
    holders[0] = admin.clone();

    // The dividend contract has to be compliant too: `claim` transfers the
    // payout *out* of it and the asset token checks both ends of a transfer.
    compliance.initialize(&admin);
    for who in holders.iter().chain(std::iter::once(&dividend_id)) {
        compliance.add_to_allowlist(&admin, who, &SdkString::from_str(&env, "US"), 0);
    }

    let name = SdkString::from_str(&env, "TST");
    let kind = SdkString::from_str(&env, "RWA");

    // The asset token is what `claimable` divides by, so give it a real supply
    // and fan it out: a zero supply would make every share zero and the whole
    // target would degenerate into the "nothing to claim" path.
    check_no_trap(
        &asset.try_initialize(
            &admin,
            &name,
            &name,
            &kind,
            &ASSET_SUPPLY,
            &6,
            &compliance_id,
            &name,
            &0i128,
        ),
        "dividend::asset_token.initialize",
    );

    let payment_supply = input.i128_amount(1_000_000_000);
    if payment_supply <= 0 {
        return;
    }
    check_no_trap(
        &payment.try_initialize(
            &admin,
            &name,
            &name,
            &kind,
            &payment_supply,
            &6,
            &compliance_id,
            &name,
            &0i128,
        ),
        "dividend::payment_token.initialize",
    );

    let per_holder = ASSET_SUPPLY / HOLDERS as i128;
    for h in holders.iter().skip(1) {
        check_no_trap(
            &asset.try_mint(&admin, h, &per_holder),
            "dividend::asset_token.mint",
        );
    }

    check_no_trap(&d.try_initialize(&admin), "dividend::initialize");

    // Host-side shadow state: the `total_amount` of each distribution, in id
    // order. `distributed` is deliberately *not* shadowed — it is read back from
    // the contract, so the escrow identity below is a genuine cross-check of
    // contract state against host state rather than a tautology.
    let mut escrowed: Vec<i128> = Vec::new();

    for _ in 0..MAX_OPS {
        let holder = holders[input.below(HOLDERS)].clone();
        let amount = input.i128_amount(payment_supply);
        step_ledger(&env, &mut input);

        // Amount still escrowed but not yet paid out, per the contract.
        let paid_out = |d: &DividendContractClient, i: usize| d.get_distribution(&(i as u64)).distributed;

        match input.below(10) {
            0 | 1 | 2 => {
                let res = d.try_create_distribution(&admin, &asset_id, &payment_id, &amount);
                check_no_trap(&res, "dividend::create_distribution");
                if amount <= 0 {
                    assert!(
                        !succeeded(&res),
                        "create_distribution accepted a non-positive amount"
                    );
                } else if succeeded(&res) {
                    // ids are dense and start at 0.
                    let id = escrowed.len() as u64;
                    escrowed.push(amount);
                    let dist = d.get_distribution(&id);
                    assert_eq!(dist.id, id, "distribution id must be dense");
                    assert_eq!(dist.total_amount, amount, "total_amount mismatch");
                    assert_eq!(dist.distributed, 0, "a new distribution starts undistributed");
                    assert_eq!(dist.completed, false, "a new distribution is not completed");
                    assert_eq!(dist.asset_token, asset_id, "asset_token mismatch");
                    assert_eq!(dist.payment_token, payment_id, "payment_token mismatch");
                    assert_eq!(
                        dist.created_at,
                        env.ledger().sequence(),
                        "created_at must be the creating ledger"
                    );
                }
            }
            3 | 4 => {
                if escrowed.is_empty() {
                    continue;
                }
                let idx = input.below(escrowed.len());
                let id = idx as u64;

                let share = d.claimable(&id, &holder);
                if share < 0 {
                    finding(
                        "dividend::claimable",
                        &format!("claimable returned a negative share: {share}"),
                    );
                }
                let remaining = escrowed[idx].saturating_sub(paid_out(&d, idx));
                assert!(
                    share <= remaining.max(0),
                    "claimable ({share}) exceeded the remaining escrow ({remaining})"
                );

                let claimed_before = d.has_claimed(&id, &holder);
                let distributed_before = paid_out(&d, idx);

                let res = d.try_claim(&id, &holder);
                check_no_trap(&res, "dividend::claim");

                if succeeded(&res) {
                    assert!(
                        !claimed_before,
                        "a claim succeeded for a holder that had already claimed"
                    );
                    assert_eq!(
                        d.has_claimed(&id, &holder),
                        true,
                        "a successful claim must set has_claimed"
                    );
                    assert_eq!(
                        paid_out(&d, idx),
                        distributed_before.saturating_add(share),
                        "distributed must advance by exactly the pre-call claimable"
                    );
                } else {
                    assert_eq!(
                        paid_out(&d, idx),
                        distributed_before,
                        "a rejected claim must not move `distributed`"
                    );
                    assert_eq!(
                        d.has_claimed(&id, &holder),
                        claimed_before,
                        "a rejected claim must not flip has_claimed"
                    );
                }

                // Re-claiming must always be refused and must always be free.
                let after_first = paid_out(&d, idx);
                let second = d.try_claim(&id, &holder);
                check_no_trap(&second, "dividend::claim(repeat)");
                assert!(
                    !succeeded(&second),
                    "a holder claimed the same distribution twice"
                );
                assert_eq!(
                    paid_out(&d, idx),
                    after_first,
                    "a refused repeat claim must not move `distributed`"
                );
            }
            5 => {
                // Per-distribution invariants.
                for (i, total) in escrowed.iter().enumerate() {
                    let id = i as u64;
                    let dist = d.get_distribution(&id);
                    assert_eq!(dist.total_amount, *total, "total_amount drift");
                    assert!(
                        dist.distributed <= *total,
                        "distributed ({}) exceeded total_amount ({total})",
                        dist.distributed
                    );
                    assert_eq!(
                        dist.completed,
                        dist.distributed >= *total,
                        "completed flag disagrees with distributed >= total_amount"
                    );
                    for h in &holders {
                        if d.has_claimed(&id, h) {
                            assert_eq!(
                                d.claimable(&id, h),
                                0,
                                "a claimed holder must have a zero claimable"
                            );
                        }
                    }
                }
            }
            6 => {
                // The per-asset filter must be exactly the matching set.
                let for_asset = d.get_distributions_for_asset(&asset_id);
                assert_eq!(
                    for_asset.len() as usize,
                    escrowed.len(),
                    "get_distributions_for_asset must return every distribution"
                );
                for dist in for_asset.iter() {
                    assert_eq!(dist.asset_token, asset_id, "wrong asset in the filter");
                }
                let unrelated = d.get_distributions_for_asset(&Address::generate(&env));
                assert!(
                    unrelated.is_empty(),
                    "an unrelated address must match no distribution"
                );
            }
            7 => {
                // Reads against ids that do not exist must be total.
                let missing = 9_999_999u64;
                assert_eq!(
                    d.claimable(&missing, &holder),
                    0,
                    "claimable on a missing distribution must be 0"
                );
                assert!(
                    !d.has_claimed(&missing, &holder),
                    "has_claimed on a missing distribution must be false"
                );
                let res = d.try_get_distribution(&missing);
                check_no_trap(&res, "dividend::get_distribution(missing)");
                assert!(!succeeded(&res), "get_distribution on a missing id must fail");
                let res = d.try_claim(&missing, &holder);
                check_no_trap(&res, "dividend::claim(missing)");
                assert!(!succeeded(&res), "claiming a missing distribution must fail");
            }
            8 => {
                let _ = d.try_pause(&admin);
                let _ = d.try_unpause(&admin);
            }
            _ => {
                // `upgrade` needs a live deployer the test ledger does not have.
                let hash = BytesN::from_array(&env, &input.bytes32());
                let _ = d.try_upgrade(&admin, &hash);
            }
        }

        // ---- the central property -----------------------------------------
        // Two laws, together saying every escrowed unit is either still in
        // escrow or was paid to exactly one holder, with none created or
        // destroyed:
        //
        //   1. `dividend_escrow == total_escrowed - total_paid`
        //   2. `payment_supply == dividend_escrow + sum(holders' balances)`
        //
        // Note the second law deliberately sums over *all* holders. The earlier
        // version of this target asserted `payment.balance(admin) ==
        // payment_supply - total_escrowed` instead, which stops holding the
        // moment the admin claims its own distribution — the payout lands in the
        // admin's balance and the assertion fails even though nothing is wrong.
        // A dividend target that cannot let the admin claim is not testing the
        // contract, it is testing a rule the contract was never asked to obey.
        let total_escrowed: i128 = escrowed.iter().fold(0i128, |a, t| a.saturating_add(*t));
        let total_paid: i128 = (0..escrowed.len())
            .map(|i| d.get_distribution(&(i as u64)).distributed)
            .fold(0i128, |a, x| a.saturating_add(x));

        assert!(
            total_paid <= total_escrowed,
            "paid out ({total_paid}) exceeded what was ever escrowed ({total_escrowed})"
        );
        assert_eq!(
            payment.balance(&dividend_id),
            total_escrowed - total_paid,
            "dividend contract's payment balance must equal escrowed minus paid out"
        );

        let accounted: i128 = holders.iter().fold(payment.balance(&dividend_id), |acc, h| {
            acc.saturating_add(payment.balance(h))
        });
        assert_eq!(
            accounted, payment_supply,
            "payment units must be fully accounted for across the dividend contract \
             and every holder"
        );
    }
});
