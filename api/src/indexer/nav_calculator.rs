use sha2::{Digest, Sha256};

#[expect(
    dead_code,
    reason = "The NAV placeholder is not wired into the indexer yet."
)]
pub fn calculate_nav(_asset_id: &str) -> f64 {
    100.0 // mock value
}

pub const DIVIDEND_LEAF_DOMAIN: &[u8; 24] = b"tessera-dividend-leaf-v1";
pub const DIVIDEND_NODE_DOMAIN: &[u8; 24] = b"tessera-dividend-node-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DividendLeaf {
    pub distribution_id: u64,
    pub leaf_index: u32,
    pub holder_key: [u8; 32],
    pub entitlement: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DividendMerkleTree {
    pub root: [u8; 32],
    pub proofs: Vec<Vec<[u8; 32]>>,
}

pub fn dividend_leaf_hash(leaf: &DividendLeaf) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DIVIDEND_LEAF_DOMAIN);
    hasher.update(leaf.distribution_id.to_be_bytes());
    hasher.update(leaf.leaf_index.to_be_bytes());
    hasher.update(leaf.holder_key);
    hasher.update(leaf.entitlement.to_be_bytes());
    hasher.finalize().into()
}

pub fn dividend_node_hash(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DIVIDEND_NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

pub fn build_dividend_merkle_tree(
    distribution_id: u64,
    entitlements: &[([u8; 32], i128)],
) -> DividendMerkleTree {
    assert!(!entitlements.is_empty(), "empty dividend tree");

    let mut levels: Vec<Vec<[u8; 32]>> = Vec::new();
    let mut level: Vec<[u8; 32]> = entitlements
        .iter()
        .enumerate()
        .map(|(index, (holder_key, entitlement))| {
            dividend_leaf_hash(&DividendLeaf {
                distribution_id,
                leaf_index: index as u32,
                holder_key: *holder_key,
                entitlement: *entitlement,
            })
        })
        .collect();
    levels.push(level.clone());

    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut index = 0;
        while index < level.len() {
            if index + 1 < level.len() {
                next.push(dividend_node_hash(level[index], level[index + 1]));
            } else {
                next.push(level[index]);
            }
            index += 2;
        }
        level = next;
        levels.push(level.clone());
    }

    let mut proofs = Vec::with_capacity(entitlements.len());
    for leaf_index in 0..entitlements.len() as u32 {
        let mut siblings = Vec::new();
        let mut index = leaf_index;
        for level in levels.iter().take(levels.len() - 1) {
            let sibling = index ^ 1;
            if (sibling as usize) < level.len() {
                siblings.push(level[sibling as usize]);
            }
            index /= 2;
        }
        proofs.push(siblings);
    }

    DividendMerkleTree {
        root: level[0],
        proofs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dividend_merkle_golden_vector_matches_contract() {
        let tree = build_dividend_merkle_tree(0, &[([1u8; 32], 120), ([2u8; 32], 80)]);
        assert_eq!(
            tree.root,
            [
                0xd4, 0x24, 0x9b, 0xc0, 0x24, 0xe1, 0x93, 0x72, 0x78, 0x70, 0xf9, 0xf2, 0xa8, 0xf2,
                0xbf, 0x6b, 0xb3, 0x24, 0x78, 0xce, 0xf8, 0x43, 0x31, 0xb5, 0x18, 0xa4, 0xb5, 0x76,
                0x05, 0x2c, 0xc0, 0xcf,
            ]
        );
        assert_eq!(
            tree.proofs[0],
            vec![[
                0xaa, 0xc9, 0xc9, 0xfd, 0xd1, 0x0d, 0x1c, 0xd1, 0x77, 0xaf, 0xb4, 0x5e, 0xe1, 0xe9,
                0xe2, 0x89, 0x1b, 0x04, 0x75, 0xf3, 0x64, 0xa8, 0x6d, 0x9b, 0x51, 0x54, 0xe6, 0xba,
                0x10, 0x6d, 0x58, 0x62,
            ]]
        );
        assert_eq!(
            tree.proofs[1],
            vec![[
                0xde, 0xa0, 0xbf, 0x2a, 0x93, 0xb7, 0x1c, 0x52, 0xd5, 0xbc, 0x87, 0xa0, 0x5b, 0xd8,
                0x41, 0xf2, 0xfc, 0xd2, 0x44, 0x66, 0xc2, 0x93, 0xa4, 0xea, 0xe5, 0x4e, 0x38, 0x45,
                0xf1, 0xbe, 0xc1, 0x09,
            ]]
        );
    }
}
