use soroban_sdk::{contracttype, panic_with_error, xdr::ToXdr, Bytes, BytesN, Env, Vec};

use crate::{AssetEntry, DataKey, Error};

const DEPTH: usize = 16;
const BRANCH_BYTES: usize = 512;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MptProof {
    pub root: BytesN<32>,
    pub asset_id: u64,
    pub path: BytesN<8>,
    pub branches: Vec<BytesN<512>>,
    pub leaf: AssetEntry,
}

pub(crate) fn zero_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0u8; 32])
}

fn zero_branch(env: &Env) -> BytesN<512> {
    BytesN::from_array(env, &[0u8; BRANCH_BYTES])
}

pub(crate) fn root(env: &Env) -> BytesN<32> {
    env.storage()
        .instance()
        .get(&DataKey::MptRoot)
        .unwrap_or_else(|| zero_hash(env))
}

pub(crate) fn sync_root_cache(env: &Env) {
    env.storage()
        .temporary()
        .set(&DataKey::MptRootCache, &root(env));
}

pub(crate) fn cached_root(env: &Env) -> BytesN<32> {
    env.storage()
        .temporary()
        .get(&DataKey::MptRootCache)
        .unwrap_or_else(|| root(env))
}

fn key_path(env: &Env, asset_id: u64) -> BytesN<8> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, b"tessera-registry-path-v1"));
    preimage.append(&Bytes::from_array(env, &asset_id.to_be_bytes()));
    let digest: Bytes = env.crypto().sha256(&preimage).into();
    BytesN::from_array(
        env,
        &[
            digest.get(0).unwrap(),
            digest.get(1).unwrap(),
            digest.get(2).unwrap(),
            digest.get(3).unwrap(),
            digest.get(4).unwrap(),
            digest.get(5).unwrap(),
            digest.get(6).unwrap(),
            digest.get(7).unwrap(),
        ],
    )
}

fn nibble_at(path: &BytesN<8>, level: usize) -> usize {
    let byte = path.get((level / 2) as u32).unwrap();
    if level % 2 == 0 {
        (byte >> 4) as usize
    } else {
        (byte & 0x0f) as usize
    }
}

fn leaf_hash(env: &Env, entry: &AssetEntry) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, b"tessera-registry-leaf-v1"));
    preimage.append(&entry.clone().to_xdr(env));
    env.crypto().sha256(&preimage).into()
}

fn branch_hash(env: &Env, node: &BytesN<512>) -> BytesN<32> {
    let mut preimage = Bytes::new(env);
    preimage.append(&Bytes::from_array(env, b"tessera-registry-branch-v1"));
    preimage.append(&node.clone().into());
    env.crypto().sha256(&preimage).into()
}

fn read_child(env: &Env, node: &BytesN<512>, index: usize) -> BytesN<32> {
    let mut child = [0u8; 32];
    for (i, slot) in child.iter_mut().enumerate() {
        *slot = node.get((index * 32 + i) as u32).unwrap();
    }
    BytesN::from_array(env, &child)
}

fn write_child(node: &mut BytesN<512>, index: usize, child: &BytesN<32>) {
    for i in 0..32 {
        node.set((index * 32 + i) as u32, child.get(i as u32).unwrap());
    }
}

fn insert_at(
    env: &Env,
    level: usize,
    path: &BytesN<8>,
    leaf: &BytesN<32>,
    node_hash: &BytesN<32>,
) -> BytesN<32> {
    let mut node: BytesN<512> = if *node_hash == zero_hash(env) {
        zero_branch(env)
    } else {
        env.storage()
            .persistent()
            .get(&DataKey::MptNode(node_hash.clone()))
            .unwrap_or_else(|| zero_branch(env))
    };

    let index = nibble_at(path, level);
    let child_hash = read_child(env, &node, index);
    let next = if level + 1 == DEPTH {
        leaf.clone()
    } else {
        insert_at(env, level + 1, path, leaf, &child_hash)
    };
    write_child(&mut node, index, &next);

    let hash = branch_hash(env, &node);
    env.storage()
        .persistent()
        .set(&DataKey::MptNode(hash.clone()), &node);
    hash
}

pub(crate) fn insert(env: &Env, entry: &AssetEntry) {
    let path = key_path(env, entry.id);
    let leaf = leaf_hash(env, entry);
    let next = insert_at(env, 0, &path, &leaf, &root(env));
    env.storage().instance().set(&DataKey::MptRoot, &next);
    sync_root_cache(env);
}

pub(crate) fn proof(env: &Env, asset_id: u64, leaf: &AssetEntry) -> MptProof {
    let path = key_path(env, asset_id);
    let root = root(env);
    let mut node_hash = root.clone();
    let mut branches = Vec::new(env);
    for level in 0..DEPTH {
        let node: BytesN<512> = env
            .storage()
            .persistent()
            .get(&DataKey::MptNode(node_hash.clone()))
            .unwrap_or_else(|| zero_branch(env));
        branches.push_back(node.clone());
        node_hash = read_child(env, &node, nibble_at(&path, level));
    }
    MptProof {
        root,
        asset_id,
        path,
        branches,
        leaf: leaf.clone(),
    }
}

pub(crate) fn verify(env: &Env, proof: &MptProof) -> bool {
    if proof.root != root(env)
        || proof.branches.len() != DEPTH as u32
        || proof.leaf.id != proof.asset_id
    {
        return false;
    }

    let expected_leaf = leaf_hash(env, &proof.leaf);
    let mut expected = proof.root.clone();
    for (level, node) in proof.branches.iter().enumerate() {
        if branch_hash(env, &node) != expected {
            return false;
        }
        expected = read_child(env, &node, nibble_at(&proof.path, level));
    }
    expected == expected_leaf
}

pub(crate) fn assert_valid_proof(env: &Env, proof: &MptProof) {
    if !verify(env, proof) {
        panic_with_error!(env, Error::InvalidMptProof);
    }
}
