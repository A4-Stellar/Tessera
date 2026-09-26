use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use async_trait::async_trait;
use redis::AsyncCommands;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::json;

#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn check(&self, key: &str, limit: u64, window: u64) -> Result<(bool, Option<u64>), String>;
}

pub struct RedisRateLimiter {
    client: redis::Client,
}

impl RedisRateLimiter {
    pub fn new(redis_url: &str) -> Result<Self, String> {
        let client = redis::Client::open(redis_url).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

#[async_trait]
impl RateLimiter for RedisRateLimiter {
    async fn check(&self, key: &str, limit: u64, window: u64) -> Result<(bool, Option<u64>), String> {
        let mut con = self.client.get_multiplexed_async_connection().await.map_err(|e| e.to_string())?;
        let current_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Lua script for atomic sliding window rate limiting
        let script = redis::Script::new(r#"
            local key = KEYS[1]
            local limit = tonumber(ARGV[1])
            local window = tonumber(ARGV[2])
            local current_ts = tonumber(ARGV[3])
            local window_start = current_ts - window

            redis.call('ZREMRANGEBYSCORE', key, '-inf', window_start)
            local count = redis.call('ZCARD', key)

            if count < limit then
                redis.call('ZADD', key, current_ts, current_ts)
                redis.call('EXPIRE', key, window)
                return {1, 0}
            else
                local oldest = redis.call('ZRANGE', key, 0, 0, 'WITHSCORES')
                local retry_after = window
                if oldest and #oldest >= 2 then
                    local oldest_ts = tonumber(oldest[2])
                    retry_after = oldest_ts + window - current_ts
                    if retry_after < 0 then
                        retry_after = 0
                    end
                end
                return {0, retry_after}
            end
        "#);

        let result: Vec<u64> = script
            .key(format!("rate_limit:{}", key))
            .arg(limit)
            .arg(window)
            .arg(current_ts)
            .invoke_async(&mut con)
            .await
            .map_err(|e| e.to_string())?;

        let allowed = result[0] == 1;
        let retry_after = if allowed { None } else { Some(result[1]) };
        
        Ok((allowed, retry_after))
    }
}

pub struct MemoryRateLimiter {
    limiter: governor::RateLimiter<
        String,
        governor::state::keyed::DefaultKeyedStateStore<String>,
        governor::clock::DefaultClock,
    >,
}

impl MemoryRateLimiter {
    pub fn new(per_second: u64, burst: u32) -> Self {
        use governor::Quota;
        use std::num::NonZeroU32;
        let quota = Quota::per_second(NonZeroU32::new(per_second as u32).unwrap())
            .allow_burst(NonZeroU32::new(burst).unwrap());
        Self {
            limiter: governor::RateLimiter::keyed(quota),
        }
    }
}

#[async_trait]
impl RateLimiter for MemoryRateLimiter {
    async fn check(&self, key: &str, _limit: u64, _window: u64) -> Result<(bool, Option<u64>), String> {
        use governor::clock::Clock as _;
        match self.limiter.check_key(&key.to_string()) {
            Ok(_) => Ok((true, None)),
            Err(e) => {
                let wait = e.wait_time_from(self.limiter.clock().now());
                Ok((false, Some(wait.as_secs())))
            }
        }
    }
}

pub struct RateLimitState {
    pub limiter: Arc<dyn RateLimiter>,
    pub limit: u64,
    pub window: u64,
}

pub async fn rate_limit_middleware(
    State(state): State<Arc<RateLimitState>>,
    req: Request,
    next: Next,
) -> Response {
    let key = if let Some(api_key) = req.headers().get("X-API-Key") {
        api_key.to_str().unwrap_or("unknown").to_string()
    } else {
        let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        ip
    };

    match state.limiter.check(&key, state.limit, state.window).await {
        Ok((true, _)) => next.run(req).await,
        Ok((false, retry_after)) => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "too_many_requests", "message": "Rate limit exceeded." })),
            ).into_response();
            
            if let Some(secs) = retry_after {
                if let Ok(val) = HeaderValue::from_str(&secs.to_string()) {
                    response.headers_mut().insert(header::RETRY_AFTER, val);
                }
            }
            response
        }
        Err(e) => {
            tracing::error!("Rate limiter error: {}", e);
            // fail open or closed? usually fail open or internal server error.
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
        }
    }
}
