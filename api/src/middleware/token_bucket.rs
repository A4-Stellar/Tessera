//! Token bucket rate limiting with per-tenant capacity and refill rate,
//! backed by Redis when `RWA_RATE_LIMIT_REDIS_URL` is set.
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
use dashmap::DashMap;
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Capacity and refill rate for a class of clients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tier {
    pub capacity: u32,
    pub refill_per_second: f64,
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
    let rate = tier.refill_per_second.max(f64::MIN_POSITIVE);
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
    buckets: DashMap<String, (f64, Instant)>,
}

#[async_trait]
impl BucketStore for MemoryBucketStore {
    async fn take(&self, key: &str, tier: Tier) -> Result<Decision, String> {
        let now = Instant::now();
        let mut bucket = self
            .buckets
            .entry(key.to_string())
            .or_insert((tier.capacity as f64, now));
        let elapsed_ms = now.saturating_duration_since(bucket.1).as_millis() as u64;
        let (decision, tokens) = take(bucket.0, elapsed_ms, tier);
        *bucket = (tokens, now);
        Ok(decision)
    }
}

pub struct RedisBucketStore {
    client: redis::Client,
    fallback: DashMap<String, (f64, Instant)>,
}

impl RedisBucketStore {
    pub fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            fallback: DashMap::new(),
        })
    }

    fn take_local(&self, key: &str, tier: Tier) -> Decision {
        let now = Instant::now();
        let mut bucket = self
            .fallback
            .entry(key.to_string())
            .or_insert((tier.capacity as f64, now));
        let elapsed_ms = now.saturating_duration_since(bucket.1).as_millis() as u64;
        let (decision, tokens) = take(bucket.0, elapsed_ms, tier);
        *bucket = (tokens, now);
        decision
    }
}

#[async_trait]
impl BucketStore for RedisBucketStore {
    async fn take(&self, key: &str, tier: Tier) -> Result<Decision, String> {
        let connection = self.client.get_multiplexed_async_connection().await;
        let mut con = match connection {
            Ok(con) => con,
            Err(error) => {
                tracing::warn!(%error, "Redis rate limiter unavailable; using local degraded bucket");
                return Ok(self.take_local(key, tier));
            }
        };

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

        let result: Result<Vec<u64>, _> = script
            .key(format!("token_bucket:{key}"))
            .arg(tier.capacity)
            .arg(tier.refill_per_second.max(f64::MIN_POSITIVE))
            .invoke_async(&mut con)
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(%error, "Redis rate-limit operation failed; using local degraded bucket");
                return Ok(self.take_local(key, tier));
            }
        };

        Ok(Decision {
            allowed: result[0] == 1,
            limit: tier.capacity,
            remaining: result[1] as u32,
            reset_seconds: result[2],
            retry_after: result[3],
        })
    }
}

/// Sandbox tenants get a relaxed but still bounded bucket (issue #468). The
/// limit stays a real token bucket so a runaway integration test cannot take
/// the shared API down.
pub const SANDBOX_RATE_PER_SECOND: u64 = 1_000;
pub const SANDBOX_RATE_BURST: u32 = 1_000;
pub const SANDBOX_TENANT_HEADER: &str = "x-sandbox-tenant-id";

pub struct TokenBucketState {
    pub store: Arc<dyn BucketStore>,
    pub default_tier: Tier,
    /// Legacy tier overrides by `X-API-Key` value for non-tenant requests.
    pub api_key_tiers: HashMap<String, Tier>,
    /// When true, `X-Sandbox-Tenant-ID` selects the relaxed 1000 req/s tier.
    pub sandbox_mode: bool,
    tenant_limits: Option<PgPool>,
    tenant_pool_error: Option<String>,
    tenant_cache: DashMap<String, (Tier, Instant)>,
}

impl TokenBucketState {
    pub fn new(
        store: Arc<dyn BucketStore>,
        default_tier: Tier,
        api_key_tiers: HashMap<String, Tier>,
    ) -> Self {
        Self {
            store,
            default_tier,
            api_key_tiers,
            sandbox_mode: false,
            tenant_limits: None,
            tenant_pool_error: None,
            tenant_cache: DashMap::new(),
        }
        .with_tenant_pool()
    }

    /// Enable the sandbox rate tier (`RWA_SANDBOX_MODE=true` in the stack).
    pub fn with_sandbox_mode(mut self, enabled: bool) -> Self {
        self.sandbox_mode = enabled;
        self
    }

    /// The bucket applied to sandbox tenants.
    pub fn sandbox_tier() -> Tier {
        Tier {
            capacity: SANDBOX_RATE_BURST,
            refill_per_second: SANDBOX_RATE_PER_SECOND as f64,
        }
    }

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

    fn with_tenant_pool(mut self) -> Self {
        let Ok(url) = std::env::var("RWA_DATABASE_URL") else {
            return self;
        };
        match PgPoolOptions::new().connect_lazy(&url) {
            Ok(pool) => self.tenant_limits = Some(pool),
            Err(error) => {
                let error = error.to_string();
                tracing::error!(%error, "could not initialize tenant rate-limit database pool");
                self.tenant_pool_error = Some(error);
            }
        }
        self
    }

    async fn tier_for_tenant(&self, tenant_id: &str) -> Result<Tier, String> {
        if let Some(error) = &self.tenant_pool_error {
            return Err(error.clone());
        }
        if let Some(cached) = self.tenant_cache.get(tenant_id) {
            if cached.1.elapsed() < Duration::from_secs(60) {
                return Ok(cached.0);
            }
        }
        let Some(pool) = &self.tenant_limits else {
            return Ok(self.default_tier);
        };
        let row = sqlx::query(
            "SELECT capacity, refill_rate, refill_interval_seconds \
             FROM tenant_rate_limits WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| error.to_string())?;
        let Some(row) = row else {
            self.tenant_cache
                .insert(tenant_id.to_string(), (self.default_tier, Instant::now()));
            return Ok(self.default_tier);
        };
        let capacity = row
            .try_get::<i64, _>("capacity")
            .map_err(|e| e.to_string())?;
        let refill_rate = row
            .try_get::<i64, _>("refill_rate")
            .map_err(|e| e.to_string())?;
        let interval = row
            .try_get::<i64, _>("refill_interval_seconds")
            .map_err(|e| e.to_string())?;
        if !(1..=u32::MAX as i64).contains(&capacity)
            || !(1..=u32::MAX as i64).contains(&refill_rate)
            || !(1..=86_400).contains(&interval)
        {
            return Err(format!(
                "invalid rate-limit configuration for tenant {tenant_id}"
            ));
        }
        let refill_per_second = refill_rate as f64 / interval as f64;
        let tier = Tier {
            capacity: capacity as u32,
            refill_per_second,
        };
        self.tenant_cache
            .insert(tenant_id.to_string(), (tier, Instant::now()));
        Ok(tier)
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
        HeaderValue::from_str(&decision.reset_seconds.to_string())
            .expect("rate-limit seconds are ASCII digits"),
    );
}

pub async fn token_bucket_middleware(
    State(state): State<Arc<TokenBucketState>>,
    req: Request,
    next: Next,
) -> Response {
    let tenant_id = match req.headers().get("X-Tenant-ID") {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(crate::middleware::tenant::tenant_id_from_header)
        {
            Some(tenant_id) => Some(tenant_id),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "invalid_tenant_id",
                        "message": "X-Tenant-ID must be 1-128 ASCII letters, digits, '-' or '_'."
                    })),
                )
                    .into_response();
            }
        },
        None => None,
    };
    // Sandbox tenants use a separate bucket namespace and a relaxed tier so
    // load tests cannot starve production tenants sharing this process.
    // Invalid values are ignored here; `sandbox_tenant_middleware` rejects them.
    let sandbox_tenant = if state.sandbox_mode {
        req.headers()
            .get(SANDBOX_TENANT_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(crate::middleware::tenant::tenant_id_from_header)
    } else {
        None
    };
    let api_key = req
        .headers()
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        // Only configured keys get their own bucket; any other value would let
        // a client mint fresh buckets, or collide with another client's IP.
        .filter(|value| state.api_key_tiers.contains_key(*value))
        .map(str::to_string);
    let key = match (&sandbox_tenant, &tenant_id, &api_key) {
        (Some(sandbox), _, _) => format!("sandbox:{sandbox}"),
        (None, Some(tenant_id), _) => format!("tenant:{tenant_id}"),
        (None, None, Some(api_key)) => format!("key:{api_key}"),
        (None, None, None) => format!(
            "ip:{}",
            req.extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|ci| ci.0.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    };
    let tier = if sandbox_tenant.is_some() {
        TokenBucketState::sandbox_tier()
    } else {
        match tenant_id.as_deref() {
            Some(tenant_id) => match state.tier_for_tenant(tenant_id).await {
                Ok(tier) => tier,
                Err(error) => {
                    tracing::error!(%error, "tenant rate-limit lookup failed");
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Rate-limit configuration unavailable",
                    )
                        .into_response();
                }
            },
            None => state.tier_for(api_key.as_deref()),
        }
    };

    match state.store.take(&key, tier).await {
        Ok(decision) => {
            if let Some(tenant) = sandbox_tenant {
                metrics::counter!("rate_limit_applied", "tenant" => tenant).increment(1);
            } else if let Some(tenant) = tenant_id {
                metrics::counter!("rate_limit_applied", "tenant" => tenant).increment(1);
            }
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
                response.headers_mut().insert(
                    header::RETRY_AFTER,
                    HeaderValue::from_str(&decision.retry_after.to_string())
                        .expect("retry-after seconds are ASCII digits"),
                );
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
        refill_per_second: 2.0,
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
    fn fractional_tenant_refill_preserves_the_retry_delay() {
        let tier = Tier {
            capacity: 2,
            refill_per_second: 0.5,
        };
        let (decision, remaining) = take(0.0, 0, tier);
        assert!(!decision.allowed);
        assert_eq!(decision.retry_after, 2);
        assert_eq!(remaining, 0.0);
    }

    #[test]
    fn api_key_tiers_are_parsed_and_selected() {
        let state = TokenBucketState {
            store: Arc::new(MemoryBucketStore::default()),
            default_tier: TIER,
            api_key_tiers: TokenBucketState::parse_api_key_tiers("ent=100:50, bad, x=1:y"),
            sandbox_mode: false,
            tenant_limits: None,
            tenant_pool_error: None,
            tenant_cache: DashMap::new(),
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

    #[tokio::test]
    async fn fifty_tenant_buckets_do_not_share_tokens() {
        let store = MemoryBucketStore::default();
        for id in 0..50 {
            let tier = Tier {
                capacity: if id % 2 == 0 { 1 } else { 2 },
                refill_per_second: 1.0,
            };
            let key = format!("tenant:{id}");
            assert!(store.take(&key, tier).await.unwrap().allowed);
            assert_eq!(
                store.take(&key, tier).await.unwrap().allowed,
                tier.capacity > 1
            );
            assert!(
                store
                    .take(&format!("tenant:{}:isolated", id), tier)
                    .await
                    .unwrap()
                    .allowed
            );
        }
    }
}
