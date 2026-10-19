use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    vec, BytesN, IntoVal, String,
};
use tessera_asset_token::{
    multi_class::ShareClass, AssetStatus, AssetTokenContract, AssetTokenContractClient,
};

#[contract]
struct AllowAll;
#[contractimpl]
impl AllowAll {
    pub fn is_allowed(_env: Env, _who: Address) -> bool {
        true
    }
}

struct Fixture {
    env: Env,
    asset: AssetTokenContractClient<'static>,
    vault: EmergencyLiquidationVaultClient<'static>,
    stable: token::Client<'static>,
    admin: Address,
    alice: Address,
    bob: Address,
    committee: Vec<Address>,
}

impl Fixture {
    fn new(classes: bool, supply: i128, proceeds: i128) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let asset = AssetTokenContractClient::new(&env, &env.register(AssetTokenContract, ()));
        let gate = env.register(AllowAll, ());
        let name = String::from_str(&env, "Equity");
        asset.initialize(&admin, &name, &name, &name, &0, &7, &gate, &name, &0);
        let left = supply * 3 / 5;
        if classes {
            asset.enable_multi_class(&admin);
            asset.mint_class(&admin, &ShareClass::ClassA, &alice, &left);
            asset.mint_class(&admin, &ShareClass::ClassB, &bob, &(supply - left));
        } else {
            asset.mint(&admin, &alice, &left);
            asset.mint(&admin, &bob, &(supply - left));
        }
        let stable_address = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        token::StellarAssetClient::new(&env, &stable_address).mint(&admin, &proceeds);
        let stable = token::Client::new(&env, &stable_address);
        let committee = Vec::from_array(
            &env,
            core::array::from_fn::<_, 6, _>(|_| Address::generate(&env)),
        );
        let trustees = vec![
            &env,
            committee.get(0).unwrap(),
            committee.get(1).unwrap(),
            committee.get(2).unwrap(),
        ];
        let reps = vec![
            &env,
            committee.get(3).unwrap(),
            committee.get(4).unwrap(),
            committee.get(5).unwrap(),
        ];
        let id = env.register(
            EmergencyLiquidationVault,
            (asset.address.clone(), stable_address, trustees, reps),
        );
        let vault = EmergencyLiquidationVaultClient::new(&env, &id);
        asset.set_emergency_vault(&admin, &id);
        Self {
            env,
            asset,
            vault,
            stable,
            admin,
            alice,
            bob,
            committee,
        }
    }

    fn quorum(&self) -> Vec<Address> {
        self.committee.slice(0..4)
    }

    fn activate(&self, proceeds: i128) {
        self.vault.activate(&self.quorum(), &self.admin, &proceeds);
    }
}

#[test]
fn every_four_of_six_quorum_can_activate() {
    for omitted_a in 0..6 {
        for omitted_b in (omitted_a + 1)..6 {
            let f = Fixture::new(false, 100, 1000);
            let mut signers = Vec::new(&f.env);
            for i in 0..6 {
                if i != omitted_a && i != omitted_b {
                    signers.push_back(f.committee.get(i).unwrap());
                }
            }
            f.vault.activate(&signers, &f.admin, &1000);
            assert_eq!(f.asset.status(), AssetStatus::Liquidated);
            assert_eq!(f.stable.balance(&f.vault.address), 1000);
        }
    }
}

#[test]
fn fewer_than_four_duplicates_and_outsiders_cannot_trigger() {
    let f = Fixture::new(false, 100, 1000);
    assert_eq!(
        f.vault
            .try_activate(&f.committee.slice(0..3), &f.admin, &1000),
        Err(Ok(LiquidationError::InsufficientSignatures.into()))
    );
    let mut duplicates = f.quorum();
    duplicates.set(3, duplicates.get(0).unwrap());
    assert_eq!(
        f.vault.try_activate(&duplicates, &f.admin, &1000),
        Err(Ok(LiquidationError::DuplicateSigner.into()))
    );
    let mut outsiders = f.quorum();
    outsiders.set(3, Address::generate(&f.env));
    assert_eq!(
        f.vault.try_activate(&outsiders, &f.admin, &1000),
        Err(Ok(LiquidationError::NotCommittee.into()))
    );
    assert!(!f.vault.liquidation_state().active);
    assert_eq!(f.asset.status(), AssetStatus::Active);
    assert_eq!(f.stable.balance(&f.admin), 1000);
}

#[test]
fn four_names_with_only_three_signatures_do_not_authorize() {
    let f = Fixture::new(false, 100, 1000);
    let quorum = f.quorum();
    let invocation = MockAuthInvoke {
        contract: &f.vault.address,
        fn_name: "activate",
        args: (quorum.clone(), f.admin.clone(), 1000i128).into_val(&f.env),
        sub_invokes: &[],
    };
    f.env.mock_auths(&[
        MockAuth {
            address: &quorum.get(0).unwrap(),
            invoke: &invocation,
        },
        MockAuth {
            address: &quorum.get(1).unwrap(),
            invoke: &invocation,
        },
        MockAuth {
            address: &quorum.get(2).unwrap(),
            invoke: &invocation,
        },
    ]);
    assert!(f.vault.try_activate(&quorum, &f.admin, &1000).is_err());
    assert!(!f.vault.liquidation_state().active);
    assert_eq!(f.asset.status(), AssetStatus::Active);
}

#[test]
fn explicit_committee_funder_and_holder_authorization_trees_succeed() {
    let f = Fixture::new(false, 100, 1000);
    let quorum = f.quorum();
    let args: Vec<soroban_sdk::Val> = (quorum.clone(), f.admin.clone(), 1000i128).into_val(&f.env);
    let committee_invoke = MockAuthInvoke {
        contract: &f.vault.address,
        fn_name: "activate",
        args: args.clone(),
        sub_invokes: &[],
    };
    let transfer = MockAuthInvoke {
        contract: &f.stable.address,
        fn_name: "transfer",
        args: (f.admin.clone(), f.vault.address.clone(), 1000i128).into_val(&f.env),
        sub_invokes: &[],
    };
    let funder_invoke = MockAuthInvoke {
        contract: &f.vault.address,
        fn_name: "activate",
        args,
        sub_invokes: &[transfer],
    };
    f.env.mock_auths(&[
        MockAuth {
            address: &quorum.get(0).unwrap(),
            invoke: &committee_invoke,
        },
        MockAuth {
            address: &quorum.get(1).unwrap(),
            invoke: &committee_invoke,
        },
        MockAuth {
            address: &quorum.get(2).unwrap(),
            invoke: &committee_invoke,
        },
        MockAuth {
            address: &quorum.get(3).unwrap(),
            invoke: &committee_invoke,
        },
        MockAuth {
            address: &f.admin,
            invoke: &funder_invoke,
        },
    ]);
    f.activate(1000);
    let burn = MockAuthInvoke {
        contract: &f.asset.address,
        fn_name: "liquidation_burn",
        args: (
            f.vault.address.clone(),
            f.alice.clone(),
            60i128,
            None::<u32>,
        )
            .into_val(&f.env),
        sub_invokes: &[],
    };
    f.env.mock_auths(&[MockAuth {
        address: &f.alice,
        invoke: &MockAuthInvoke {
            contract: &f.vault.address,
            fn_name: "redeem",
            args: (f.alice.clone(), 60i128, None::<u32>).into_val(&f.env),
            sub_invokes: &[burn],
        },
    }]);
    assert_eq!(f.vault.redeem(&f.alice, &60, &None), 600);
    assert_eq!(f.asset.balance(&f.alice), 0);
}

#[test]
fn failed_funding_rolls_back_freeze_and_reserve() {
    let f = Fixture::new(false, 100, 1000);
    assert!(f.vault.try_activate(&f.quorum(), &f.admin, &1001).is_err());
    assert!(!f.vault.liquidation_state().active);
    assert_eq!(f.asset.status(), AssetStatus::Active);
    assert_eq!(f.stable.balance(&f.admin), 1000);
    f.activate(1000);
    assert!(f.vault.try_activate(&f.quorum(), &f.admin, &1000).is_err());
}

#[test]
fn holder_burns_receive_pro_rata_payments_and_cannot_double_claim() {
    let f = Fixture::new(false, 100, 1000);
    f.activate(1000);
    assert_eq!(f.vault.redeem(&f.alice, &20, &None), 200);
    assert_eq!(f.vault.redeem(&f.bob, &40, &None), 400);
    assert_eq!(f.vault.redeem(&f.alice, &40, &None), 400);
    assert_eq!(f.asset.total_supply(), 0);
    assert_eq!(f.stable.balance(&f.alice), 600);
    assert_eq!(f.stable.balance(&f.bob), 400);
    assert_eq!(f.stable.balance(&f.vault.address), 0);
    assert!(f.vault.try_redeem(&f.alice, &1, &None).is_err());
    assert_eq!(f.vault.liquidation_state().paid, 1000);
}

#[test]
fn payout_rounding_is_independent_of_claim_chunk_size() {
    let f = Fixture::new(false, 5, 7);
    f.activate(7);
    let first = f.vault.redeem(&f.alice, &1, &None);
    let second = f.vault.redeem(&f.alice, &2, &None);
    assert_eq!(first + second, 4); // floor(3 * 7 / 5)
    assert_eq!(f.vault.redeem(&f.bob, &2, &None), 2);
    assert_eq!(f.stable.balance(&f.vault.address), 1); // indivisible residual
    assert_eq!(f.asset.total_supply(), 0);
}

#[test]
fn settlement_handles_products_wider_than_u128() {
    let amount = 100_000_000_000_000_000_000i128;
    let f = Fixture::new(false, amount, amount);
    f.activate(amount);
    let alice = amount * 3 / 5;
    assert_eq!(f.vault.redeem(&f.alice, &alice, &None), alice);
    assert_eq!(
        f.vault.redeem(&f.bob, &(amount - alice), &None),
        amount - alice
    );
    assert_eq!(f.stable.balance(&f.vault.address), 0);
}

#[test]
fn preactivation_and_invalid_claims_do_not_mutate_balances() {
    let f = Fixture::new(false, 100, 1000);
    assert!(f.vault.try_redeem(&f.alice, &1, &None).is_err());
    f.activate(1000);
    for amount in [-1, 0, 61] {
        assert!(f.vault.try_redeem(&f.alice, &amount, &None).is_err());
    }
    assert_eq!(f.asset.balance(&f.alice), 60);
    assert_eq!(f.vault.redeemed(&f.alice), 0);
    assert_eq!(f.vault.liquidation_state().burned, 0);
}

#[test]
fn liquidation_permanently_blocks_all_market_and_admin_bypasses() {
    let f = Fixture::new(false, 100, 1000);
    assert!(f
        .asset
        .try_deposit_liquidation_proceeds(&f.admin, &1)
        .is_err());
    assert!(f.asset.try_set_emergency_vault(&f.admin, &f.alice).is_err());
    assert!(f.asset.try_activate_liquidation(&f.alice).is_err());
    assert!(f
        .asset
        .try_upgrade(&f.admin, &BytesN::from_array(&f.env, &[0; 32]))
        .is_err());
    f.activate(1000);
    f.asset.unpause(&f.admin);
    assert!(f.asset.try_transfer(&f.alice, &f.bob, &1).is_err());
    assert!(f.asset.try_mint(&f.admin, &f.alice, &1).is_err());
    assert!(f.asset.try_burn(&f.alice, &1).is_err());
    assert!(f.asset.try_clawback(&f.admin, &f.alice, &1).is_err());
    assert!(f.asset.try_execute_share_split(&f.admin, &2, &1).is_err());
    assert!(f
        .asset
        .try_redeem_liquidation_proceeds(&f.alice, &1)
        .is_err());
    // A lockup and trading pause cannot trap court-ordered settlement funds.
    f.asset.set_lockup(&f.admin, &f.alice, &10000);
    f.asset.pause(&f.admin);
    assert_eq!(f.vault.redeem(&f.alice, &60, &None), 600);
}

#[test]
fn both_share_classes_freeze_and_redeem_with_equal_economic_weight() {
    let f = Fixture::new(true, 100, 1000);
    f.activate(1000);
    assert!(f
        .asset
        .try_transfer_class(&ShareClass::ClassA, &f.alice, &f.bob, &1)
        .is_err());
    assert!(f
        .asset
        .try_transfer_class(&ShareClass::ClassB, &f.bob, &f.alice, &1)
        .is_err());
    assert!(f.asset.try_convert_class(&f.alice, &1).is_err());
    assert!(f
        .asset
        .try_burn_class(&f.alice, &ShareClass::ClassA, &1)
        .is_err());
    assert!(f.vault.try_redeem(&f.alice, &1, &None).is_err());
    assert!(f.vault.try_redeem(&f.alice, &1, &Some(2)).is_err());
    assert!(f.vault.try_redeem(&f.alice, &1, &Some(1)).is_err());
    assert_eq!(f.vault.redeemed(&f.alice), 0);
    assert_eq!(f.vault.redeem(&f.alice, &60, &Some(0)), 600);
    assert_eq!(f.vault.redeem(&f.bob, &40, &Some(1)), 400);
    assert_eq!(f.asset.class_supply(&ShareClass::ClassA), 0);
    assert_eq!(f.asset.class_supply(&ShareClass::ClassB), 0);
    assert_eq!(f.asset.total_supply(), 0);
}

#[test]
fn mixed_class_claims_share_one_cumulative_holder_entitlement() {
    let f = Fixture::new(true, 5, 7);
    f.asset.convert_class(&f.alice, &1);
    f.activate(7);
    let first = f.vault.redeem(&f.alice, &1, &Some(0));
    let second = f.vault.redeem(&f.alice, &1, &Some(1));
    let third = f.vault.redeem(&f.alice, &1, &Some(0));
    assert_eq!(first + second + third, 4);
    assert_eq!(f.vault.redeemed(&f.alice), 3);
    assert_eq!(f.vault.redeem(&f.bob, &2, &Some(1)), 2);
    assert_eq!(f.asset.total_supply(), 0);
}

#[test]
fn failed_payout_rolls_back_holder_burn_and_claim_accounting() {
    let f = Fixture::new(false, 100, 1000);
    f.activate(1000);
    // The fixture mocks contract auth: emulate an external stablecoin
    // confiscation so the transfer fails after the asset burn.
    f.stable.transfer(&f.vault.address, &f.admin, &1000);
    assert!(f.vault.try_redeem(&f.alice, &60, &None).is_err());
    assert_eq!(f.asset.balance(&f.alice), 60);
    assert_eq!(f.asset.total_supply(), 100);
    assert_eq!(f.vault.redeemed(&f.alice), 0);
    assert_eq!(f.vault.liquidation_state().paid, 0);
}

#[test]
fn holders_cannot_be_burned_without_their_signature() {
    let f = Fixture::new(false, 100, 1000);
    f.activate(1000);
    f.env.set_auths(&[]);
    assert!(f.vault.try_redeem(&f.alice, &60, &None).is_err());
    assert!(f
        .asset
        .try_liquidation_burn(&f.vault.address, &f.alice, &60, &None)
        .is_err());
    assert_eq!(f.asset.balance(&f.alice), 60);
}

#[test]
#[should_panic]
fn deployment_rejects_duplicate_committee() {
    let f = Fixture::new(false, 100, 1000);
    let duplicates = vec![&f.env, f.alice.clone(), f.alice.clone(), f.bob.clone()];
    f.env.register(
        EmergencyLiquidationVault,
        (
            f.asset.address.clone(),
            f.stable.address.clone(),
            duplicates,
            f.committee.slice(0..3),
        ),
    );
}

#[test]
#[should_panic]
fn deployment_requires_asset_admin_signature() {
    let f = Fixture::new(false, 100, 1000);
    f.env.set_auths(&[]);
    f.env.register(
        EmergencyLiquidationVault,
        (
            f.asset.address.clone(),
            f.stable.address.clone(),
            f.committee.slice(0..3),
            f.committee.slice(3..6),
        ),
    );
}
