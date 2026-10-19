use super::*;
use multi_class::{ClassPolicy, ShareClass};
use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token,
};

#[contract]
struct Gate;

#[contractimpl]
impl Gate {
    pub fn is_allowed(env: Env, who: Address) -> bool {
        !env.storage()
            .persistent()
            .get::<_, bool>(&who)
            .unwrap_or(false)
    }
    pub fn block(env: Env, who: Address) {
        env.storage().persistent().set(&who, &true);
    }
}

struct Fixture {
    env: Env,
    token: AssetTokenContractClient<'static>,
    admin: Address,
    alice: Address,
    bob: Address,
    gate: Address,
}

impl Fixture {
    fn new(supply: i128) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let gate = env.register(Gate, ());
        let token = AssetTokenContractClient::new(&env, &env.register(AssetTokenContract, ()));
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let s = String::from_str(&env, "Equity");
        token.initialize(&admin, &s, &s, &s, &supply, &7, &gate, &s, &0);
        Self {
            env,
            token,
            admin,
            alice,
            bob,
            gate,
        }
    }

    fn classes() -> Self {
        let f = Self::new(0);
        f.token.enable_multi_class(&f.admin);
        f.token
            .mint_class(&f.admin, &ShareClass::ClassA, &f.alice, &100);
        f.token
            .mint_class(&f.admin, &ShareClass::ClassB, &f.bob, &60);
        f
    }

    fn conserved(&self) {
        assert_eq!(
            self.token.total_supply(),
            [&self.admin, &self.alice, &self.bob, &self.token.address]
                .iter()
                .map(|a| self.token.balance(a))
                .sum::<i128>()
        );
    }

    fn classes_conserved(&self) {
        self.conserved();
        let mut total = 0;
        for class in [ShareClass::ClassA, ShareClass::ClassB] {
            let sum: i128 = [&self.admin, &self.alice, &self.bob]
                .iter()
                .map(|a| self.token.class_balance(&class, a))
                .sum();
            assert_eq!(sum, self.token.class_supply(&class));
            total += sum;
        }
        assert_eq!(total, self.token.total_supply());
    }
}

#[test]
fn mint_burn_transfer_clawback_and_splits_conserve_exactly() {
    let f = Fixture::new(1000);
    f.conserved();
    f.token.transfer(&f.admin, &f.alice, &101);
    f.token.mint(&f.admin, &f.bob, &303);
    f.conserved();
    f.token.clawback(&f.admin, &f.alice, &11);
    assert_eq!(f.token.balance(&f.admin), 910);
    f.conserved();
    f.token.clawback(&f.admin, &f.admin, &5);
    f.conserved();
    f.token.execute_share_split(&f.admin, &1, &7);
    f.conserved();
    f.token.burn(&f.bob, &3);
    f.token.transfer(&f.alice, &f.alice, &1);
    f.conserved();
    f.token.execute_share_split(&f.admin, &3, &2);
    f.conserved();
}

#[test]
fn rejected_mints_and_burns_leave_supply_and_balances_unchanged() {
    let f = Fixture::new(10);
    assert!(f.token.try_mint(&f.alice, &f.alice, &5).is_err());
    assert!(f.token.try_burn(&f.alice, &1).is_err());
    assert!(f.token.try_mint(&f.admin, &f.alice, &0).is_err());
    assert!(f.token.try_mint(&f.admin, &f.alice, &-5).is_err());
    f.env.set_auths(&[]);
    assert!(f.token.try_mint(&f.admin, &f.alice, &5).is_err());
    assert!(f.token.try_burn(&f.admin, &1).is_err());
    assert_eq!(f.token.total_supply(), 10);
    assert_eq!(f.token.balance(&f.admin), 10);
    f.conserved();
}

#[test]
fn mint_overflow_reverts_both_writes() {
    let f = Fixture::new(i128::MAX);
    assert_eq!(
        f.token.try_mint(&f.admin, &f.alice, &1),
        Err(Ok(Error::Overflow.into()))
    );
    assert_eq!(f.token.balance(&f.alice), 0);
    f.conserved();
}

#[test]
fn class_weights_and_independent_balances() {
    let f = Fixture::classes();
    assert_eq!(f.token.voting_weight(&f.alice), 1000);
    assert_eq!(f.token.voting_weight(&f.bob), 0);
    assert_eq!(f.token.dividend_weight(&f.alice), 100);
    assert_eq!(f.token.dividend_weight(&f.bob), 60);
    assert_eq!(f.token.class_balance(&ShareClass::ClassB, &f.alice), 0);
    f.classes_conserved();
}

#[test]
fn a_transfer_converts_to_b_and_explicit_conversion_is_one_way() {
    let f = Fixture::classes();
    f.token
        .transfer_class(&ShareClass::ClassA, &f.alice, &f.bob, &25);
    assert_eq!(f.token.class_balance(&ShareClass::ClassA, &f.alice), 75);
    assert_eq!(f.token.class_balance(&ShareClass::ClassB, &f.bob), 85);
    assert_eq!(f.token.voting_weight(&f.bob), 0);
    f.token.convert_class(&f.alice, &30);
    assert_eq!(f.token.class_balance(&ShareClass::ClassA, &f.alice), 45);
    assert_eq!(f.token.class_balance(&ShareClass::ClassB, &f.alice), 30);
    assert_eq!(f.token.dividend_weight(&f.alice), 75);
    assert!(f.token.try_convert_class(&f.bob, &1).is_err());
    f.classes_conserved();
}

#[test]
fn self_transfer_keeps_class_and_cannot_manufacture_voting_units() {
    let f = Fixture::classes();
    f.token
        .transfer_class(&ShareClass::ClassA, &f.alice, &f.alice, &100);
    assert_eq!(f.token.class_supply(&ShareClass::ClassA), 100);
    assert_eq!(f.token.voting_weight(&f.alice), 1000);
    f.classes_conserved();
}

#[test]
fn per_class_policy_enforces_freeze_and_optional_a_conversion() {
    let f = Fixture::classes();
    f.token.set_class_policy(
        &f.admin,
        &ShareClass::ClassA,
        &ClassPolicy {
            transfers_enabled: false,
            convert_on_transfer: true,
        },
    );
    assert_eq!(
        f.token
            .try_transfer_class(&ShareClass::ClassA, &f.alice, &f.bob, &1),
        Err(Ok(Error::ClassTransferDisabled.into()))
    );
    f.token.set_class_policy(
        &f.admin,
        &ShareClass::ClassA,
        &ClassPolicy {
            transfers_enabled: true,
            convert_on_transfer: false,
        },
    );
    f.token
        .transfer_class(&ShareClass::ClassA, &f.alice, &f.bob, &10);
    assert_eq!(f.token.voting_weight(&f.bob), 100);
    assert!(f
        .token
        .try_set_class_policy(
            &f.admin,
            &ShareClass::ClassB,
            &ClassPolicy {
                transfers_enabled: true,
                convert_on_transfer: true,
            }
        )
        .is_err());
    f.classes_conserved();
}

#[test]
fn class_paths_enforce_compliance_lockup_pause_and_auth() {
    let f = Fixture::classes();
    GateClient::new(&f.env, &f.gate).block(&f.bob);
    assert_eq!(
        f.token
            .try_transfer_class(&ShareClass::ClassA, &f.alice, &f.bob, &1),
        Err(Ok(Error::RecipientNotCompliant.into()))
    );
    f.token.set_lockup(&f.admin, &f.alice, &10);
    assert_eq!(
        f.token.try_convert_class(&f.alice, &1),
        Err(Ok(Error::Locked.into()))
    );
    f.env.ledger().set_sequence_number(10);
    f.token.convert_class(&f.alice, &1);
    f.token.pause(&f.admin);
    assert_eq!(
        f.token.try_burn_class(&f.alice, &ShareClass::ClassA, &1),
        Err(Ok(Error::Paused.into()))
    );
    f.token.unpause(&f.admin);
    f.env.set_auths(&[]);
    assert!(f.token.try_convert_class(&f.alice, &1).is_err());
    assert!(f
        .token
        .try_mint_class(&f.admin, &ShareClass::ClassA, &f.alice, &1)
        .is_err());
    f.classes_conserved();
}

#[test]
fn legacy_mutations_cannot_create_unclassified_units() {
    let f = Fixture::classes();
    assert!(f.token.try_mint(&f.admin, &f.alice, &1).is_err());
    assert!(f.token.try_transfer(&f.alice, &f.bob, &1).is_err());
    assert!(f.token.try_burn(&f.alice, &1).is_err());
    assert!(f.token.try_clawback(&f.admin, &f.alice, &1).is_err());
    assert!(f.token.try_execute_share_split(&f.admin, &2, &1).is_err());
    assert!(f.token.try_enable_multi_class(&f.admin).is_err());
    f.token.burn_class(&f.alice, &ShareClass::ClassA, &20);
    f.token.burn_class(&f.bob, &ShareClass::ClassB, &10);
    f.classes_conserved();
}

#[test]
fn classes_require_empty_supply_and_bound_voting_arithmetic() {
    let f = Fixture::new(1);
    assert_eq!(
        f.token.try_enable_multi_class(&f.admin),
        Err(Ok(Error::NonZeroSupply.into()))
    );
    f.token.burn(&f.admin, &1);
    f.token.enable_multi_class(&f.admin);
    assert_eq!(
        f.token
            .try_mint_class(&f.admin, &ShareClass::ClassA, &f.alice, &i128::MAX),
        Err(Ok(Error::Overflow.into()))
    );
    f.classes_conserved();
}

#[test]
fn buyback_tender_burn_and_payment_are_atomic_and_finalize_does_not_burn_twice() {
    let f = Fixture::new(100);
    f.token.transfer(&f.admin, &f.alice, &20);
    let stable = f
        .env
        .register_stellar_asset_contract_v2(f.admin.clone())
        .address();
    let payment = token::Client::new(&f.env, &stable);
    token::StellarAssetClient::new(&f.env, &stable).mint(&f.token.address, &100);
    f.token.initiate_buyback(&f.admin, &2, &100, &100, &stable);
    f.token.tender(&f.alice, &10);
    assert_eq!(payment.balance(&f.alice), 20);
    assert_eq!(f.token.total_supply(), 90);
    f.conserved();
    // Not enough stablecoins: every balance/supply/budget write must revert.
    payment.transfer(&f.token.address, &f.admin, &80);
    assert!(f.token.try_tender(&f.alice, &5).is_err());
    assert_eq!(f.token.balance(&f.alice), 10);
    assert_eq!(f.token.total_supply(), 90);
    f.token.finalize_buyback(&f.admin);
    assert_eq!(f.token.total_supply(), 90);
    f.conserved();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn stateful_single_class_supply_is_exact_after_every_call(
        operations in proptest::collection::vec((0u8..7, 0usize..3, 0usize..3, -5i128..500), 1..40)
    ) {
        let f = Fixture::new(1000);
        let holders = [&f.admin, &f.alice, &f.bob];
        for (op, from, to, amount) in operations {
            f.env.budget().reset_default();
            match op {
                0 => { let _ = f.token.try_mint(&f.admin, holders[to], &amount); }
                1 => { let _ = f.token.try_burn(holders[from], &amount); }
                2 => { let _ = f.token.try_transfer(holders[from], holders[to], &amount); }
                3 => { let _ = f.token.try_clawback(&f.admin, holders[from], &amount); }
                4 => { let _ = f.token.try_execute_share_split(&f.admin, &1, &3); }
                5 => { let _ = f.token.try_execute_share_split(&f.admin, &3, &2); }
                _ => { let _ = f.token.try_transfer(holders[from], holders[from], &amount); }
            }
            f.conserved();
            for holder in holders { prop_assert!(f.token.balance(holder) >= 0); }
        }
    }

    #[test]
    fn stateful_classes_always_partition_aggregate_supply_and_balances(
        operations in proptest::collection::vec((0u8..5, any::<bool>(), 0usize..3, 0usize..3, -5i128..200), 1..40)
    ) {
        let f = Fixture::classes();
        let holders = [&f.admin, &f.alice, &f.bob];
        for (op, is_a, from, to, amount) in operations {
            f.env.budget().reset_default();
            let class = if is_a { ShareClass::ClassA } else { ShareClass::ClassB };
            match op {
                0 => { let _ = f.token.try_mint_class(&f.admin, &class, holders[to], &amount); }
                1 => { let _ = f.token.try_burn_class(holders[from], &class, &amount); }
                2 => { let _ = f.token.try_transfer_class(&class, holders[from], holders[to], &amount); }
                3 => { let _ = f.token.try_convert_class(holders[from], &amount); }
                _ => { let _ = f.token.try_transfer_class(&class, holders[from], holders[from], &amount); }
            }
            f.classes_conserved();
            for holder in holders {
                let a = f.token.class_balance(&ShareClass::ClassA, holder);
                let b = f.token.class_balance(&ShareClass::ClassB, holder);
                prop_assert_eq!(a + b, f.token.balance(holder));
                prop_assert_eq!(10 * a, f.token.voting_weight(holder));
                prop_assert_eq!(a + b, f.token.dividend_weight(holder));
            }
        }
    }
}
