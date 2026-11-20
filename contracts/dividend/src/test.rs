use crate::{ClaimProof, DividendContract, DividendContractClient, Error, RootSlot};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    xdr::{AccountId, PublicKey, ScAddress, ScVal, Uint256},
    Address, Bytes, BytesN, Env, IntoVal, Symbol, TryFromVal, Val, Vec,
};

fn account(env: &Env, seed: u8) -> Address {
    Address::try_from_val(
        env,
        &ScVal::Address(ScAddress::Account(AccountId(
            PublicKey::PublicKeyTypeEd25519(Uint256([seed; 32])),
        ))),
    )
    .unwrap()
}
use tessera_cap_table::{CapTableContract, CapTableContractClient};

fn err(error: Error) -> soroban_sdk::Error {
    error.into()
}

#[derive(Clone)]
#[contracttype]
enum TokenKey {
    Supply,
    Balance(Address),
}

#[contract]
struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn initialize(env: Env, owner: Address, supply: i128, balance: i128) {
        env.storage().instance().set(&TokenKey::Supply, &supply);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(owner), &balance);
    }

    pub fn balance(env: Env, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TokenKey::Balance(holder))
            .unwrap_or(0)
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage().instance().get(&TokenKey::Supply).unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        let to_balance = Self::balance(env.clone(), to.clone());
        assert!(amount > 0 && from_balance >= amount);
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(from), &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(to_balance + amount));
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&TokenKey::Balance(to), &(balance + amount));
    }
}

#[contract]
struct MockAmm;

#[contractimpl]
impl MockAmm {
    pub fn swap_exact_in(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        payer: Address,
        recipient: Address,
    ) -> i128 {
        let amm = env.current_contract_address();
        let args: Vec<Val> = (payer, amm, amount_in).into_val(&env);
        let _: () = env.invoke_contract(&token_in, &Symbol::new(&env, "transfer"), args);

        let amount_out = amount_in * 2;
        assert!(amount_out >= min_amount_out);
        let args: Vec<Val> = (recipient, amount_out).into_val(&env);
        let _: () = env.invoke_contract(&token_out, &Symbol::new(&env, "mint"), args);
        amount_out
    }
}

#[test]
fn opted_in_claim_swaps_and_records_acquisition() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let stable = env.register(MockToken, ());
    let asset = env.register(MockToken, ());
    let amm = env.register(MockAmm, ());
    let cap_table_id = env.register(CapTableContract, ());
    let cap_table = CapTableContractClient::new(&env, &cap_table_id);

    MockTokenClient::new(&env, &stable).initialize(&admin, &10_000, &10_000);
    MockTokenClient::new(&env, &asset).initialize(&holder, &1_000, &1_000);

    let dividend_id = env.register(DividendContract, ());
    let dividend = DividendContractClient::new(&env, &dividend_id);
    dividend.initialize(&admin);
    cap_table.initialize(&admin);
    cap_table.set_drip_registrar(&admin, &dividend_id);
    dividend.configure_drip(&admin, &amm, &cap_table_id);
    let distribution_id = dividend.create_distribution(&admin, &asset, &stable, &100);
    dividend.set_drip_preference(&holder, &true);

    dividend.claim_legacy(&distribution_id, &holder);

    assert_eq!(MockTokenClient::new(&env, &asset).balance(&holder), 1_200);
    assert_eq!(cap_table.drip_acquisitions(&asset, &holder), 200);
    assert!(dividend.has_claimed(&distribution_id, &holder));
    assert_eq!(dividend.get_distribution(&distribution_id).distributed, 100);
}

#[test]
fn preference_is_opt_in_by_default_and_can_be_disabled() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let admin = Address::generate(&env);
    let holder = Address::generate(&env);
    let contract = env.register(DividendContract, ());
    let client = DividendContractClient::new(&env, &contract);
    client.initialize(&admin);

    assert!(!client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &true);
    assert!(client.get_drip_preference(&holder));
    client.set_drip_preference(&holder, &false);
    assert!(!client.get_drip_preference(&holder));
}

const LEAF_DOMAIN: &[u8; 24] = b"tessera-dividend-leaf-v1";
const NODE_DOMAIN: &[u8; 24] = b"tessera-dividend-node-v1";

fn holder_key(env: &Env, holder: &Address) -> BytesN<32> {
    match ScVal::from(holder.clone()) {
        ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
            Uint256(key),
        )))) => BytesN::from_array(env, &key),
        _ => panic!("test holder must be an account address"),
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
) -> (BytesN<32>, Vec<Vec<BytesN<32>>>) {
    let mut levels: Vec<Vec<BytesN<32>>> = Vec::new(env);
    let mut level = Vec::new(env);
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
        let mut next = Vec::new(env);
        let mut i = 0;
        while i < level.len() {
            if i + 1 < level.len() {
                next.push_back(node_hash(
                    env,
                    &level.get(i).unwrap(),
                    &level.get(i + 1).unwrap(),
                ));
            } else {
                next.push_back(level.get(i).unwrap());
            }
            i += 2;
        }
        level = next;
        levels.push_back(level.clone());
    }

    let mut proofs: Vec<Vec<BytesN<32>>> = Vec::new(env);
    for leaf_index in 0..leaves.len() as u32 {
        let mut siblings = Vec::new(env);
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
        proofs.push_back(siblings);
    }

    let root = levels.get(levels.len() - 1).unwrap().get(0).unwrap();
    (root, proofs)
}

fn setup_distribution(total: i128) -> (Env, Address, Address, Address, Address, u64) {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    let admin = Address::generate(&env);
    let h1 = account(&env, 1);
    let h2 = account(&env, 2);
    let asset = env.register(MockToken, ());
    let stable = env.register(MockToken, ());
    MockTokenClient::new(&env, &stable).initialize(&admin, &1_000_000, &1_000_000);
    MockTokenClient::new(&env, &asset).initialize(&h1, &1_000, &1_000);
    let dividend_id = env.register(DividendContract, ());
    let dividend = DividendContractClient::new(&env, &dividend_id);
    dividend.initialize(&admin);
    let distribution_id = dividend.create_distribution(&admin, &asset, &stable, &total);
    (env, admin, h1, h2, dividend_id, distribution_id)
}

#[test]
fn merkle_claim_pays_once_and_tracks_paid_deltas() {
    let (env, admin, h1, h2, dividend_id, id) = setup_distribution(200);
    let dividend = DividendContractClient::new(&env, &dividend_id);
    let leaves = [(h1.clone(), 120i128), (h2.clone(), 80i128)];
    let (root, proofs) = merkle_tree(&env, id, &leaves);

    dividend.set_root(&admin, &id, &root, &2, &1000, &200);
    assert_eq!(dividend.get_root(), root);
    assert_eq!(dividend.get_root_cache(), root);
    assert_eq!(
        dividend
            .get_root_meta(&RootSlot::Current)
            .unwrap()
            .leaf_count,
        2
    );

    let proof = ClaimProof {
        distribution_id: id,
        holder: h1.clone(),
        entitlement: 120,
        siblings: proofs.get(0).unwrap(),
    };
    assert_eq!(dividend.claim(&proof, &0), 120);
    assert!(dividend.merkle_claimed(&id, &h1));
    assert_eq!(dividend.merkle_paid(&id, &h1), 120);
    assert_eq!(dividend.get_distribution(&id).distributed, 120);
    assert_eq!(
        dividend.try_claim(&proof, &0),
        Err(Ok(err(Error::AlreadyClaimed)))
    );

    let proof2 = ClaimProof {
        distribution_id: id,
        holder: h2.clone(),
        entitlement: 80,
        siblings: proofs.get(1).unwrap(),
    };
    assert_eq!(dividend.claim(&proof2, &1), 80);
    assert!(dividend.get_distribution(&id).completed);
    assert_eq!(dividend.get_distribution(&id).distributed, 200);
}

#[test]
fn merkle_root_rotation_keeps_previous_root_claimable() {
    let (env, admin, h1, h2, dividend_id, id) = setup_distribution(250);
    let dividend = DividendContractClient::new(&env, &dividend_id);
    let leaves_a = [(h1.clone(), 100i128), (h2.clone(), 100i128)];
    let leaves_b = [(h1.clone(), 150i128), (h2.clone(), 50i128)];
    let (root_a, proofs_a) = merkle_tree(&env, id, &leaves_a);
    let (root_b, proofs_b) = merkle_tree(&env, id, &leaves_b);

    dividend.set_root(&admin, &id, &root_a, &2, &1000, &200);
    dividend.rotate_root(&admin, &id, &root_b, &2, &2000, &200);
    assert_eq!(
        dividend.get_root_meta(&RootSlot::Previous).unwrap().root,
        root_a
    );
    assert_eq!(dividend.get_root(), root_b);

    let old_h1 = ClaimProof {
        distribution_id: id,
        holder: h1.clone(),
        entitlement: 100,
        siblings: proofs_a.get(0).unwrap(),
    };
    let old_h2 = ClaimProof {
        distribution_id: id,
        holder: h2.clone(),
        entitlement: 100,
        siblings: proofs_a.get(1).unwrap(),
    };
    let new_h1 = ClaimProof {
        distribution_id: id,
        holder: h1.clone(),
        entitlement: 150,
        siblings: proofs_b.get(0).unwrap(),
    };

    assert_eq!(dividend.claim(&old_h1, &0), 100);
    assert_eq!(dividend.claim(&old_h2, &1), 100);
    assert_eq!(dividend.claim(&new_h1, &0), 50);
    assert_eq!(dividend.merkle_paid(&id, &h1), 150);
    assert_eq!(dividend.merkle_paid(&id, &h2), 100);
    assert_eq!(dividend.get_distribution(&id).distributed, 250);
}

#[test]
fn merkle_claim_rejects_invalid_proofs_and_roots() {
    let (env, admin, h1, h2, dividend_id, id) = setup_distribution(100);
    let dividend = DividendContractClient::new(&env, &dividend_id);
    let leaves = [(h1.clone(), 100i128), (h2.clone(), 80i128)];
    let (root, proofs) = merkle_tree(&env, id, &leaves);
    dividend.set_root(&admin, &id, &root, &2, &1000, &180);

    let bad_index = ClaimProof {
        distribution_id: id,
        holder: h1.clone(),
        entitlement: 100,
        siblings: proofs.get(0).unwrap(),
    };
    assert_eq!(
        dividend.try_claim(&bad_index, &1),
        Err(Ok(err(Error::InvalidMerkleProof)))
    );

    let bad_amount = ClaimProof {
        distribution_id: id,
        holder: h1.clone(),
        entitlement: 101,
        siblings: proofs.get(0).unwrap(),
    };
    assert_eq!(
        dividend.try_claim(&bad_amount, &0),
        Err(Ok(err(Error::InvalidMerkleProof)))
    );

    let contract_holder = ClaimProof {
        distribution_id: id,
        holder: env.register(MockToken, ()),
        entitlement: 100,
        siblings: proofs.get(0).unwrap(),
    };
    assert_eq!(
        dividend.try_claim(&contract_holder, &0),
        Err(Ok(err(Error::InvalidClaimHolder)))
    );

    assert_eq!(
        dividend.try_set_root(
            &admin,
            &id,
            &BytesN::from_array(&env, &[0u8; 32]),
            &2,
            &1000,
            &180,
        ),
        Err(Ok(err(Error::InvalidMerkleRoot)))
    );
    assert_eq!(
        dividend.try_set_root(&admin, &id, &root, &2, &1000, &180),
        Err(Ok(err(Error::InvalidMerkleRoot)))
    );
}

#[test]
fn merkle_golden_vector_matches_offchain_generator() {
    let env = Env::default();
    let h1 = account(&env, 1);
    let h2 = account(&env, 2);
    let (root, _) = merkle_tree(&env, 0, &[(h1, 120i128), (h2, 80i128)]);
    assert_eq!(
        root,
        BytesN::from_array(
            &env,
            &[
                0xd4, 0x24, 0x9b, 0xc0, 0x24, 0xe1, 0x93, 0x72, 0x78, 0x70, 0xf9, 0xf2, 0xa8, 0xf2,
                0xbf, 0x6b, 0xb3, 0x24, 0x78, 0xce, 0xf8, 0x43, 0x31, 0xb5, 0x18, 0xa4, 0xb5, 0x76,
                0x05, 0x2c, 0xc0, 0xcf,
            ],
        )
    );
}

#[test]
fn merkle_claims_handle_non_power_of_two_leaf_counts() {
    let (env, admin, h1, h2, dividend_id, id) = setup_distribution(300);
    let dividend = DividendContractClient::new(&env, &dividend_id);
    let h3 = account(&env, 3);
    let leaves = [(h1, 100i128), (h2, 100i128), (h3, 100i128)];
    let (root, proofs) = merkle_tree(&env, id, &leaves);
    dividend.set_root(&admin, &id, &root, &3, &1000, &300);

    for (index, (holder, entitlement)) in leaves.iter().enumerate() {
        let due = dividend.claim(
            &ClaimProof {
                distribution_id: id,
                holder: holder.clone(),
                entitlement: *entitlement,
                siblings: proofs.get(u32::try_from(index).unwrap()).unwrap(),
            },
            &u32::try_from(index).unwrap(),
        );
        assert_eq!(due, *entitlement);
    }
    assert_eq!(dividend.get_distribution(&id).distributed, 300);
}
