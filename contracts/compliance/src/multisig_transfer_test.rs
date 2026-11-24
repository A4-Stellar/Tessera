//! Issue #478: 3-of-5 board approval, settled on the real asset token.

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{vec, Address, Env, String, Vec};
use tessera_asset_token::{AssetTokenContract, AssetTokenContractClient};

use crate::multisig_transfer::DEFAULT_EXPIRY_LEDGERS;
use crate::{ComplianceContract, ComplianceContractClient, Error};

const SUPPLY: i128 = 1_000_000;
const THRESHOLD: i128 = 10_000;

struct Setup<'a> {
    env: Env,
    admin: Address,
    compliance: ComplianceContractClient<'a>,
    token: AssetTokenContractClient<'a>,
    holder: Address,
    buyer: Address,
    board: Vec<Address>,
}

fn setup<'a>() -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_sequence_number(1_000);

    let compliance_id = env.register(ComplianceContract, ());
    let compliance = ComplianceContractClient::new(&env, &compliance_id);
    let admin = Address::generate(&env);
    compliance.initialize(&admin);

    let holder = Address::generate(&env);
    let buyer = Address::generate(&env);
    let us = String::from_str(&env, "US");
    for who in [&admin, &holder, &buyer] {
        compliance.add_to_allowlist(&admin, who, &us, &0);
    }

    let token_id = env.register(AssetTokenContract, ());
    let token = AssetTokenContractClient::new(&env, &token_id);
    token.initialize(
        &admin,
        &String::from_str(&env, "Atlas Residences"),
        &String::from_str(&env, "ATLS"),
        &String::from_str(&env, "real_estate"),
        &SUPPLY,
        &7,
        &compliance_id,
        &String::from_str(&env, "Board-governed building"),
        &5_000_000,
    );
    token.transfer(&admin, &holder, &500_000);

    let board: Vec<Address> = Vec::from_array(
        &env,
        core::array::from_fn::<_, 5, _>(|_| Address::generate(&env)),
    );
    compliance.set_multisig_policy(&admin, &token_id, &THRESHOLD, &3, &board, &0);

    Setup {
        env,
        admin,
        compliance,
        token,
        holder,
        buyer,
        board,
    }
}

#[test]
fn three_of_five_needs_the_third_signature() {
    let s = setup();
    let amount = 25_000;
    let id = s.compliance.request_transfer(&s.holder, &s.buyer, &amount);

    // Two signatures: not enough. Nothing moves and the request stays pending.
    assert!(!s.compliance.co_sign(&s.board.get(0).unwrap(), &id));
    assert!(!s.compliance.co_sign(&s.board.get(3).unwrap(), &id));
    assert_eq!(s.token.balance(&s.buyer), 0);
    assert_eq!(s.token.balance(&s.holder), 500_000);
    let pending = s.compliance.get_pending_transfer(&id).unwrap();
    assert_eq!(pending.signers.len(), 2);
    assert_eq!(pending.required_signatures, 3);

    // Third signature: the compliance contract settles on the asset token.
    assert!(s.compliance.co_sign(&s.board.get(4).unwrap(), &id));
    assert_eq!(s.token.balance(&s.buyer), amount);
    assert_eq!(s.token.balance(&s.holder), 500_000 - amount);
    assert!(s.compliance.get_pending_transfer(&id).is_none());
}

#[test]
fn ordinary_transfers_above_the_threshold_are_refused() {
    let s = setup();
    let err = s
        .token
        .try_transfer(&s.holder, &s.buyer, &(THRESHOLD + 1))
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        tessera_asset_token::Error::BoardApprovalRequired.into()
    );
    // At or below the threshold nothing changes.
    s.token.transfer(&s.holder, &s.buyer, &THRESHOLD);
    assert_eq!(s.token.balance(&s.buyer), THRESHOLD);
    assert!(!s
        .compliance
        .requires_board_approval(&s.token.address, &THRESHOLD));
    assert!(s
        .compliance
        .requires_board_approval(&s.token.address, &(THRESHOLD + 1)));
}

#[test]
fn only_the_token_s_compliance_contract_may_call_board_transfer() {
    let s = setup();
    // With real auth checks, the holder's or the admin's signature does not
    // satisfy `compliance.require_auth()` inside `board_transfer`.
    s.env.set_auths(&[]);
    assert!(s
        .token
        .try_board_transfer(&s.holder, &s.buyer, &50_000)
        .is_err());
    assert_eq!(s.token.balance(&s.buyer), 0);
}

#[test]
fn rejects_outsiders_duplicates_and_small_requests() {
    let s = setup();
    let id = s.compliance.request_transfer(&s.holder, &s.buyer, &20_000);

    let outsider = Address::generate(&s.env);
    assert_eq!(
        s.compliance
            .try_co_sign(&outsider, &id)
            .unwrap_err()
            .unwrap(),
        Error::NotBoardSigner.into()
    );

    let first = s.board.get(1).unwrap();
    s.compliance.co_sign(&first, &id);
    assert_eq!(
        s.compliance.try_co_sign(&first, &id).unwrap_err().unwrap(),
        Error::AlreadySigned.into()
    );

    assert_eq!(
        s.compliance
            .try_request_transfer(&s.holder, &s.buyer, &THRESHOLD)
            .unwrap_err()
            .unwrap(),
        Error::BelowMultisigThreshold.into()
    );
    assert_eq!(
        s.compliance.try_co_sign(&first, &999).unwrap_err().unwrap(),
        Error::PendingTransferNotFound.into()
    );
}

#[test]
fn pending_transfers_expire_after_24_hours() {
    let s = setup();
    let id = s.compliance.request_transfer(&s.holder, &s.buyer, &30_000);
    let pending = s.compliance.get_pending_transfer(&id).unwrap();
    assert_eq!(
        pending.expires_at,
        pending.created_at + DEFAULT_EXPIRY_LEDGERS
    );
    s.compliance.co_sign(&s.board.get(0).unwrap(), &id);
    s.compliance.co_sign(&s.board.get(1).unwrap(), &id);

    s.env.ledger().set_sequence_number(pending.expires_at + 1);
    assert!(s.compliance.get_pending_transfer(&id).is_none());
    assert!(s
        .compliance
        .try_co_sign(&s.board.get(2).unwrap(), &id)
        .is_err());
    assert_eq!(s.token.balance(&s.buyer), 0);
}

#[test]
fn a_party_removed_from_the_allowlist_blocks_execution() {
    let s = setup();
    let id = s.compliance.request_transfer(&s.holder, &s.buyer, &40_000);
    s.compliance.co_sign(&s.board.get(0).unwrap(), &id);
    s.compliance.co_sign(&s.board.get(1).unwrap(), &id);
    s.compliance.suspend(&s.admin, &s.buyer);

    assert_eq!(
        s.compliance
            .try_co_sign(&s.board.get(2).unwrap(), &id)
            .unwrap_err()
            .unwrap(),
        Error::MultisigTransferNotCompliant.into()
    );
    assert_eq!(s.token.balance(&s.buyer), 0);
}

#[test]
fn changing_the_policy_invalidates_pending_transfers() {
    let s = setup();
    let id = s.compliance.request_transfer(&s.holder, &s.buyer, &40_000);
    let smaller = vec![&s.env, s.board.get(0).unwrap(), s.board.get(1).unwrap()];
    s.compliance
        .set_multisig_policy(&s.admin, &s.token.address, &THRESHOLD, &2, &smaller, &0);
    assert_eq!(
        s.compliance
            .try_co_sign(&s.board.get(0).unwrap(), &id)
            .unwrap_err()
            .unwrap(),
        Error::MultisigPolicyChanged.into()
    );
}

#[test]
fn invalid_policies_are_rejected() {
    let s = setup();
    let two = vec![&s.env, s.board.get(0).unwrap(), s.board.get(1).unwrap()];
    let dup = vec![&s.env, s.board.get(0).unwrap(), s.board.get(0).unwrap()];
    let empty: Vec<Address> = Vec::new(&s.env);
    let t = &s.token.address;
    for (threshold, m, board) in [
        (THRESHOLD, 3, &two),
        (THRESHOLD, 0, &two),
        (0, 1, &two),
        (THRESHOLD, 1, &dup),
        (THRESHOLD, 1, &empty),
    ] {
        assert_eq!(
            s.compliance
                .try_set_multisig_policy(&s.admin, t, &threshold, &m, board, &0)
                .unwrap_err()
                .unwrap(),
            Error::MultisigPolicyInvalid.into()
        );
    }
}
