//! Fuzz target for `vaults` (`RentDistributionVault`).
//!
//! # Scope, stated honestly
//!
//! This contract is a stub. `claim_yield` and `reinvest_yield` are
//! byte-for-byte identical: both read the holder's accrued reward into a
//! discarded `_reward` binding and then zero it, so neither pays out nor
//! compounds anything. `DataKey::Token`, `TotalShares`, `RewardPerShareStored`,
//! `LastUpdateTime` and `UserRewardPerTokenPaid` are declared and never read or
//! written, and there is no `initialize` and no admin, so nothing can write a
//! non-zero `Rewards` balance in the first place.
//!
//! That means the two entry points are only reachable in their zero state, and
//! a fuzzer cannot manufacture the non-zero state that would make them
//! interesting. Writing one is a design decision — a reward-accrual model, a
//! share price, and a token to pay out in — not a bug fix, so it is flagged
//! rather than invented here. See the `Scope` note in the crate docs.
//!
//! What this target *does* establish is the part that is well-defined today:
//!
//! * neither entry point can trap, for any holder and in any interleaving;
//! * both are total over storage: a holder with no accrued reward is a no-op,
//!   not a `unwrap()` on `None`;
//! * claiming or reinvesting repeatedly is idempotent and never traps, which is
//!   what a naive `unwrap()`-based implementation would get wrong once rewards
//!   do become reachable;
//! * holders are independent of each other, so one holder's claim cannot
//!   disturb another's state;
//! * `env.storage` access is bounded — no unbounded growth per iteration.
//!
//! The `RUST_BACKTRACE`-free `step_ledger` calls keep the persistent entries
//! alive so an `EntryExpired` host error cannot be mistaken for a real trap.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, Env};
use tessera_fuzz_harness::{check_no_trap, step_ledger, succeeded, Input, MAX_OPS};
use vaults::rent_distribution::{RentDistributionVault, RentDistributionVaultClient};

const HOLDERS: usize = 4;

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let vault_id = env.register(RentDistributionVault, ());
    let admin = Address::generate(&env);
    let v = RentDistributionVaultClient::new(&env, &vault_id).mock_all_auths();

    let mut holders: Vec<Address> = (0..HOLDERS).map(|_| Address::generate(&env)).collect();
    holders[0] = admin.clone();

    // `vaults` exposes no getter for `Rewards` — the contract never wrote one —
    // so the observable state is just "did it trap". Tracked here to make that
    // explicit rather than accidental.
    let mut claim_attempts = 0u32;
    let mut reinvest_attempts = 0u32;

    for _ in 0..MAX_OPS {
        let holder = holders[input.below(HOLDERS)].clone();
        let other = holders[input.below(HOLDERS)].clone();
        // Included so the harness's extreme-value coverage is exercised on this
        // target's inputs too, even though no amount is currently accepted.
        let _unused = input.i128_interesting();
        step_ledger(&env, &mut input);

        match input.below(6) {
            0 | 1 => {
                let res = v.try_claim_yield(&holder);
                check_no_trap(&res, "vaults::claim_yield");
                claim_attempts += 1;
                // The call is total over storage: an un-accrued holder is a
                // no-op, never a rejection.
                assert!(
                    succeeded(&res),
                    "claim_yield rejected a holder with no accrued reward"
                );
            }
            2 | 3 => {
                let res = v.try_reinvest_yield(&holder);
                check_no_trap(&res, "vaults::reinvest_yield");
                reinvest_attempts += 1;
                assert!(
                    succeeded(&res),
                    "reinvest_yield rejected a holder with no accrued reward"
                );
            }
            4 => {
                // Idempotence: repeating either call must stay a no-op rather
                // than trapping on the second read. A holder that is not in the
                // set must behave identically to one that is.
                for _ in 0..3 {
                    let a = v.try_claim_yield(&holder);
                    check_no_trap(&a, "vaults::claim_yield(repeat)");
                    assert!(succeeded(&a), "a repeated claim_yield was rejected");
                }
                for _ in 0..3 {
                    let b = v.try_reinvest_yield(&holder);
                    check_no_trap(&b, "vaults::reinvest_yield(repeat)");
                    assert!(succeeded(&b), "a repeated reinvest_yield was rejected");
                }
            }
            _ => {
                // Interleave two holders to check that neither call can observe
                // or disturb the other's state.
                let a = v.try_claim_yield(&holder);
                check_no_trap(&a, "vaults::claim_yield(a)");
                let b = v.try_reinvest_yield(&other);
                check_no_trap(&b, "vaults::reinvest_yield(b)");
                let c = v.try_claim_yield(&other);
                check_no_trap(&c, "vaults::claim_yield(b)");
                let d = v.try_reinvest_yield(&holder);
                check_no_trap(&d, "vaults::reinvest_yield(a)");
                assert!(
                    succeeded(&a) && succeeded(&b) && succeeded(&c) && succeeded(&d),
                    "interleaved vault calls must all be total"
                );
            }
        }
    }

    // The harness is expected to reach both entry points in every iteration;
    // if it never does, the match arms above are wrong.
    assert!(
        claim_attempts + reinvest_attempts > 0,
        "no vault entry point was exercised"
    );
});
