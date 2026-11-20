use ed25519_dalek::{Signature, Signer, SigningKey};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    xdr::{AccountId, PublicKey, ScAddress, ScVal, ToXdr, Uint256},
    Address, BytesN, Env, String, TryFromVal,
};
use tessera_asset_token::{permit::PermitPayload, AssetTokenContract, AssetTokenContractClient};

#[contract]
pub struct Gate;

#[contractimpl]
impl Gate {
    pub fn is_allowed(_env: Env, _who: Address) -> bool {
        true
    }
    pub fn check_velocity(_env: Env, _who: Address, _amount: i128, _jurisdiction: String) -> bool {
        true
    }
}

fn account_from_key(env: &Env, key: [u8; 32]) -> Address {
    Address::try_from_val(
        env,
        &ScVal::Address(ScAddress::Account(AccountId(
            PublicKey::PublicKeyTypeEd25519(Uint256(key)),
        ))),
    )
    .unwrap()
}

fn sign_permit(
    env: &Env,
    token: &AssetTokenContractClient<'_>,
    owner: &Address,
    spender: &Address,
    nonce: u64,
    amount: i128,
    deadline: u32,
    key: &[u8; 32],
) -> BytesN<64> {
    let payload = PermitPayload {
        version: 1,
        contract: token.address.clone(),
        network_id: env.ledger().network_id(),
        nonce,
        owner: owner.clone(),
        spender: spender.clone(),
        amount,
        deadline_ledger: deadline,
    };
    let digest = Sha256::digest(payload.to_xdr(env).to_alloc_vec());
    let signing_key = SigningKey::from_bytes(key);
    let signature: Signature = signing_key.sign(&digest);
    BytesN::from_array(env, &signature.to_bytes())
}

proptest! {
    #[test]
    fn permit_signatures_are_single_use_and_bound_to_payload(
        amount in 1i128..100,
        deadline in 11u32..1000,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|state| state.sequence_number = 10);
        let gate = env.register(Gate, ());
        let token = AssetTokenContractClient::new(&env, &env.register(AssetTokenContract, ()));
        let admin = Address::generate(&env);
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let owner = account_from_key(&env, signing_key.verifying_key().to_bytes());
        let spender = account_from_key(&env, [8u8; 32]);
        let recipient = Address::generate(&env);
        let label = String::from_str(&env, "Equity");
        token.initialize(&admin, &label, &label, &label, &1000, &7, &gate, &label, &0);
        token.mint(&admin, &owner, &500);

        let signature = sign_permit(
            &env, &token, &owner, &spender, 0, amount, deadline, &[7u8; 32],
        );
        token.permit(&owner, &spender, &amount, &deadline, &signature);
        prop_assert_eq!(token.allowance(&owner, &spender), amount);
        prop_assert!(token
            .try_permit(&owner, &spender, &amount, &deadline, &signature)
            .is_err());

        let next = sign_permit(
            &env, &token, &owner, &spender, 1, amount, deadline, &[7u8; 32],
        );
        token.permit(&owner, &spender, &amount, &deadline, &next);
        prop_assert_eq!(token.allowance(&owner, &spender), amount);

        token.transfer_from(&spender, &owner, &recipient, &amount);
        prop_assert_eq!(token.allowance(&owner, &spender), 0);
        prop_assert_eq!(token.balance(&owner), 500 - amount);
        prop_assert_eq!(token.balance(&recipient), amount);
    }
}
