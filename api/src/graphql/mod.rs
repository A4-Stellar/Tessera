use async_graphql::{
    Context, EmptyMutation, Object, Schema, Subscription, FieldResult,
};
use futures_util::{Stream, stream};
use axum::{
    routing::get,
    Router,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub tvl: f64,
}

#[async_graphql::Object]
impl Asset {
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
        // Stub implementation
        Ok(Asset {
            id,
            name: "Tokenized Real Estate Fund".to_string(),
            tvl: 1500000.0,
        })
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
        // Stub implementation
        Ok(Holder {
            address,
            balance: 100.5,
        })
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
    let schema = Schema::build(QueryRoot, EmptyMutation, SubscriptionRoot).finish();

    Router::new()
        .route("/graphql", get(graphiql).post(graphql_handler))
        .route("/graphql/ws", get(GraphQLSubscription::new(schema.clone())))
        .layer(axum::extract::Extension(schema))
}
