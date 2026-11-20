use proptest::prelude::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    xdr::{AccountId, PublicKey, ScAddress, ScVal, Uint256},
    Address, Bytes, BytesN, Env, TryFromVal,
};
use tessera_dividend::{ClaimProof, DividendContract, DividendContractClient};

const LEAF_DOMAIN: &[u8; 24] = b"tessera-dividend-leaf-v1";
const NODE_DOMAIN: &[u8; 24] = b"tessera-dividend-node-v1";

#[contract]
pub struct MockPaymentToken;

#[contractimpl]
impl MockPaymentToken {
    pub fn transfer(_env: Env, _from: Address, _to: Address, _amount: i128) {}
}

fn account(env: &Env, key: [u8; 32]) -> Address {
    Address::try_from_val(
        env,
        &ScVal::Address(ScAddress::Account(AccountId(
            PublicKey::PublicKeyTypeEd25519(Uint256(key)),
        ))),
    )
    .unwrap()
}

fn holder_key(env: &Env, holder: &Address) -> BytesN<32> {
    match ScVal::from(holder.clone()) {
        ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
            Uint256(key),
        )))) => BytesN::from_array(env, &key),
        _ => panic!("holder must be an account address"),
    }
}

fn leaf_hash(
    env: &Env,
    distribution_id: u64,
    leaf_index: u32,
    holder: &Address,
    entitlement: i128,
) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, LEAF_DOMAIN));
    preimage.append(&Bytes::from_array(env, &distribution_id.to_be_bytes()));
    preimage.append(&Bytes::from_array(env, &leaf_index.to_be_bytes()));
    preimage.append(&holder_key(env, holder).into());
    preimage.append(&Bytes::from_array(env, &entitlement.to_be_bytes()));
    env.crypto().sha256(&preimage).into()
}

fn node_hash(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, NODE_DOMAIN));
    preimage.append(&left.clone().into());
    preimage.append(&right.clone().into());
    env.crypto().sha256(&preimage).into()
}

fn merkle_tree(
    env: &Env,
    distribution_id: u64,
    leaves: &[(Address, i128)],
) -> (BytesN<32>, Vec<soroban_sdk::Vec<BytesN<32>>>) {
    let mut levels: soroban_sdk::Vec<soroban_sdk::Vec<BytesN<32>>> = soroban_sdk::Vec::new(env);
    let mut level = soroban_sdk::Vec::new(env);
    for (index, (holder, entitlement)) in leaves.iter().enumerate() {
        level.push_back(leaf_hash(
            env,
            distribution_id,
            index as u32,
            holder,
            *entitlement,
        ));
    }
    levels.push_back(level.clone());

    while level.len() > 1 {
        let mut next = soroban_sdk::Vec::new(env);
        let mut index = 0;
        while index < level.len() {
            if index + 1 < level.len() {
                next.push_back(node_hash(
                    env,
                    &level.get(index).unwrap(),
                    &level.get(index + 1).unwrap(),
                ));
            } else {
                next.push_back(level.get(index).unwrap());
            }
            index += 2;
        }
        level = next;
        levels.push_back(level.clone());
    }

    let mut proofs: Vec<soroban_sdk::Vec<BytesN<32>>> = Vec::new();
    for leaf_index in 0..leaves.len() as u32 {
        let mut siblings = soroban_sdk::Vec::new(env);
        let mut index = leaf_index;
        for level in levels
            .iter()
            .take(usize::try_from(levels.len() - 1).unwrap())
        {
            let sibling = index ^ 1;
            if sibling < level.len() {
                siblings.push_back(level.get(sibling).unwrap());
            }
            index /= 2;
        }
        proofs.push(siblings);
    }

    let root = levels.get(levels.len() - 1).unwrap().get(0).unwrap();
    (root, proofs)
}

proptest! {
    #[test]
    fn merkle_claims_pay_the_full_committed_total_once(
        count in 1usize..8,
        entitlements in prop::collection::vec(1i128..100, 1..8),
    ) {
        let count = count.min(entitlements.len());
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let payment_token = env.register(MockPaymentToken, ());
        let asset_token = env.register(MockPaymentToken, ());
        let dividend_id = env.register(DividendContract, ());
        let dividend = DividendContractClient::new(&env, &dividend_id);
        dividend.initialize(&admin);

        let mut leaves = Vec::new();
        for index in 0..count {
            leaves.push((account(&env, [u8::try_from(index + 1).unwrap(); 32]), entitlements[index]));
        }
        let total: i128 = entitlements[..count].iter().sum();
        let distribution_id = dividend.create_distribution(
            &admin,
            &asset_token,
            &payment_token,
            &total,
        );
        let (root, proofs) = merkle_tree(&env, distribution_id, &leaves);
        dividend.set_root(
            &admin,
            &distribution_id,
            &root,
            &u32::try_from(count).unwrap(),
            &1000,
            &total,
        );

        for index in 0..count {
            let holder = leaves[index].0.clone();
            let due = dividend.claim(
                &ClaimProof {
                    distribution_id,
                    holder: holder.clone(),
                    entitlement: leaves[index].1,
                    siblings: proofs[index].clone(),
                },
                &u32::try_from(index).unwrap(),
            );
            prop_assert_eq!(due, leaves[index].1);
            prop_assert_eq!(dividend.merkle_paid(&distribution_id, &holder), due);
            let replay = dividend.try_claim(
                &ClaimProof {
                    distribution_id,
                    holder,
                    entitlement: leaves[index].1,
                    siblings: proofs[index].clone(),
                },
                &u32::try_from(index).unwrap(),
            );
            prop_assert!(replay.is_err());
        }

        prop_assert_eq!(dividend.get_distribution(&distribution_id).distributed, total);
    }
}
