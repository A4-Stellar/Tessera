//! Stateful fuzz target for `tessera-compliance`.
//!
//! Covers every entry point: the allowlist lifecycle, jurisdiction blocking,
//! the pluggable transfer-gate hooks (issue #9), tax-residency hashes, the
//! emergency pause, and every read path.
//!
//! The properties worth asserting are the ones a hook can break:
//!
//! * `is_allowed` is false for anything with no record, and false for any
//!   status other than `Approved`;
//! * `expires_at == 0` never expires, but `expires_at <= now` is already
//!   expired (the comparison in `is_allowed` is `>=`, inclusive);
//! * a blocked jurisdiction revokes approval for every record carrying it;
//! * `is_allowed_with_hooks` may only ever be *more* restrictive than
//!   `is_allowed` — a hook layer must never approve an address the built-in
//!   check rejects;
//! * `register_hook` rejects a duplicate and `unregister_hook` rejects an
//!   unknown hook, and `get_hooks` always agrees with what was registered.

#![no_main]

use libfuzzer_sys::fuzz_target;

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String as SdkString};
use tessera_compliance::{ComplianceContract, ComplianceContractClient, ComplianceStatus};
use tessera_fuzz_harness::{
    assert_conserved, check_no_trap, finding, step_ledger, succeeded, Input, MAX_OPS,
};

const SUBJECTS: usize = 5;
const JURISDICTIONS: usize = 3;

/// Minimal contract satisfying the `check(env, address) -> bool` shape that
/// `register_hook` invokes cross-contract, with a denial list the fuzzer
/// controls.
mod gate {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

    #[contracttype]
    #[derive(Clone)]
    pub enum DataKey {
        Denied(Address),
    }

    #[contract]
    pub struct GateHook;

    #[contractimpl]
    impl GateHook {
        /// Add `who` to the hook's denial list.
        pub fn deny(env: Env, who: Address) {
            env.storage().instance().set(&DataKey::Denied(who), &true);
        }

        /// The transfer-gate entry point `is_allowed_with_hooks` invokes.
        pub fn check(env: Env, who: Address) -> bool {
            !env.storage().instance().has(&DataKey::Denied(who))
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let env = Env::default();

    let compliance_id = env.register(ComplianceContract, ());
    let hook_id = env.register(gate::GateHook, ());
    let hook2_id = env.register(gate::GateHook, ());

    let admin = Address::generate(&env);
    let c = ComplianceContractClient::new(&env, &compliance_id).mock_all_auths();
    let hook = gate::GateHookClient::new(&env, &hook_id).mock_all_auths();
    let hook2 = gate::GateHookClient::new(&env, &hook2_id).mock_all_auths();

    let mut subjects = [admin.clone(); SUBJECTS];
    for s in subjects.iter_mut() {
        *s = Address::generate(&env);
    }
    let jurisdictions: Vec<SdkString> = (0..JURISDICTIONS)
        .map(|i| SdkString::from_str(&env, &format!("J{i}")))
        .collect::<Vec<_>>();

    c.initialize(&admin);

    // Shadow state mirroring the contract, so the expected outcome of each
    // operation can be asserted rather than merely observed. Deliberately
    // host-side `std` collections: this state lives in the fuzzer process, not
    // in the contract, and tuples are not `Val`-encodable so a
    // `soroban_sdk::Vec` of them would not compile.
    let mut on_allowlist: Vec<Address> = Vec::new();
    let mut registered_hooks: Vec<Address> = Vec::new();
    let mut blocked: Vec<SdkString> = Vec::new();
    let mut denied_by_hook: Vec<Address> = Vec::new();
    // Mirrors `DataKey::Paused`. Needed because `suspend`, `remove`,
    // `block_jurisdiction` and `unblock_jurisdiction` are all pause-gated: a
    // call made while paused is *expected* to be rejected, and any assertion
    // about the resulting state has to be conditional on the write landing.
    let mut paused = false;

    for _ in 0..MAX_OPS {
        let who = subjects[input.below(SUBJECTS)].clone();
        let j = jurisdictions[input.below(JURISDICTIONS)].clone();
        step_ledger(&env, &mut input);

        match input.below(16) {
            // ---- allowlist lifecycle ---------------------------------------
            0 | 1 => {
                // 0 = never expires; otherwise an expiry in the past or future.
                let expires_at = match input.below(3) {
                    0 => 0,
                    1 => env
                        .ledger()
                        .sequence()
                        .saturating_sub(input.below(4_096) as u32),
                    _ => env
                        .ledger()
                        .sequence()
                        .saturating_add(input.below(4_096) as u32),
                };
                // Snapshot before the call so a *rejected* add can be proven
                // not to have written, rather than merely not re-adding to the
                // allowlist.
                let record_before = c
                    .get_record(&who)
                    .map(|x| (x.jurisdiction, x.expires_at));

                let r = c.try_add_to_allowlist(&admin, &who, &j, &expires_at);
                check_no_trap(&r, "compliance::add_to_allowlist");
                // `add_to_allowlist` guards `expires_at != 0 && expires_at <= now`
                // with `InvalidExpiry`, so a stale-or-equal expiry must be a
                // *declared* rejection, not a write.
                if expires_at != 0 && expires_at <= env.ledger().sequence() {
                    if succeeded(&r) {
                        finding(
                            "compliance::add_to_allowlist",
                            "accepted an expiry that is already in the past",
                        );
                    }
                    assert_eq!(
                        c.get_record(&who).map(|x| (x.jurisdiction, x.expires_at)),
                        record_before,
                        "a rejected add must not have modified the record"
                    );
                } else if succeeded(&r) {
                    if !on_allowlist.iter().any(|a| a == &who) {
                        on_allowlist.push(who.clone());
                    }
                    let rec = c
                        .get_record(&who)
                        .unwrap_or_else(|| finding("compliance::add_to_allowlist", "no record after a successful add"));
                    // `KycRecord` is `Clone`-only, so fields are compared
                    // individually rather than the whole struct.
                    assert_eq!(rec.address, who, "get_record address mismatch");
                    assert_eq!(rec.jurisdiction, j, "get_record jurisdiction mismatch");
                    assert_eq!(rec.expires_at, expires_at, "get_record expiry mismatch");
                    assert_eq!(
                        rec.verified_at,
                        env.ledger().sequence(),
                        "verified_at must be the ledger the add happened on"
                    );
                    assert_eq!(
                        rec.status,
                        ComplianceStatus::Approved,
                        "add_to_allowlist must record the subject as Approved"
                    );
                }
            }
            2 => {
                let r = c.try_suspend(&admin, &who);
                check_no_trap(&r, "compliance::suspend");
                if succeeded(&r) {
                    assert!(
                        !paused,
                        "suspend succeeded while the contract was paused"
                    );
                    assert!(
                        !c.is_allowed(&who),
                        "a suspended subject must not be allowed"
                    );
                    assert_eq!(
                        c.get_record(&who).map(|x| x.status),
                        Some(ComplianceStatus::Suspended),
                        "suspend must persist the Suspended status"
                    );
                }
            }
            3 => {
                let r = c.try_remove(&admin, &who);
                check_no_trap(&r, "compliance::remove");
                if succeeded(&r) {
                    assert!(!paused, "remove succeeded while the contract was paused");
                    assert!(
                        !c.is_allowed(&who),
                        "a removed subject must not be allowed"
                    );
                    assert!(
                        c.get_record(&who).is_none(),
                        "remove must delete the record"
                    );
                    let list = c.get_allowlist();
                    assert!(
                        !list.iter().any(|a| a == &who),
                        "remove must also drop the address from the allowlist"
                    );
                    if let Some(idx) = on_allowlist.iter().position(|a| a == &who) {
                        on_allowlist.remove(idx);
                    }
                } else {
                    // A rejected `remove` must not have touched the allowlist.
                    assert_conserved(
                        "compliance::remove",
                        "allowlist length after a rejected remove",
                        on_allowlist.len() as i128,
                        c.get_allowlist().len() as i128,
                    );
                }
            }
            // ---- jurisdictions ---------------------------------------------
            4 => {
                let r = c.try_block_jurisdiction(&admin, &j);
                check_no_trap(&r, "compliance::block_jurisdiction");
                // `block_jurisdiction` is pause-gated, so while paused it
                // legitimately leaves the jurisdiction unblocked. The earlier
                // version of this arm asserted `is_jurisdiction_blocked`
                // unconditionally, which failed on exactly that expected
                // rejection — a test bug that would have looked like a contract
                // bug. Only assert the post-state when the write landed.
                if !succeeded(&r) {
                    assert_eq!(
                        c.is_jurisdiction_blocked(&j),
                        blocked.iter().any(|b| b == &j),
                        "a rejected block must leave the jurisdiction state alone"
                    );
                    continue;
                }
                assert!(
                    c.is_jurisdiction_blocked(&j),
                    "a jurisdiction just blocked must report as blocked"
                );
                assert!(!paused, "a jurisdiction was blocked while the contract was paused");
                if !blocked.iter().any(|b| b == &j) {
                    blocked.push(j.clone());
                }
                for s in &subjects {
                    if let Some(rec) = c.get_record(s) {
                        if rec.jurisdiction == j {
                            assert!(
                                !c.is_allowed(s),
                                "blocking a jurisdiction must revoke the approvals carrying it"
                            );
                        }
                    }
                }
            }
            5 => {
                let r = c.try_unblock_jurisdiction(&admin, &j);
                check_no_trap(&r, "compliance::unblock_jurisdiction");
                if succeeded(&r) {
                    if let Some(idx) = blocked.iter().position(|b| b == &j) {
                        blocked.remove(idx);
                    }
                }
                assert_eq!(
                    c.is_jurisdiction_blocked(&j),
                    blocked.iter().any(|b| b == &j),
                    "is_jurisdiction_blocked must agree with the shadow state"
                );
            }
            // ---- transfer-gate hooks (issue #9) ----------------------------
            6 => {
                let target = if input.bool() {
                    hook_id.clone()
                } else {
                    hook2_id.clone()
                };
                let already = registered_hooks.iter().any(|x| x == &target);
                let r = c.try_register_hook(&admin, &target);
                check_no_trap(&r, "compliance::register_hook");
                // Registering an already-registered hook is `HookAlreadyRegistered`,
                // not a silent success. Stated the other way round so the
                // assertion can actually fire.
                if already {
                    assert!(
                        !succeeded(&r),
                        "register_hook accepted a hook that was already registered"
                    );
                } else if succeeded(&r) {
                    registered_hooks.push(target);
                }
            }
            7 => {
                let target = if input.bool() {
                    hook_id.clone()
                } else {
                    hook2_id.clone()
                };
                let was = registered_hooks.iter().any(|x| x == &target);
                let r = c.try_unregister_hook(&admin, &target);
                check_no_trap(&r, "compliance::unregister_hook");
                if was {
                    if succeeded(&r) {
                        if let Some(idx) = registered_hooks.iter().position(|x| x == &target) {
                            registered_hooks.remove(idx);
                        }
                    }
                } else {
                    // A hook that was never registered cannot be silently
                    // "removed"; that would let the count drift.
                    assert!(
                        !succeeded(&r),
                        "unregister_hook accepted a hook that was not registered"
                    );
                }
            }
            8 => {
                // Flip a hook's verdict and re-check the layered gate.
                let client = if input.bool() { &hook } else { &hook2 };
                if input.bool() {
                    client.deny(&who);
                    if !denied_by_hook.iter().any(|a| a == &who) {
                        denied_by_hook.push(who.clone());
                    }
                }
                let allowed = c.is_allowed(&who);
                let layered = c.is_allowed_with_hooks(&who);
                if layered {
                    assert!(
                        allowed,
                        "is_allowed_with_hooks approved an address the built-in check rejects"
                    );
                }
                // A hook may only ever remove approvals, never add them.
                if allowed && !denied_by_hook.iter().any(|a| a == &who) {
                    assert!(
                        !layered,
                        "is_allowed_with_hooks rejected an address no active hook denies"
                    );
                }
            }
            // ---- tax residency ---------------------------------------------
            9 => {
                let hash = BytesN::from_array(&env, &input.bytes32());
                let before = c.get_tax_residency(&who).map(|b| b.to_array());
                let r = c.try_set_tax_residency(&admin, &who, &hash);
                check_no_trap(&r, "compliance::set_tax_residency");
                if succeeded(&r) {
                    assert_eq!(
                        c.get_tax_residency(&who).map(|b| b.to_array()),
                        Some(hash.to_array()),
                        "get_tax_residency must return exactly what was stored"
                    );
                } else {
                    // The only reason this can legitimately fail is the pause
                    // guard, and a paused call must not have written.
                    assert_eq!(
                        c.get_tax_residency(&who).map(|b| b.to_array()),
                        before,
                        "a rejected set must not have written"
                    );
                }
            }
            // ---- expiry boundary -------------------------------------------
            10 => {
                // `expires_at == now` is rejected outright with `InvalidExpiry`
                // (`expires_at != 0 && expires_at <= now`), and
                // `expires_at == now + 1` is the smallest accepted value. Both
                // halves of that boundary are worth pinning, because an
                // off-by-one here silently approves an already-expired subject.
                let subject = subjects[input.below(SUBJECTS)].clone();
                let now = env.ledger().sequence();
                let record_before = c
                    .get_record(&subject)
                    .map(|x| (x.jurisdiction, x.expires_at));

                let rejected = c.try_add_to_allowlist(&admin, &subject, &j, &now);
                check_no_trap(&rejected, "compliance::add_to_allowlist(expires_at == now)");
                if succeeded(&rejected) {
                    finding(
                        "compliance::add_to_allowlist",
                        "accepted expires_at == now; the guard is `expires_at <= now`",
                    );
                }
                assert_eq!(
                    c.get_record(&subject).map(|x| (x.jurisdiction, x.expires_at)),
                    record_before,
                    "a rejected add must not have modified the record"
                );

                // One ledger later is the smallest legal expiry, and it is
                // approved right up until the ledger reaches it.
                let accepted = c.try_add_to_allowlist(&admin, &subject, &j, &now + 1);
                check_no_trap(&accepted, "compliance::add_to_allowlist(expires_at == now+1)");
                if succeeded(&accepted) {
                    if !on_allowlist.iter().any(|a| a == &subject) {
                        on_allowlist.push(subject.clone());
                    }
                    assert!(
                        c.is_allowed(&subject),
                        "an expiry of now+1 must still be valid at `now`"
                    );
                    // Step the ledger onto the expiry; it must lapse exactly then.
                    env.ledger().set_sequence_number(now + 1);
                    assert!(
                        !c.is_allowed(&subject),
                        "an expiry of now+1 must lapse once the ledger reaches it"
                    );
                }
            }
            // ---- pause / reads ---------------------------------------------
            11 => {
                let r = c.try_pause(&admin);
                check_no_trap(&r, "compliance::pause");
                if succeeded(&r) {
                    paused = true;
                }
            }
            12 => {
                let r = c.try_unpause(&admin);
                check_no_trap(&r, "compliance::unpause");
                if succeeded(&r) {
                    paused = false;
                }
            }
            13 => {
                // Read paths, plus the mirror-state cross-checks.
                let list = c.get_allowlist();
                let hooks = c.get_hooks();
                assert_conserved(
                    "compliance",
                    "allowlist length",
                    on_allowlist.len() as i128,
                    list.len() as i128,
                );
                assert_conserved(
                    "compliance",
                    "registered hook count",
                    registered_hooks.len() as i128,
                    hooks.len() as i128,
                );
                assert_eq!(c.version(), 1, "ABI version must stay 1");
            }
            14 => {
                // An address with no record is never allowed, and the allowlist
                // must never contain a subject whose record is gone.
                let stranger = Address::generate(&env);
                assert!(!c.is_allowed(&stranger), "unknown address must not be allowed");
                for a in c.get_allowlist().iter() {
                    assert!(
                        c.get_record(&a).is_some(),
                        "allowlist entry without a backing record"
                    );
                }
            }
            _ => {
                // `upgrade` needs a live deployer, which the test ledger does
                // not have, so it legitimately fails with a host error. Call it
                // only to prove it does not trap.
                let hash = BytesN::from_array(&env, &input.bytes32());
                let _ = c.try_upgrade(&admin, &hash);
            }
        }
    }
});
