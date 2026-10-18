//! Federation entity lookup functions kept separate from transport wiring.
//!
//! The `#[graphql(entity)]` resolvers in `mod.rs` expose these through the
//! Federation v2 `_entities` field.  IDs are strings deliberately: they are
//! stable across database migrations and are the `@key` used by routers.

use super::{Asset, Holder};

pub fn asset_by_id(id: String) -> Asset {
    Asset { id, name: "Tokenized Real Estate Fund".into(), tvl: 1_500_000.0 }
}

pub fn holder_by_address(address: String) -> Holder {
    Holder { address, balance: 100.5 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entity_keys_are_preserved_by_reference_lookups() {
        assert_eq!(asset_by_id("ASSET123".into()).id, "ASSET123");
        assert_eq!(holder_by_address("GABC".into()).address, "GABC");
    }
}
