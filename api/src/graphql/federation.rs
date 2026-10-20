//! Apollo Federation v2 support shared by the GraphQL query root.
//!
//! `FEDERATION_V2_SDL` is intentionally kept explicit: Apollo Router composes
//! the service document rather than relying on implementation-specific
//! introspection details from async-graphql.

use async_graphql::{Object, Union};
use serde_json::Value;

use super::{Asset, Holder};

/// Federation v2 service definition consumed by Apollo Router's `_service`.
pub const FEDERATION_V2_SDL: &str = r#"
extend schema @link(url: "https://specs.apollo.dev/federation/v2.5", import: ["@key", "@shareable", "@external"])

type Asset @key(fields: "id") {
  # Asset identity is supplied by the registry subgraph when this service is
  # composed as an extension in an enterprise graph.
  id: ID! @external
  name: String! @shareable
  tvl: Float! @shareable
}

type Holder @key(fields: "address") {
  address: ID!
  balance: Float! @shareable
}
"#;

/// The Federation `_Service` value.
pub struct Service;

#[Object(name = "_Service")]
impl Service {
    async fn sdl(&self) -> &'static str { FEDERATION_V2_SDL }
}

/// Values accepted by the Federation `_entities` resolver.
#[derive(Union)]
#[graphql(name = "_Entity")]
pub enum Entity {
    Asset(Asset),
    Holder(Holder),
}

/// Resolve representations supplied by Apollo Router. Unknown typenames and
/// malformed keys are represented as `null`, per the federation contract.
pub fn resolve_entities(representations: Vec<Value>) -> Vec<Option<Entity>> {
    representations.into_iter().map(|representation| {
        let object = representation.as_object()?;
        let typename = object.get("__typename")?.as_str()?;
        match typename {
            "Asset" => Some(Entity::Asset(Asset {
                id: object.get("id")?.as_str()?.to_owned(),
                name: "Tokenized Real Estate Fund".to_owned(),
                tvl: 1_500_000.0,
            })),
            "Holder" => Some(Entity::Holder(Holder {
                address: object.get("address")?.as_str()?.to_owned(),
                balance: 100.5,
            })),
            _ => None,
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn federation_sdl_declares_v2_keys_and_shareable_fields() {
        assert!(FEDERATION_V2_SDL.contains("federation/v2.5"));
        assert!(FEDERATION_V2_SDL.contains("type Asset @key(fields: \"id\")"));
        assert!(FEDERATION_V2_SDL.contains("type Holder @key(fields: \"address\")"));
        assert!(FEDERATION_V2_SDL.contains("@external"));
    }

    #[test]
    fn resolves_asset_and_holder_representations() {
        let entities = resolve_entities(vec![json!({"__typename": "Asset", "id": "A1"}), json!({"__typename": "Holder", "address": "G1"})]);
        assert!(matches!(entities[0], Some(Entity::Asset(_))));
        assert!(matches!(entities[1], Some(Entity::Holder(_))));
    }
}
