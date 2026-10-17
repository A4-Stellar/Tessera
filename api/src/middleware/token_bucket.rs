//! Token bucket rate limiting with per-API-key-tier capacity (burst cap) and
//! refill rate, backed by Redis when `RWA_RATE_LIMIT_REDIS_URL` is set.
//!
//! A bucket holds at most `capacity` tokens and gains `refill_per_second`
//! tokens every second: `tokens = min(capacity, tokens + elapsed * rate)`.
//! Each request spends one token.

use async_trait::async_trait;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Capacity and refill rate for a class of clients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tier {
    pub capacity: u32,
    pub refill_per_second: u32,
}

/// Result of spending a token, carrying the values for the rate-limit headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    /// Seconds until the bucket is full again.
    pub reset_seconds: u64,
    /// Seconds until one token is available (meaningful when denied).
    pub retry_after: u64,
}

/// Refill `tokens` for `elapsed_ms` and try to spend one token.
fn take(tokens: f64, elapsed_ms: u64, tier: Tier) -> (Decision, f64) {
    let capacity = tier.capacity as f64;
    let rate = tier.refill_per_second.max(1) as f64;
    let mut tokens = (tokens + elapsed_ms as f64 * rate / 1000.0).min(capacity);
    let allowed = tokens >= 1.0;
    if allowed {
        tokens -= 1.0;
    }
    let decision = Decision {
        allowed,
        limit: tier.capacity,
        remaining: tokens as u32,
        reset_seconds: ((capacity - tokens) / rate).ceil() as u64,
        retry_after: if allowed {
            0
        } else {
            ((1.0 - tokens) / rate).ceil() as u64
        },
    };
    (decision, tokens)
}

#[async_trait]
pub trait BucketStore: Send + Sync {
    async fn take(&self, key: &str, tier: Tier) -> Result<Decision, String>;
}

#[derive(Default)]
pub struct MemoryBucketStore {
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

#[async_trait]
impl BucketStore for MemoryBucketStore {
    async fn take(&self, key: &str, tier: Tier) -> Result<Decision, String> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap();
        let (tokens, updated) = buckets
            .get(key)
            .copied()
            .unwrap_or((tier.capacity as f64, now));
        let elapsed_ms = now.saturating_duration_since(updated).as_millis() as u64;
        let (decision, tokens) = take(tokens, elapsed_ms, tier);
        buckets.insert(key.to_string(), (tokens, now));
        Ok(decision)
    }
}

pub struct RedisBucketStore {
    client: redis::Client,
}

impl RedisBucketStore {
    pub fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

#[async_trait]
impl BucketStore for RedisBucketStore {
    async fn take(&self, key: &str, tier: Tier) -> Result<Decision, String> {
        let mut con = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;

        // Atomic refill-and-spend using the Redis clock, so every API node
        // shares one view of time.
        let script = redis::Script::new(
            r#"
            local t = redis.call('TIME')
            local now = t[1] * 1000 + math.floor(t[2] / 1000)
            local capacity = tonumber(ARGV[1])
            local rate = tonumber(ARGV[2])
            local state = redis.call('HMGET', KEYS[1], 'tokens', 'ts')
            local tokens = tonumber(state[1]) or capacity
            local ts = tonumber(state[2]) or now
            tokens = math.min(capacity, tokens + math.max(0, now - ts) * rate / 1000)
            local allowed = 0
            if tokens >= 1 then
                tokens = tokens - 1
                allowed = 1
            end
            redis.call('HSET', KEYS[1], 'tokens', tokens, 'ts', now)
            redis.call('PEXPIRE', KEYS[1], math.ceil(capacity / rate * 1000) + 1000)
            local retry_after = 0
            if allowed == 0 then
                retry_after = math.ceil((1 - tokens) / rate)
            end
            return {allowed, math.floor(tokens), math.ceil((capacity - tokens) / rate), retry_after}
        "#,
        );

        let result: Vec<u64> = script
            .key(format!("token_bucket:{key}"))
            .arg(tier.capacity)
            .arg(tier.refill_per_second.max(1))
            .invoke_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Decision {
            allowed: result[0] == 1,
            limit: tier.capacity,
            remaining: result[1] as u32,
            reset_seconds: result[2],
            retry_after: result[3],
        })
    }
}

pub struct TokenBucketState {
    pub store: Arc<dyn BucketStore>,
    pub default_tier: Tier,
    /// Tier overrides by `X-API-Key` value.
    pub api_key_tiers: HashMap<String, Tier>,
}

impl TokenBucketState {
    /// Parse `RWA_API_KEY_TIERS`: `key=capacity:refill_per_second,...`.
    pub fn parse_api_key_tiers(spec: &str) -> HashMap<String, Tier> {
        spec.split(',')
            .filter_map(|entry| {
                let (key, tier) = entry.trim().split_once('=')?;
                let (capacity, refill) = tier.split_once(':')?;
                Some((
                    key.to_string(),
                    Tier {
                        capacity: capacity.parse().ok()?,
                        refill_per_second: refill.parse().ok()?,
                    },
                ))
            })
            .collect()
    }

    fn tier_for(&self, api_key: Option<&str>) -> Tier {
        api_key
            .and_then(|key| self.api_key_tiers.get(key))
            .copied()
            .unwrap_or(self.default_tier)
    }
}

fn insert_rate_limit_headers(headers: &mut HeaderMap, decision: &Decision) {
    headers.insert("x-ratelimit-limit", HeaderValue::from(decision.limit));
    headers.insert(
        "x-ratelimit-remaining",
        HeaderValue::from(decision.remaining),
    );
    headers.insert(
        "x-ratelimit-reset",
        HeaderValue::from(decision.reset_seconds),
    );
}

pub async fn token_bucket_middleware(
    State(state): State<Arc<TokenBucketState>>,
    req: Request,
    next: Next,
) -> Response {
    let api_key = req
        .headers()
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        // Only configured keys get their own bucket; any other value would let
        // a client mint fresh buckets, or collide with another client's IP.
        .filter(|value| state.api_key_tiers.contains_key(*value))
        .map(str::to_string);
    let key = match &api_key {
        Some(api_key) => format!("key:{api_key}"),
        None => format!(
            "ip:{}",
            req.extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    };
    let tier = state.tier_for(api_key.as_deref());

    match state.store.take(&key, tier).await {
        Ok(decision) => {
            let mut response = if decision.allowed {
                next.run(req).await
            } else {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(
                        json!({ "error": "too_many_requests", "message": "Rate limit exceeded." }),
                    ),
                )
                    .into_response();
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(decision.retry_after));
                response
            };
            insert_rate_limit_headers(response.headers_mut(), &decision);
            response
        }
        Err(e) => {
            tracing::error!("Rate limiter error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIER: Tier = Tier {
        capacity: 3,
        refill_per_second: 2,
    };

    #[test]
    fn burst_is_capped_at_capacity() {
        let (first, tokens) = take(3.0, 0, TIER);
        assert!(first.allowed);
        assert_eq!((first.limit, first.remaining), (3, 2));
        let (_, tokens) = take(tokens, 0, TIER);
        let (_, tokens) = take(tokens, 0, TIER);
        let (denied, _) = take(tokens, 0, TIER);
        assert!(!denied.allowed);
        assert_eq!(denied.remaining, 0);
        assert_eq!(denied.retry_after, 1);
    }

    #[test]
    fn tokens_refill_at_the_tier_rate_up_to_capacity() {
        let (decision, tokens) = take(0.0, 1000, TIER);
        assert!(decision.allowed);
        assert_eq!(decision.remaining, 1);
        assert_eq!(decision.reset_seconds, 1);
        let (_, tokens) = take(tokens, 60_000, TIER);
        assert_eq!(tokens, 2.0);
    }

    #[test]
    fn api_key_tiers_are_parsed_and_selected() {
        let state = TokenBucketState {
            store: Arc::new(MemoryBucketStore::default()),
            default_tier: TIER,
            api_key_tiers: TokenBucketState::parse_api_key_tiers("ent=100:50, bad, x=1:y"),
        };
        assert_eq!(state.api_key_tiers.len(), 1);
        assert_eq!(state.tier_for(Some("ent")).capacity, 100);
        assert_eq!(state.tier_for(Some("other")), TIER);
        assert_eq!(state.tier_for(None), TIER);
    }

    #[tokio::test]
    async fn memory_store_denies_after_burst() {
        let store = MemoryBucketStore::default();
        for _ in 0..3 {
            assert!(store.take("client", TIER).await.unwrap().allowed);
        }
        assert!(!store.take("client", TIER).await.unwrap().allowed);
        assert!(store.take("other", TIER).await.unwrap().allowed);
    }
}
