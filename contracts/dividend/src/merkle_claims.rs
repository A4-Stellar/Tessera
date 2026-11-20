use soroban_sdk::{
    contracttype, panic_with_error,
    xdr::{AccountId, PublicKey, ScAddress, ScVal, Uint256},
    Address, Bytes, BytesN, Env, Vec,
};

use crate::{DataKey, Distribution, Error};

const LEAF_DOMAIN: &[u8; 24] = b"tessera-dividend-leaf-v1";
const NODE_DOMAIN: &[u8; 24] = b"tessera-dividend-node-v1";

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootSlot {
    Current,
    Previous,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootMeta {
    pub distribution_id: u64,
    pub root: BytesN<32>,
    pub leaf_count: u32,
    pub epoch: u32,
    pub valid_until: u32,
    pub total_entitlement: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimProof {
    pub distribution_id: u64,
    pub holder: Address,
    pub entitlement: i128,
    pub siblings: Vec<BytesN<32>>,
}

pub(crate) fn commit_root(
    env: &Env,
    distribution_id: u64,
    root: BytesN<32>,
    leaf_count: u32,
    valid_until: u32,
    total_entitlement: i128,
) {
    if leaf_count == 0 || total_entitlement <= 0 || root == BytesN::from_array(env, &[0u8; 32]) {
        panic_with_error!(env, Error::InvalidMerkleRoot);
    }
    if env
        .storage()
        .persistent()
        .get::<DataKey, Distribution>(&DataKey::Distribution(distribution_id))
        .is_none()
    {
        panic_with_error!(env, Error::DistributionNotFound);
    }

    let current: Option<RootMeta> = env
        .storage()
        .instance()
        .get(&DataKey::RootMeta(RootSlot::Current));
    let previous: Option<RootMeta> = env
        .storage()
        .instance()
        .get(&DataKey::RootMeta(RootSlot::Previous));
    if current.as_ref().is_some_and(|meta| meta.root == root)
        || previous.as_ref().is_some_and(|meta| meta.root == root)
    {
        panic_with_error!(env, Error::InvalidMerkleRoot);
    }

    let epoch = current.as_ref().map(|meta| meta.epoch + 1).unwrap_or(1);
    if let Some(meta) = current {
        env.storage()
            .instance()
            .set(&DataKey::RootMeta(RootSlot::Previous), &meta);
    }
    let meta = RootMeta {
        distribution_id,
        root: root.clone(),
        leaf_count,
        epoch,
        valid_until,
        total_entitlement,
    };
    env.storage()
        .instance()
        .set(&DataKey::RootMeta(RootSlot::Current), &meta);
    env.storage()
        .temporary()
        .set(&DataKey::DistributionRoot, &root);
    env.events().publish(
        (soroban_sdk::symbol_short!("mrklroot"), distribution_id),
        (root, leaf_count, epoch),
    );
}

pub(crate) fn current_root(env: &Env) -> Option<RootMeta> {
    env.storage()
        .instance()
        .get(&DataKey::RootMeta(RootSlot::Current))
}

pub(crate) fn previous_root(env: &Env) -> Option<RootMeta> {
    env.storage()
        .instance()
        .get(&DataKey::RootMeta(RootSlot::Previous))
}

pub(crate) fn sync_root_cache(env: &Env) {
    if let Some(meta) = current_root(env) {
        env.storage()
            .temporary()
            .set(&DataKey::DistributionRoot, &meta.root);
    }
}

pub(crate) fn claimed(env: &Env, distribution_id: u64, holder: &Address) -> bool {
    paid(env, distribution_id, holder) > 0
}

pub(crate) fn paid(env: &Env, distribution_id: u64, holder: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Paid(distribution_id, holder.clone()))
        .unwrap_or(0)
}

pub(crate) fn claim(
    env: &Env,
    proof: &ClaimProof,
    leaf_index: u32,
    transfer: impl FnOnce(&Address, i128),
) -> i128 {
    if holder_key(env, &proof.holder).is_none() {
        panic_with_error!(env, Error::InvalidClaimHolder);
    }
    let meta = [RootSlot::Current, RootSlot::Previous]
        .into_iter()
        .filter_map(|slot| {
            env.storage()
                .instance()
                .get::<DataKey, RootMeta>(&DataKey::RootMeta(slot))
        })
        .find(|meta| {
            meta.distribution_id == proof.distribution_id
                && env.ledger().sequence() <= meta.valid_until
                && verify(env, meta, proof, leaf_index)
        })
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidMerkleProof));

    let word_index = leaf_index / 256;
    let bit = leaf_index % 256;
    let key = DataKey::ClaimedWord(meta.epoch, word_index);
    let mut word: BytesN<32> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| BytesN::from_array(env, &[0u8; 32]));
    if bit_set(&word, bit) {
        panic_with_error!(env, Error::AlreadyClaimed);
    }

    let paid_key = DataKey::Paid(proof.distribution_id, proof.holder.clone());
    let already_paid: i128 = env.storage().persistent().get(&paid_key).unwrap_or(0);
    let due = proof.entitlement - already_paid;
    if due <= 0 {
        panic_with_error!(env, Error::NothingToClaim);
    }

    transfer(&proof.holder, due);

    set_bit(&mut word, bit);
    env.storage().persistent().set(&key, &word);
    env.storage()
        .persistent()
        .set(&paid_key, &(already_paid + due));
    env.events().publish(
        (soroban_sdk::symbol_short!("mrklclm"), proof.holder.clone()),
        (proof.distribution_id, leaf_index, due),
    );
    due
}

pub(crate) fn verify(env: &Env, meta: &RootMeta, proof: &ClaimProof, leaf_index: u32) -> bool {
    if leaf_index >= meta.leaf_count || proof.entitlement < 0 {
        return false;
    }
    let holder = match holder_key(env, &proof.holder) {
        Some(holder) => holder,
        None => return false,
    };
    if proof.siblings.len() != expected_proof_len(meta.leaf_count, leaf_index) {
        return false;
    }

    let mut current = leaf_hash(
        env,
        proof.distribution_id,
        leaf_index,
        &holder,
        proof.entitlement,
    );
    let mut index = leaf_index;
    let mut size = meta.leaf_count;
    let mut used = 0u32;
    while size > 1 {
        if index ^ 1 < size {
            let sibling = match proof.siblings.get(used) {
                Some(sibling) => sibling,
                None => return false,
            };
            current = if index % 2 == 0 {
                node_hash(env, &current, &sibling)
            } else {
                node_hash(env, &sibling, &current)
            };
            used += 1;
        }
        index /= 2;
        size = size.div_ceil(2);
    }
    used == proof.siblings.len() && current == meta.root
}

fn expected_proof_len(leaf_count: u32, mut index: u32) -> u32 {
    let mut size = leaf_count;
    let mut len = 0;
    while size > 1 {
        if index ^ 1 < size {
            len += 1;
        }
        index /= 2;
        size = size.div_ceil(2);
    }
    len
}

fn leaf_hash(
    env: &Env,
    distribution_id: u64,
    leaf_index: u32,
    holder: &BytesN<32>,
    entitlement: i128,
) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, LEAF_DOMAIN));
    preimage.append(&Bytes::from_array(env, &distribution_id.to_be_bytes()));
    preimage.append(&Bytes::from_array(env, &leaf_index.to_be_bytes()));
    preimage.append(&holder.clone().into());
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

fn holder_key(env: &Env, holder: &Address) -> Option<BytesN<32>> {
    match ScVal::from(holder.clone()) {
        ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
            Uint256(key),
        )))) => Some(BytesN::from_array(env, &key)),
        _ => None,
    }
}

fn bit_set(word: &BytesN<32>, bit: u32) -> bool {
    let value = word.get((bit / 8) as u32).unwrap();
    (value >> (bit % 8)) & 1 == 1
}

fn set_bit(word: &mut BytesN<32>, bit: u32) {
    let index = (bit / 8) as u32;
    let value = word.get(index).unwrap() | (1u8 << (bit % 8));
    word.set(index, value);
}
