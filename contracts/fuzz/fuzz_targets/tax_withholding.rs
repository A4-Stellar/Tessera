//! Fuzz target for `tessera-dividend::tax_withholding`.
//!
//! This target exists because of a specific bug the arithmetic found: the
//! withholding was computed as `dividend_amount * rate / 10_000` with a plain
//! `*`, which overflows `i128` at `i128::MAX * 10_000` — a dividend amount the
//! caller controls and a rate the admin sets. The following
//! `dividend_amount - tax_amount` was a second trap, reachable whenever a rate
//! above 100% had been stored, because there was nothing rejecting one. Both
//! are now saturating, and rates are validated to be at most 10_000 bps on every
//! write, so the assertions here are the *proof* that they hold rather than the
//! report that found them.
//!
//! Properties asserted:
//!
//! * `net + tax == dividend` exactly — the split is lossless, which is what a
//!   saturating subtraction is most likely to break;
//! * `tax >= 0` and `net >= 0` for a non-negative dividend, and a negative
//!   dividend is rejected outright;
//! * `rate == 0` yields `tax == 0` and `net == dividend`, and `rate == 10_000`
//!   yields `net == 0` and `tax == dividend`;
//! * a rate above 10_000 bps is rejected on every write path, so an out-of-range
//!   rate can never reach storage in the first place;
//! * a per-residency rate of exactly 0 is honoured as a genuine exemption
//!   rather than falling back to the default — the bug a `Option::or_else`
//!   fallback would introduce;
//! * a non-admin can never move a rate, and re-initialisation is always
//!   refused.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};
use tessera_dividend::tax_withholding::{TaxWithholdingContract, TaxWithholdingContractClient};
use tessera_fuzz_harness::{check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS};

/// Every rate the fuzzer will try to store. `10_001` and `u32::MAX` are the
/// interesting rejections: the first is the smallest illegal value, the second
/// the largest a `u32` can hold.
const RATES: [u32; 10] = [
    0, 1, 2, 5, 2_500, 9_999, 10_000, 10_001, 100_000, u32::MAX,
];

/// Dividend amounts straddling the point where `amount * rate` stops fitting in
/// an `i128` at a rate of 10_000.
const AMOUNTS: [i128; 12] = [
    0,
    1,
    -1,
    100,
    10_000,
    1_000_000,
    1_000_000_000_000_000_000,
    i128::MAX / 10_000,
    i128::MAX / 10_000 + 1,
    i128::MAX / 2,
    i128::MAX,
    i128::MIN + 1,
];

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let compliance_id = env.register(ComplianceContract, ());
    let tax_id = env.register(TaxWithholdingContract, ());
    let admin = Address::generate(&env);
    let custody = Address::generate(&env);
    let impostor = Address::generate(&env);
    let resident = Address::generate(&env);
    let stranger = Address::generate(&env);

    let compliance = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let tax = TaxWithholdingContractClient::new(&env, &tax_id).mock_all_auths();

    compliance.initialize(&admin);

    // Index 0 is attached to `resident`, so a per-hash rate set on it is what
    // `process_dividend(&resident, _)` actually resolves. The other two keep
    // falling back to the default.
    let hashes: [BytesN<32>; 3] = [
        BytesN::from_array(&env, &input.bytes32()),
        BytesN::from_array(&env, &input.bytes32()),
        BytesN::from_array(&env, &[7u8; 32]),
    ];
    compliance.set_tax_residency(&admin, &resident, &hashes[0]);

    let default_rate = RATES[input.below(RATES.len())];
    let init = tax.try_initialize(&admin, &compliance_id, &custody, &default_rate);
    check_no_trap(&init, "tax_withholding::initialize");

    // Everything downstream is gated on whether initialisation actually took
    // effect, so track it rather than assuming it.
    let initialized = succeeded(&init);
    let mut default_rate = default_rate;
    if initialized {
        assert!(
            default_rate <= 10_000,
            "initialize accepted an out-of-range default rate {default_rate}"
        );
    }

    // Explicit per-hash rates, mirroring contract storage.
    let mut rate_of: Vec<Option<u32>> = vec![None; hashes.len()];

    for _ in 0..MAX_OPS {
        step_ledger(&env, &mut input);
        let which = input.below(hashes.len());
        let rate = RATES[input.below(RATES.len())];
        let amount = if input.bool() {
            AMOUNTS[input.below(AMOUNTS.len())]
        } else {
            input.i128_interesting()
        };

        match input.below(8) {
            0 | 1 => {
                let res = tax.try_set_tax_rate(&admin, &hashes[which], &rate);
                check_no_trap(&res, "tax_withholding::set_tax_rate");
                if initialized {
                    if rate > 10_000 {
                        assert!(
                            !succeeded(&res),
                            "set_tax_rate accepted {rate} bps, which is over 100%"
                        );
                    } else if succeeded(&res) {
                        rate_of[which] = Some(rate);
                    }
                }
            }
            2 => {
                let res = tax.try_set_default_tax_rate(&admin, &rate);
                check_no_trap(&res, "tax_withholding::set_default_tax_rate");
                if initialized {
                    if rate > 10_000 {
                        assert!(
                            !succeeded(&res),
                            "set_default_tax_rate accepted {rate} bps, which is over 100%"
                        );
                    } else if succeeded(&res) {
                        // Per-hash rates are untouched by a default change.
                        default_rate = rate;
                    }
                }
            }
            3 => {
                // A non-admin must never be able to move a rate.
                let res = tax.try_set_tax_rate(&impostor, &hashes[which], &rate);
                check_no_trap(&res, "tax_withholding::set_tax_rate(impostor)");
                if initialized {
                    assert!(!succeeded(&res), "a non-admin changed a tax rate");
                }
            }
            4 | 5 => {
                // The randomised amount sweep, against the resident whose rate
                // the shadow state can predict exactly.
                let res = tax.try_process_dividend(&resident, &amount);
                check_no_trap(&res, "tax_withholding::process_dividend");
                if !initialized {
                    continue;
                }
                let (net, tax_amount, custody_addr) = match &res {
                    Ok(Ok(v)) => *v,
                    Ok(Err(_)) => finding(
                        "tax_withholding::process_dividend",
                        "the dividend amount could not be encoded for the ABI",
                    ),
                    Err(Ok(_)) => continue,
                    Err(Err(_)) => unreachable!("check_no_trap already rejected a trap"),
                };

                assert_eq!(custody_addr, custody, "wrong custody address returned");

                if amount < 0 {
                    assert!(
                        !matches!(&res, Ok(Ok(_))),
                        "process_dividend accepted a negative dividend"
                    );
                } else {
                    assert_eq!(
                        net.saturating_add(tax_amount),
                        amount,
                        "net + tax must reconstruct the dividend exactly"
                    );
                    if net < 0 || tax_amount < 0 {
                        finding(
                            "tax_withholding::process_dividend",
                            &format!("negative split: net {net}, tax {tax_amount}"),
                        );
                    }
                }

                // Reconstruct the rate the contract actually used and check it
                // against the shadow state. This is what catches a fallback that
                // ignores a legitimately-stored zero rate.
                let effective = rate_of[0].unwrap_or(default_rate);
                assert!(effective <= 10_000, "an illegal rate reached storage");
                if amount > 0 {
                    // Exact u128 arithmetic, matching the contract's widened
                    // intermediate. Using `saturating_mul` here would clamp at
                    // `i128::MAX` and quietly agree with the old under-withholding
                    // bug instead of catching it.
                    let expected_tax = ((amount as u128) * (effective as u128) / 10_000) as i128;
                    assert_eq!(
                        tax_amount, expected_tax,
                        "withholding must be `amount * {effective} / 10_000`"
                    );
                    assert!(
                        tax_amount <= amount,
                        "withholding exceeded the dividend at rate {effective}, amount {amount}"
                    );
                }
            }
            6 => {
                // Boundary sweep, gated on initialization. If `initialize` was
                // rejected (an illegal initial default rate), every admin-gated
                // setter below returns `NotInitialized`, so asserting success
                // here would fail for a reason that has nothing to do with the
                // rate boundaries this arm exists to check.
                if !initialized {
                    continue;
                }
                // The default rate is saved and restored, and the shadow state is
                // kept in step, so the rest of the iteration is unaffected.
                let saved = default_rate;
                for r in [0u32, 1, 5_000, 9_999, 10_000] {
                    let res = tax.try_set_default_tax_rate(&admin, &r);
                    check_no_trap(&res, "tax_withholding::set_default_tax_rate(sweep)");
                    assert!(succeeded(&res), "a legal rate of {r} bps was rejected");
                    default_rate = r;

                    for a in [0i128, 1, i128::MAX / 10_000, i128::MAX] {
                        let out = tax.try_process_dividend(&stranger, &a);
                        check_no_trap(&out, "tax_withholding::process_dividend(sweep)");
                        // `stranger` has no residency hash, so the default is
                        // guaranteed to be the rate in effect.
                        if let Ok(Ok((net, tax_amount, _))) = &out {
                            assert_eq!(
                                net.saturating_add(*tax_amount),
                                a,
                                "split must be lossless at rate {r}, amount {a}"
                            );
                            // The *mathematically correct* withholding, computed
                            // in u128 with no intermediate clamping. The old
                            // contract clamped the product to i128::MAX before
                            // dividing, so at 50% on a large dividend it
                            // withheld ~0.5% instead of 50% and reported no
                            // error. Asserting the exact value is what catches
                            // that; `saturating_mul` here would just reproduce
                            // the bug and pass.
                            let expected =
                                ((a as u128) * (r as u128) / 10_000) as i128;
                            assert_eq!(
                                *tax_amount, expected,
                                "withholding at rate {r}, amount {a}"
                            );
                            assert!(
                                *tax_amount <= a,
                                "withholding exceeded the dividend at rate {r}, amount {a}"
                            );
                            if r == 0 {
                                assert_eq!(*tax_amount, 0, "0% must withhold nothing");
                                assert_eq!(*net, a, "0% must pass the full amount");
                            }
                            if r == 10_000 {
                                assert_eq!(*net, 0, "100% must net the investor nothing");
                                assert_eq!(*tax_amount, a, "100% must withhold everything");
                            }
                        }
                    }
                }
                let _ = tax.try_set_default_tax_rate(&admin, &saved);
                default_rate = saved;
            }
            _ => {
                // Re-initialising must always be refused, and must never trap.
                let res = tax.try_initialize(&impostor, &compliance_id, &custody, &0);
                check_no_trap(&res, "tax_withholding::initialize(again)");
                if initialized {
                    assert!(!succeeded(&res), "the contract was initialised twice");
                }
            }
        }
    }
});
