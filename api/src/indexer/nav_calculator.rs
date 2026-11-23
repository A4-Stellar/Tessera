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

// ── NAV aggregation (issue #456) ──────────────────────────────────────────

/// Decimal places of the fixed-point NAV representation (`price * 10^7`),
/// matching the Stellar 7-decimal amount convention used on-chain.
pub const NAV_SCALE: u32 = 7;

/// Maximum tolerated move between consecutive NAVs before an alert fires: 5 %.
pub const NAV_DEVIATION_ALERT_BPS: i128 = 500;

/// One oracle quote for a single underlying RWA asset.
#[allow(dead_code)] // not yet wired into the indexer
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    /// Oracle identifier, e.g. `"stellar_dex_twap"`, `"pyth"`, `"band"`.
    pub source: &'static str,
    /// Price in fixed point with [`NAV_SCALE`] decimals.
    pub price: i128,
    /// Relative weight; zero-weight quotes are ignored.
    pub weight: u32,
    /// Unix seconds of the oracle's last update.
    pub updated_at: u64,
}

/// Result of one NAV aggregation pass.
#[allow(dead_code)] // not yet wired into the indexer
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavResult {
    /// Fresh NAV when `status` is [`NavStatus::Fresh`]; the last known NAV
    /// (possibly `None`) when stale.
    pub nav: Option<i128>,
    pub status: NavStatus,
    /// Oracles excluded for staleness; callers emit `OracleStaleness` for each.
    pub stale_sources: Vec<&'static str>,
}

#[allow(dead_code)] // not yet wired into the indexer
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavStatus {
    Fresh,
    Stale,
}

/// Compute a weighted-average NAV from fresh oracle quotes.
///
/// * Quotes older than `max_staleness_seconds` are excluded and reported in
///   `stale_sources`.
/// * If fewer than `min_oracles` fresh quotes remain (or arithmetic would
///   overflow), the result is [`NavStatus::Stale`] and carries
///   `last_known` instead of a new value.
#[allow(dead_code)] // not yet wired into the indexer
pub fn aggregate_nav(
    prices: &[OraclePrice],
    now: u64,
    max_staleness_seconds: u64,
    min_oracles: usize,
    last_known: Option<i128>,
) -> NavResult {
    let mut stale_sources = Vec::new();
    let mut weighted_sum: i128 = 0;
    let mut total_weight: i128 = 0;
    let mut fresh_count = 0usize;
    let mut overflow = false;

    for quote in prices.iter().filter(|q| q.weight > 0) {
        if now.saturating_sub(quote.updated_at) > max_staleness_seconds {
            stale_sources.push(quote.source);
            continue;
        }
        fresh_count += 1;
        let weight = i128::from(quote.weight);
        match quote.price.checked_mul(weight) {
            Some(term) => match weighted_sum.checked_add(term) {
                Some(sum) => weighted_sum = sum,
                None => overflow = true,
            },
            None => overflow = true,
        }
        total_weight += weight;
    }

    if fresh_count < min_oracles || fresh_count == 0 || overflow {
        return NavResult {
            nav: last_known,
            status: NavStatus::Stale,
            stale_sources,
        };
    }

    NavResult {
        nav: Some(weighted_sum / total_weight),
        status: NavStatus::Fresh,
        stale_sources,
    }
}

/// True when `next` differs from `previous` by more than
/// [`NAV_DEVIATION_ALERT_BPS`] basis points of `previous`.
#[allow(dead_code)] // not yet wired into the indexer
pub fn nav_deviation_exceeds_alert(previous: i128, next: i128) -> bool {
    if previous == 0 {
        return next != 0;
    }
    let diff = next.checked_sub(previous).and_then(i128::checked_abs);
    match diff.and_then(|d| d.checked_mul(10_000)) {
        Some(scaled) => scaled / previous.abs() > NAV_DEVIATION_ALERT_BPS,
        None => true,
    }
}

#[cfg(test)]
mod nav_aggregation_tests {
    use super::*;

    fn quote(source: &'static str, price: i128, weight: u32, updated_at: u64) -> OraclePrice {
        OraclePrice {
            source,
            price,
            weight,
            updated_at,
        }
    }

    #[test]
    fn weighted_average_uses_only_fresh_quotes() {
        let prices = [
            quote("dex", 100_0000000, 1, 1_000),
            quote("pyth", 200_0000000, 1, 1_000),
            quote("band", 900_0000000, 1, 0), // stale
        ];
        let result = aggregate_nav(&prices, 1_030, 60, 2, None);
        assert_eq!(result.status, NavStatus::Fresh);
        assert_eq!(result.nav, Some(150_0000000));
        assert_eq!(result.stale_sources, vec!["band"]);
    }

    #[test]
    fn too_few_fresh_oracles_serves_last_known_as_stale() {
        let prices = [quote("dex", 100, 1, 0), quote("pyth", 200, 1, 1_000)];
        let result = aggregate_nav(&prices, 1_030, 60, 2, Some(42));
        assert_eq!(result.status, NavStatus::Stale);
        assert_eq!(result.nav, Some(42));
        assert_eq!(result.stale_sources, vec!["dex"]);
    }

    #[test]
    fn zero_weight_quotes_are_ignored() {
        let prices = [quote("dex", 100, 0, 1_000), quote("pyth", 300, 2, 1_000)];
        let result = aggregate_nav(&prices, 1_000, 60, 1, None);
        assert_eq!(result.nav, Some(300));
        assert!(result.stale_sources.is_empty());
    }

    #[test]
    fn overflow_is_treated_as_stale() {
        let prices = [quote("dex", i128::MAX, 2, 1_000)];
        let result = aggregate_nav(&prices, 1_000, 60, 1, Some(7));
        assert_eq!(result.status, NavStatus::Stale);
        assert_eq!(result.nav, Some(7));
    }

    #[test]
    fn deviation_alert_triggers_above_five_percent() {
        assert!(!nav_deviation_exceeds_alert(1_000, 1_050));
        assert!(nav_deviation_exceeds_alert(1_000, 1_051));
        assert!(nav_deviation_exceeds_alert(1_000, 940));
        assert!(!nav_deviation_exceeds_alert(1_000, 960));
    }
}
