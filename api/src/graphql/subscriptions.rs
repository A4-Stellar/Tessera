use async_graphql::{Context, FieldError, FieldResult, Subscription};
use async_stream::stream;
use futures_util::{SinkExt, Stream, StreamExt};
use redis::{aio::PubSub, AsyncCommands, Client as RedisClient, RedisResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

use crate::models::{AssetTransfer, DividendEvent};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const REDIS_CHANNEL_ASSET_TRANSFERS: &str = "tessera:asset_transfers";
const REDIS_CHANNEL_DIVIDEND_EVENTS: &str = "tessera:dividend_events";
const REDIS_CHANNEL_HEARTBEAT: &str = "tessera:heartbeat";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubscriptionEvent {
    AssetTransfer(AssetTransfer),
    DividendEvent(DividendEvent),
    Heartbeat { timestamp: u64 },
}

pub struct RedisSubscriptionManager {
    redis_client: RedisClient,
    tx: broadcast::Sender<SubscriptionEvent>,
}

impl RedisSubscriptionManager {
    pub fn new(redis_url: &str) -> RedisResult<Self> {
        let redis_client = RedisClient::open(redis_url)?;
        let (tx, _) = broadcast::channel(1024);
        Ok(Self { redis_client, tx })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SubscriptionEvent> {
        self.tx.subscribe()
    }

    pub async fn publish_asset_transfer(&self, event: AssetTransfer) -> RedisResult<()> {
        let event = SubscriptionEvent::AssetTransfer(event);
        let payload = serde_json::to_string(&event).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        let mut conn = self.redis_client.get_async_connection().await?;
        conn.publish(REDIS_CHANNEL_ASSET_TRANSFERS, payload).await
    }

    pub async fn publish_dividend_event(&self, event: DividendEvent) -> RedisResult<()> {
        let event = SubscriptionEvent::DividendEvent(event);
        let payload = serde_json::to_string(&event).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        let mut conn = self.redis_client.get_async_connection().await?;
        conn.publish(REDIS_CHANNEL_DIVIDEND_EVENTS, payload).await
    }

    pub async fn start_listener(self: Arc<Self>) {
        let mut pubsub = match self.redis_client.get_async_pubsub().await {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to create Redis pubsub: {}", e);
                return;
            }
        };

        if let Err(e) = pubsub.subscribe(REDIS_CHANNEL_ASSET_TRANSFERS).await {
            error!("Failed to subscribe to asset_transfers: {}", e);
            return;
        }
        if let Err(e) = pubsub.subscribe(REDIS_CHANNEL_DIVIDEND_EVENTS).await {
            error!("Failed to subscribe to dividend_events: {}", e);
            return;
        }
        if let Err(e) = pubsub.subscribe(REDIS_CHANNEL_HEARTBEAT).await {
            error!("Failed to subscribe to heartbeat: {}", e);
            return;
        }

        info!("Redis subscription listener started on channels: asset_transfers, dividend_events, heartbeat");

        let mut stream = pubsub.on_message();
        while let Some(msg) = stream.next().await {
            let channel = msg.get_channel_name();
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(e) => {
                    warn!("Failed to get payload: {}", e);
                    continue;
                }
            };

            let event: SubscriptionEvent = match serde_json::from_str(&payload) {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to deserialize event from channel {}: {}", channel, e);
                    continue;
                }
            };

            if self.tx.send(event).is_err() {
                debug!("No active subscribers for event from channel {}", channel);
            }
        }
    }

    pub async fn start_heartbeat(self: Arc<Self>) {
        let mut interval = interval(HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            let event = SubscriptionEvent::Heartbeat {
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            let payload = match serde_json::to_string(&event) {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to serialize heartbeat: {}", e);
                    continue;
                }
            };
            let mut conn = match self.redis_client.get_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to get Redis connection for heartbeat: {}", e);
                    continue;
                }
            };
            if let Err(e) = conn.publish(REDIS_CHANNEL_HEARTBEAT, payload).await {
                error!("Failed to publish heartbeat: {}", e);
            }
        }
    }
}

pub struct SubscriptionManager {
    redis_manager: Arc<RedisSubscriptionManager>,
}

impl SubscriptionManager {
    pub fn new(redis_url: &str) -> RedisResult<Self> {
        let redis_manager = Arc::new(RedisSubscriptionManager::new(redis_url)?);
        Ok(Self { redis_manager })
    }

    pub fn redis_manager(&self) -> &Arc<RedisSubscriptionManager> {
        &self.redis_manager
    }

    pub async fn start_background_tasks(self: Arc<Self>) {
        let redis_manager = self.redis_manager.clone();
        tokio::spawn(async move {
            redis_manager.start_listener().await;
        });

        let redis_manager = self.redis_manager.clone();
        tokio::spawn(async move {
            redis_manager.start_heartbeat().await;
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SubscriptionEvent> {
        self.redis_manager.subscribe()
    }

    pub async fn publish_asset_transfer(&self, event: AssetTransfer) -> RedisResult<()> {
        self.redis_manager.publish_asset_transfer(event).await
    }

    pub async fn publish_dividend_event(&self, event: DividendEvent) -> RedisResult<()> {
        self.redis_manager.publish_dividend_event(event).await
    }
}

pub struct SubscriptionRoot {
    manager: Arc<SubscriptionManager>,
}

impl SubscriptionRoot {
    pub fn new(manager: Arc<SubscriptionManager>) -> Self {
        Self { manager }
    }
}

#[Subscription]
impl SubscriptionRoot {
    async fn asset_transfers(&self, ctx: &Context<'_>) -> impl Stream<Item = AssetTransfer> {
        let manager = self.manager.clone();
        let mut rx = manager.subscribe();

        stream! {
            loop {
                match rx.recv().await {
                    Ok(SubscriptionEvent::AssetTransfer(event)) => {
                        yield event;
                    }
                    Ok(SubscriptionEvent::Heartbeat { .. }) => {
                        continue;
                    }
                    Ok(SubscriptionEvent::DividendEvent(_)) => {
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("Asset transfers subscription channel closed");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Asset transfers subscription lagged by {} messages", n);
                    }
                }
            }
        }
    }

    async fn dividend_events(&self, ctx: &Context<'_>) -> impl Stream<Item = DividendEvent> {
        let manager = self.manager.clone();
        let mut rx = manager.subscribe();

        stream! {
            loop {
                match rx.recv().await {
                    Ok(SubscriptionEvent::DividendEvent(event)) => {
                        yield event;
                    }
                    Ok(SubscriptionEvent::Heartbeat { .. }) => {
                        continue;
                    }
                    Ok(SubscriptionEvent::AssetTransfer(_)) => {
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("Dividend events subscription channel closed");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Dividend events subscription lagged by {} messages", n);
                    }
                }
            }
        }
    }
}

pub async fn create_subscription_manager(redis_url: &str) -> RedisResult<Arc<SubscriptionManager>> {
    let manager = Arc::new(SubscriptionManager::new(redis_url)?);
    manager.start_background_tasks().await;
    Ok(manager)
}