use async_graphql::{
    Context, EmptyMutation, Object, Schema, Subscription, FieldResult,
};
use futures::{stream, Stream};
use axum::{
    routing::get,
    Router,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use serde::{Deserialize, Serialize};

pub mod complexity;
pub mod federation;

#[derive(Serialize, Deserialize, Clone)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub tvl: f64,
}

#[async_graphql::Object]
impl Asset {
    #[graphql(shareable)]
    async fn id(&self) -> &str {
        &self.id
    }
    async fn name(&self) -> &str {
        &self.name
    }
    async fn tvl(&self) -> f64 {
        self.tvl
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Holder {
    pub address: String,
    pub balance: f64,
}

#[async_graphql::Object]
impl Holder {
    #[graphql(shareable)]
    async fn address(&self) -> &str {
        &self.address
    }
    async fn balance(&self) -> f64 {
        self.balance
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ComplianceSummary {
    pub asset_id: String,
    pub allowlist_count: i32,
}

#[async_graphql::Object]
impl ComplianceSummary {
    async fn asset_id(&self) -> &str {
        &self.asset_id
    }
    async fn allowlist_count(&self) -> i32 {
        self.allowlist_count
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AssetTransfer {
    pub asset_id: String,
    pub from: String,
    pub to: String,
    pub amount: f64,
}

#[async_graphql::Object]
impl AssetTransfer {
    async fn asset_id(&self) -> &str {
        &self.asset_id
    }
    async fn from(&self) -> &str {
        &self.from
    }
    async fn to(&self) -> &str {
        &self.to
    }
    async fn amount(&self) -> f64 {
        self.amount
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DividendEvent {
    pub asset_id: String,
    pub amount_per_share: f64,
}

#[async_graphql::Object]
impl DividendEvent {
    async fn asset_id(&self) -> &str {
        &self.asset_id
    }
    async fn amount_per_share(&self) -> f64 {
        self.amount_per_share
    }
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn asset(&self, _ctx: &Context<'_>, id: String) -> FieldResult<Asset> {
        Ok(federation::asset_by_id(id))
    }

    async fn assets(&self, _ctx: &Context<'_>, filter: Option<String>) -> FieldResult<Vec<Asset>> {
        // Stub implementation
        Ok(vec![Asset {
            id: "ASSET123".to_string(),
            name: "Tokenized Bond".to_string(),
            tvl: 500000.0,
        }])
    }

    async fn holder(&self, _ctx: &Context<'_>, address: String) -> FieldResult<Holder> {
        Ok(federation::holder_by_address(address))
    }

    /// Apollo Federation v2 reference resolver. Generates `Asset @key(fields:
    /// "id")` and enables the router's `_entities` query automatically.
    #[graphql(entity)]
    async fn find_asset_by_id(&self, id: String) -> Asset {
        federation::asset_by_id(id)
    }

    /// Apollo Federation v2 reference resolver for investor identity.
    #[graphql(entity)]
    async fn find_holder_by_address(&self, address: String) -> Holder {
        federation::holder_by_address(address)
    }

    async fn compliance_summary(&self, _ctx: &Context<'_>, id: String) -> FieldResult<ComplianceSummary> {
        // Stub implementation
        Ok(ComplianceSummary {
            asset_id: id,
            allowlist_count: 42,
        })
    }
}

pub struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    async fn asset_transfers(&self, _ctx: &Context<'_>) -> impl Stream<Item = AssetTransfer> {
        stream::empty() // Stub for real-time subscription
    }

    async fn dividend_events(&self, _ctx: &Context<'_>) -> impl Stream<Item = DividendEvent> {
        stream::empty() // Stub for real-time subscription
    }
}

pub type AppSchema = Schema<QueryRoot, EmptyMutation, SubscriptionRoot>;

async fn graphql_handler(schema: axum::extract::Extension<AppSchema>, req: GraphQLRequest) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

async fn graphiql() -> axum::response::Html<String> {
    axum::response::Html(
        async_graphql::http::GraphiQLSource::build()
            .endpoint("/graphql")
            .subscription_endpoint("/graphql")
            .finish()
    )
}

pub fn create_graphql_router() -> Router {
    let schema =
        complexity::apply_limits(Schema::build(QueryRoot, EmptyMutation, SubscriptionRoot))
            // Emits Federation v2 `@link`/directive definitions even if an
            // entity resolver is conditionally removed in a future build.
            .enable_federation()
            .finish();

    Router::new()
        .route("/graphql", get(graphiql).post(graphql_handler))
        .route_service("/graphql/ws", GraphQLSubscription::new(schema.clone()))
        .layer(axum::extract::Extension(schema))
}

#[cfg(test)]
mod federation_tests {
    use super::*;

    #[tokio::test]
    async fn federation_schema_exposes_keys_and_entity_resolution() {
        let schema = Schema::build(QueryRoot, EmptyMutation, SubscriptionRoot)
            .enable_federation()
            .finish();
        let sdl = schema.sdl();
        assert!(sdl.contains("type Asset @key(fields: \"id\")"));
        assert!(sdl.contains("type Holder @key(fields: \"address\")"));
        assert!(sdl.contains("@shareable"));
        let response = schema.execute(r#"{ _entities(representations: [{__typename: "Asset", id: "A-1"}]) { ... on Asset { id } } }"#).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }
}
