//! Redis Pub/Sub cache invalidation across API nodes.
//!
//! When the indexer updates an asset it publishes `purge:<asset_id>` on
//! [`CHANNEL`]. Every node subscribes and drops the matching entry from its
//! local in-memory cache as soon as the message arrives.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;

/// Pub/Sub channel carrying purge messages.
pub const CHANNEL: &str = "rwa:cache:invalidate";

pub fn purge_message(asset_id: &str) -> String {
    format!("purge:{asset_id}")
}

/// Extract the asset id from a `purge:<asset_id>` message.
pub fn parse_purge(message: &str) -> Option<&str> {
    message.strip_prefix("purge:").filter(|id| !id.is_empty())
}

/// Node-local in-memory cache keyed by asset id.
#[derive(Clone, Default)]
pub struct LocalCache {
    entries: Arc<Mutex<HashMap<String, serde_json::Value>>>,
}

impl LocalCache {
    pub fn get(&self, asset_id: &str) -> Option<serde_json::Value> {
        self.entries.lock().unwrap().get(asset_id).cloned()
    }

    pub fn insert(&self, asset_id: &str, value: serde_json::Value) {
        self.entries
            .lock()
            .unwrap()
            .insert(asset_id.to_string(), value);
    }

    /// Apply one Pub/Sub message; unrelated payloads are ignored.
    pub fn handle_message(&self, message: &str) {
        if let Some(asset_id) = parse_purge(message) {
            self.entries.lock().unwrap().remove(asset_id);
        }
    }
}

/// Publish a purge for `asset_id` to every subscribed node.
pub async fn publish_purge(client: &redis::Client, asset_id: &str) -> Result<(), String> {
    let mut con = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|e| e.to_string())?;
    let _: i64 = redis::cmd("PUBLISH")
        .arg(CHANNEL)
        .arg(purge_message(asset_id))
        .query_async(&mut con)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Subscribe to [`CHANNEL`] and purge `cache` for every message, until the
/// connection drops.
pub async fn run_invalidator(client: redis::Client, cache: LocalCache) -> Result<(), String> {
    let mut pubsub = client.get_async_pubsub().await.map_err(|e| e.to_string())?;
    pubsub.subscribe(CHANNEL).await.map_err(|e| e.to_string())?;
    let mut messages = pubsub.on_message();
    while let Some(message) = messages.next().await {
        if let Ok(payload) = message.get_payload::<String>() {
            cache.handle_message(&payload);
        }
    }
    Err("Redis Pub/Sub connection closed".to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn purge_message_round_trips() {
        assert_eq!(purge_message("7"), "purge:7");
        assert_eq!(parse_purge("purge:7"), Some("7"));
        assert_eq!(parse_purge("purge:"), None);
        assert_eq!(parse_purge("other:7"), None);
    }

    #[test]
    fn purge_message_invalidates_every_node_cache() {
        let nodes = [LocalCache::default(), LocalCache::default()];
        for node in &nodes {
            node.insert("1", json!({"v": "old"}));
            node.insert("2", json!({"v": "keep"}));
        }

        // Every node receives the same published message.
        let message = purge_message("1");
        for node in &nodes {
            node.handle_message(&message);
        }

        for node in &nodes {
            assert_eq!(node.get("1"), None);
            assert_eq!(node.get("2"), Some(json!({"v": "keep"})));
        }
    }
}
