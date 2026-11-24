//! Regression tests for the fuzz-found admin-bypass / compile defects in
//! `debt-token` (issue #470).

use soroban_sdk::{testutils::Address as _, Address, Env, Error as SdkError};

use crate::{DebtTokenAmortization, DebtTokenAmortizationClient, Error, Tranche};

fn declared(error: Error) -> SdkError {
    error.into()
}

#[test]
fn only_the_admin_can_accrue_and_only_after_initialization() {
    let env = Env::default();
    let id = env.register(DebtTokenAmortization, ());
    let client = DebtTokenAmortizationClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    let stranger = Address::generate(&env);

    let before_init = declared(Error::NotInitialized);
    assert!(matches!(
        client.try_accrue_interest(&stranger, &Tranche::Senior, &100),
        Err(Ok(error)) if error == before_init
    ));

    client.initialize(&admin);

    let unauthorized = declared(Error::Unauthorized);
    assert!(matches!(
        client.try_accrue_interest(&stranger, &Tranche::Senior, &100),
        Err(Ok(error)) if error == unauthorized
    ));
    assert_eq!(client.senior_owed(), 0);

    client.accrue_interest(&admin, &Tranche::Senior, &100);
    client.accrue_interest(&admin, &Tranche::Mezzanine, &50);
    assert_eq!(client.senior_owed(), 100);
    assert_eq!(client.mezzanine_owed(), 50);
    assert_eq!(client.total_owed(), 150);
}

#[test]
fn overpayment_retires_the_ledger_without_an_underflow_trap() {
    let env = Env::default();
    let id = env.register(DebtTokenAmortization, ());
    let client = DebtTokenAmortizationClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    let borrower = Address::generate(&env);
    client.initialize(&admin);
    client.accrue_interest(&admin, &Tranche::Equity, &10);

    // The old uncapped equity leg underflowed on this exact call.
    client.deposit_repayment(&borrower, &1_000_000);
    assert_eq!(client.total_owed(), 0);
}

#[test]
fn non_positive_accrual_is_rejected() {
    let env = Env::default();
    let id = env.register(DebtTokenAmortization, ());
    let client = DebtTokenAmortizationClient::new(&env, &id).mock_all_auths();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let invalid = declared(Error::InvalidAmount);
    assert!(matches!(
        client.try_accrue_interest(&admin, &Tranche::Senior, &0),
        Err(Ok(error)) if error == invalid
    ));
}
