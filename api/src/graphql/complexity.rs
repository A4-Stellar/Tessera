//! Query depth and complexity limits for the GraphQL endpoint.
//!
//! async-graphql validates both limits against the parsed query before any
//! resolver runs, and answers an over-limit query with an error naming the
//! limit that was breached.

use async_graphql::{ObjectType, SchemaBuilder, SubscriptionType};

/// Queries nested deeper than this are rejected.
pub const MAX_DEPTH: usize = 5;
/// Queries scoring above this are rejected.
pub const MAX_COMPLEXITY: usize = 500;

pub fn apply_limits<Q, M, S>(builder: SchemaBuilder<Q, M, S>) -> SchemaBuilder<Q, M, S>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    builder
        .limit_depth(MAX_DEPTH)
        .limit_complexity(MAX_COMPLEXITY)
}

#[cfg(test)]
mod tests {
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};

    use super::*;

    struct Node;

    #[Object]
    impl Node {
        async fn id(&self) -> i32 {
            1
        }

        async fn child(&self) -> Node {
            Node
        }
    }

    struct Query;

    #[Object]
    impl Query {
        async fn node(&self) -> Node {
            Node
        }
    }

    fn schema() -> Schema<Query, EmptyMutation, EmptySubscription> {
        apply_limits(Schema::build(Query, EmptyMutation, EmptySubscription)).finish()
    }

    #[tokio::test]
    async fn query_within_limits_executes() {
        let response = schema().execute("{ node { child { id } } }").await;
        assert!(response.errors.is_empty());
    }

    #[tokio::test]
    async fn query_deeper_than_limit_is_rejected() {
        let query = format!("{{ node {}id{} }}", "{ child ".repeat(5), " }".repeat(5));
        let response = schema().execute(query).await;
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("too deep"));
    }

    #[tokio::test]
    async fn query_more_complex_than_limit_is_rejected() {
        let fields: String = (0..=MAX_COMPLEXITY).map(|i| format!("a{i}: id ")).collect();
        let response = schema().execute(format!("{{ node {{ {fields} }} }}")).await;
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("too complex"));
    }
}
