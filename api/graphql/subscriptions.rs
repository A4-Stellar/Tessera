// GraphQL Subscriptions with Redis Pub/Sub for issue #157
use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, Subscription, Result};
use futures_util::{Stream, stream};
use redis::{Client as RedisClient, AsyncCommands, PubSubCommands};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SorobanEvent {
    AssetTransferred(String),
    DividendDistributed(String),
    ComplianceUpdated(String),
    NewProposal(String),
    VoteCast(String),
}

impl SorobanEvent {
    pub fn channel(&self) -> &'static str {
        match self {
            SorobanEvent::AssetTransferred(_) => "soroban:transfers",
            SorobanEvent::DividendDistributed(_) => "soroban:dividends",
            SorobanEvent::ComplianceUpdated(_) => "soroban:compliance",
            SorobanEvent::NewProposal(_) => "soroban:governance:proposals",
            SorobanEvent::VoteCast(_) => "soroban:governance:votes",
        }
    }
}

pub struct RedisPubSubManager {
    client: RedisClient,
    publisher: redis::aio::Connection,
    subscriber: redis::aio::Connection,
}

impl RedisPubSubManager {
    pub async fn new(redis_url: &str) -> Result<Self> {
        let client = RedisClient::open(redis_url)?;
        let publisher = client.get_async_connection().await?;
        let subscriber = client.get_async_connection().await?;
        Ok(Self { client, publisher, subscriber })
    }

    pub async fn publish(&mut self, event: SorobanEvent) -> Result<()> {
        let channel = event.channel();
        let payload = serde_json::to_string(&event)?;
        self.publisher.publish(channel, payload).await.map_err(|e| async_graphql::Error::new(format!("Publish error: {}", e)))?;
        Ok(())
    }

    pub async fn subscribe(&mut self, channels: &[&str]) -> Result<redis::aio::PubSub> {
        let mut pubsub = self.subscriber.as_pubsub();
        for channel in channels {
            pubsub.subscribe(channel).await.map_err(|e| async_graphql::Error::new(format!("Subscribe error: {}", e)))?;
        }
        Ok(pubsub)
    }
}

#[Object]
impl SorobanEvent {
    fn asset_transferred(&self) -> Option<String> { Some("asset_transferred".to_string()) }
    fn dividend_distributed(&self) -> Option<String> { Some("dividend_distributed".to_string()) }
    fn compliance_updated(&self) -> Option<String> { Some("compliance_updated".to_string()) }
    fn new_proposal(&self) -> Option<String> { Some("new_proposal".to_string()) }
    fn vote_cast(&self) -> Option<String> { Some("vote_cast".to_string()) }
}

#[Subscription]
impl SubscriptionRoot {
    async fn asset_transfers(&self, _ctx: &Context<'_>) -> Result<impl Stream<Item = String>> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn(async move {
            let mut pubsub = redis::aio::Connection::connect("redis://localhost/").await.unwrap();
            pubsub.subscribe(&["soroban:transfers"]).await.unwrap();
            let mut ies = pubsub.on_message();
            while let Some(msg) = ies.next().await {
                let _ = tx.send(msg.get_payload::<String>().unwrap_or_default()).await;
            }
        });
        Ok(rx.map(|s| s.unwrap_or_default()))
    }

    async fn dividends(&self, _ctx: &Context<'_>) -> Result<impl Stream<Item = String>> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn(async move {
            let mut pubsub = redis::aio::Connection::connect("redis://localhost/").await.unwrap();
            pubsub.subscribe(&["soroban:dividends"]).await.unwrap();
            let mut ies = pubsub.on_message();
            while let Some(msg) = ies.next().await {
                let _ = tx.send(msg.get_payload::<String>().unwrap_or_default()).await;
            }
        });
        Ok(rx.map(|s| s.unwrap_or_default()))
    }
}
