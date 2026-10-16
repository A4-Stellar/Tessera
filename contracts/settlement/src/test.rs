extern crate std;

use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, IntoVal, String,
};
use tessera_asset_token::{AssetTokenContract, AssetTokenContractClient};
use tessera_compliance::{ComplianceContract, ComplianceContractClient};

use crate::{Error, SettlementContract, SettlementContractClient, TradeLeg};

struct Market<'a> {
    env: Env,
    admin: Address,
    engine: SettlementContractClient<'a>,
    compliance: ComplianceContractClient<'a>,
    real_estate: Address,
    bond: Address,
    usdc: Address,
    a: Address,
    b: Address,
    c: Address,
}

fn asset_token(env: &Env, admin: &Address, compliance: &Address, symbol: &str) -> Address {
    let id = env.register(AssetTokenContract, ());
    AssetTokenContractClient::new(env, &id).initialize(
        admin,
        &String::from_str(env, symbol),
        &String::from_str(env, symbol),
        &String::from_str(env, "rwa"),
        &1_000_000,
        &7,
        compliance,
        &String::from_str(env, "test asset"),
        &1_000_000,
    );
    id
}

/// A holds real estate, B holds bonds, C holds USDC; all three are KYC'd.
fn setup<'a>() -> Market<'a> {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);

    let compliance_id = env.register(ComplianceContract, ());
    let compliance = ComplianceContractClient::new(&env, &compliance_id);
    compliance.initialize(&admin);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    let us = String::from_str(&env, "US");
    for who in [&admin, &a, &b, &c] {
        compliance.add_to_allowlist(&admin, who, &us, &0);
    }

    let real_estate = asset_token(&env, &admin, &compliance_id, "RE");
    let bond = asset_token(&env, &admin, &compliance_id, "BOND");
    let usdc = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    TokenClient::new(&env, &real_estate).transfer(&admin, &a, &500);
    TokenClient::new(&env, &bond).transfer(&admin, &b, &300);
    StellarAssetClient::new(&env, &usdc).mint(&c, &10_000);

    let engine_id = env.register(SettlementContract, ());
    let engine = SettlementContractClient::new(&env, &engine_id);
    engine.initialize(&admin);
    engine.register_asset(&admin, &real_estate, &Some(compliance_id.clone()));
    engine.register_asset(&admin, &bond, &Some(compliance_id.clone()));
    engine.register_asset(&admin, &usdc, &None);

    Market {
        env,
        admin,
        engine,
        compliance,
        real_estate,
        bond,
        usdc,
        a,
        b,
        c,
    }
}

/// The host-level error a contract panic with `e` surfaces as.
fn ce(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

fn leg(from: &Address, to: &Address, token: &Address, amount: i128) -> TradeLeg {
    TradeLeg {
        from: from.clone(),
        to: to.clone(),
        token: token.clone(),
        amount,
    }
}

fn balance(m: &Market, token: &Address, who: &Address) -> i128 {
    TokenClient::new(&m.env, token).balance(who)
}

/// A -> B real estate, B -> C bonds, C -> A USDC.
fn ring(m: &Market) -> soroban_sdk::Vec<TradeLeg> {
    vec![
        &m.env,
        leg(&m.a, &m.b, &m.real_estate, 100),
        leg(&m.b, &m.c, &m.bond, 50),
        leg(&m.c, &m.a, &m.usdc, 2_000),
    ]
}

fn snapshot(m: &Market) -> [i128; 9] {
    let mut out = [0i128; 9];
    let mut i = 0;
    for token in [&m.real_estate, &m.bond, &m.usdc] {
        for who in [&m.a, &m.b, &m.c] {
            out[i] = balance(m, token, who);
            i += 1;
        }
    }
    out
}

#[test]
fn three_party_ring_settles_atomically() {
    let m = setup();
    let id = m.engine.execute_multilateral_settlement(&ring(&m));
    assert_eq!(id, 0);
    assert_eq!(m.engine.batch_count(), 1);

    assert_eq!(balance(&m, &m.real_estate, &m.a), 400);
    assert_eq!(balance(&m, &m.real_estate, &m.b), 100);
    assert_eq!(balance(&m, &m.bond, &m.b), 250);
    assert_eq!(balance(&m, &m.bond, &m.c), 50);
    assert_eq!(balance(&m, &m.usdc, &m.c), 8_000);
    assert_eq!(balance(&m, &m.usdc, &m.a), 2_000);
}

#[test]
fn every_sender_must_authorize() {
    let m = setup();
    let legs = ring(&m);
    m.engine.execute_multilateral_settlement(&legs);
    let auths = m.env.auths();
    for sender in [&m.a, &m.b, &m.c] {
        assert!(
            auths.iter().any(|(who, _)| who == sender),
            "missing auth for a sender"
        );
    }
}

#[test]
fn missing_signature_reverts_whole_batch() {
    let m = setup();
    let legs = ring(&m);
    let before = snapshot(&m);

    // A and B sign the batch (including their own token transfer), C does
    // not: C's USDC leg is unauthorized, so A's and B's legs must not stick.
    let root_args = (legs.clone(),).into_val(&m.env);
    let a_transfer = MockAuthInvoke {
        contract: &m.real_estate,
        fn_name: "transfer",
        args: (&m.a, &m.b, 100i128).into_val(&m.env),
        sub_invokes: &[],
    };
    let b_transfer = MockAuthInvoke {
        contract: &m.bond,
        fn_name: "transfer",
        args: (&m.b, &m.c, 50i128).into_val(&m.env),
        sub_invokes: &[],
    };
    let a_root = MockAuthInvoke {
        contract: &m.engine.address,
        fn_name: "execute_multilateral_settlement",
        args: root_args,
        sub_invokes: core::slice::from_ref(&a_transfer),
    };
    let b_root = MockAuthInvoke {
        contract: &m.engine.address,
        fn_name: "execute_multilateral_settlement",
        args: (legs.clone(),).into_val(&m.env),
        sub_invokes: core::slice::from_ref(&b_transfer),
    };
    let result = m
        .engine
        .mock_auths(&[
            MockAuth {
                address: &m.a,
                invoke: &a_root,
            },
            MockAuth {
                address: &m.b,
                invoke: &b_root,
            },
        ])
        .try_execute_multilateral_settlement(&legs);
    assert!(result.is_err());
    assert_eq!(snapshot(&m), before);
    assert_eq!(m.engine.batch_count(), 0);
}

#[test]
fn open_chain_is_rejected() {
    let m = setup();
    // A -> B -> C with nothing flowing back to A.
    let legs = vec![
        &m.env,
        leg(&m.a, &m.b, &m.real_estate, 100),
        leg(&m.b, &m.c, &m.bond, 50),
    ];
    let err = m.engine.try_execute_multilateral_settlement(&legs);
    assert_eq!(err, Err(Ok(ce(Error::LegNotInCycle))));
}

#[test]
fn gift_appended_to_a_valid_ring_is_rejected() {
    let m = setup();
    let mut legs = ring(&m);
    let outsider = Address::generate(&m.env);
    legs.push_back(leg(&m.c, &outsider, &m.usdc, 1));
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&legs),
        Err(Ok(ce(Error::LegNotInCycle)))
    );
}

#[test]
fn intermediary_must_pass_through_in_full() {
    let m = setup();
    // B receives 100 real estate from A but forwards only 60 to C.
    let legs = vec![
        &m.env,
        leg(&m.a, &m.b, &m.real_estate, 100),
        leg(&m.b, &m.c, &m.real_estate, 60),
        leg(&m.c, &m.a, &m.usdc, 1_000),
    ];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&legs),
        Err(Ok(ce(Error::ConservationViolation)))
    );
}

#[test]
fn pass_through_intermediary_needs_no_prefunding() {
    let m = setup();
    // B holds no real estate; it receives A's 100 and forwards all of it.
    // The engine must run A -> B before B -> C regardless of input order.
    let legs = vec![
        &m.env,
        leg(&m.b, &m.c, &m.real_estate, 100),
        leg(&m.c, &m.a, &m.usdc, 1_000),
        leg(&m.a, &m.b, &m.real_estate, 100),
    ];
    m.engine.execute_multilateral_settlement(&legs);
    assert_eq!(balance(&m, &m.real_estate, &m.b), 0);
    assert_eq!(balance(&m, &m.real_estate, &m.c), 100);
    assert_eq!(balance(&m, &m.usdc, &m.a), 1_000);
}

#[test]
fn non_compliant_recipient_reverts_whole_batch() {
    let m = setup();
    let before = snapshot(&m);
    // C loses KYC: the bond leg into C must fail, and so must A's and C's legs.
    m.compliance.suspend(&m.admin, &m.c);
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&ring(&m)),
        Err(Ok(ce(Error::RecipientNotCompliant)))
    );
    assert_eq!(snapshot(&m), before);
}

#[test]
fn non_compliant_sender_is_rejected() {
    let m = setup();
    m.compliance.suspend(&m.admin, &m.a);
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&ring(&m)),
        Err(Ok(ce(Error::SenderNotCompliant)))
    );
}

#[test]
fn unregulated_cash_leg_skips_compliance() {
    let m = setup();
    // An un-KYC'd market maker may take USDC but not the regulated tokens.
    let mm = Address::generate(&m.env);
    StellarAssetClient::new(&m.env, &m.usdc).mint(&mm, &500);
    let legs = vec![
        &m.env,
        leg(&m.c, &mm, &m.usdc, 100),
        leg(&mm, &m.c, &m.usdc, 100),
    ];
    m.engine.execute_multilateral_settlement(&legs);

    let legs = vec![
        &m.env,
        leg(&m.a, &mm, &m.real_estate, 1),
        leg(&mm, &m.a, &m.usdc, 10),
    ];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&legs),
        Err(Ok(ce(Error::RecipientNotCompliant)))
    );
}

#[test]
fn insufficient_balance_reverts_whole_batch() {
    let m = setup();
    let before = snapshot(&m);
    let legs = vec![
        &m.env,
        leg(&m.a, &m.b, &m.real_estate, 100),
        leg(&m.b, &m.c, &m.bond, 301), // B only holds 300
        leg(&m.c, &m.a, &m.usdc, 2_000),
    ];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&legs),
        Err(Ok(ce(Error::InsufficientBalance)))
    );
    assert_eq!(snapshot(&m), before);
}

#[test]
fn token_failure_mid_execution_rolls_back_earlier_legs() {
    let m = setup();
    let before = snapshot(&m);
    // Pausing the bond token is invisible to the engine's pre-checks, so the
    // real-estate leg executes before the bond transfer panics. The whole
    // batch must still revert.
    AssetTokenContractClient::new(&m.env, &m.bond).pause(&m.admin);
    let legs = vec![
        &m.env,
        leg(&m.a, &m.b, &m.real_estate, 100),
        leg(&m.b, &m.c, &m.bond, 50),
        leg(&m.c, &m.a, &m.usdc, 2_000),
    ];
    assert!(m.engine.try_execute_multilateral_settlement(&legs).is_err());
    assert_eq!(snapshot(&m), before);
    assert_eq!(m.engine.batch_count(), 0);
}

#[test]
fn shape_errors() {
    let m = setup();
    let empty = soroban_sdk::Vec::<TradeLeg>::new(&m.env);
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&empty),
        Err(Ok(ce(Error::InvalidBatchSize)))
    );

    let mut too_many = soroban_sdk::Vec::new(&m.env);
    for _ in 0..17 {
        too_many.push_back(leg(&m.c, &m.a, &m.usdc, 1));
        too_many.push_back(leg(&m.a, &m.c, &m.usdc, 1));
    }
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&too_many),
        Err(Ok(ce(Error::InvalidBatchSize)))
    );

    let zero = vec![&m.env, leg(&m.a, &m.b, &m.real_estate, 0)];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&zero),
        Err(Ok(ce(Error::InvalidAmount)))
    );

    let selfie = vec![&m.env, leg(&m.a, &m.a, &m.real_estate, 1)];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&selfie),
        Err(Ok(ce(Error::SelfTransfer)))
    );

    let unknown = m
        .env
        .register_stellar_asset_contract_v2(m.admin.clone())
        .address();
    let legs = vec![
        &m.env,
        leg(&m.a, &m.b, &unknown, 1),
        leg(&m.b, &m.a, &unknown, 1),
    ];
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&legs),
        Err(Ok(ce(Error::AssetNotRegistered)))
    );
}

#[test]
fn validate_settlement_moves_nothing() {
    let m = setup();
    let before = snapshot(&m);
    m.engine.validate_settlement(&ring(&m));
    assert_eq!(snapshot(&m), before);
    assert_eq!(m.engine.batch_count(), 0);
}

#[test]
fn paused_engine_rejects_batches() {
    let m = setup();
    m.engine.pause(&m.admin);
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&ring(&m)),
        Err(Ok(ce(Error::Paused)))
    );
    m.engine.unpause(&m.admin);
    m.engine.execute_multilateral_settlement(&ring(&m));
}

#[test]
fn deregistered_asset_fails_closed() {
    let m = setup();
    m.engine.deregister_asset(&m.admin, &m.bond);
    assert_eq!(
        m.engine.try_execute_multilateral_settlement(&ring(&m)),
        Err(Ok(ce(Error::AssetNotRegistered)))
    );
}

#[test]
fn each_sender_signs_only_the_batch_and_its_own_transfer() {
    let m = setup();
    let legs = ring(&m);
    let transfer = |token: &Address, from: &Address, to: &Address, amount: i128| {
        let args: soroban_sdk::Vec<soroban_sdk::Val> =
            (from.clone(), to.clone(), amount).into_val(&m.env);
        (token.clone(), args)
    };
    let subs = [
        transfer(&m.real_estate, &m.a, &m.b, 100),
        transfer(&m.bond, &m.b, &m.c, 50),
        transfer(&m.usdc, &m.c, &m.a, 2_000),
    ];
    let sub_invokes: std::vec::Vec<MockAuthInvoke> = subs
        .iter()
        .map(|(token, args)| MockAuthInvoke {
            contract: token,
            fn_name: "transfer",
            args: args.clone(),
            sub_invokes: &[],
        })
        .collect();
    let roots: std::vec::Vec<MockAuthInvoke> = sub_invokes
        .iter()
        .map(|sub| MockAuthInvoke {
            contract: &m.engine.address,
            fn_name: "execute_multilateral_settlement",
            args: (legs.clone(),).into_val(&m.env),
            sub_invokes: core::slice::from_ref(sub),
        })
        .collect();
    let auths = [
        MockAuth {
            address: &m.a,
            invoke: &roots[0],
        },
        MockAuth {
            address: &m.b,
            invoke: &roots[1],
        },
        MockAuth {
            address: &m.c,
            invoke: &roots[2],
        },
    ];
    m.engine
        .mock_auths(&auths)
        .execute_multilateral_settlement(&legs);
    assert_eq!(balance(&m, &m.usdc, &m.a), 2_000);
    assert_eq!(m.engine.batch_count(), 1);
}
