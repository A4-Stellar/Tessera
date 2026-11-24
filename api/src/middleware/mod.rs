pub mod anonymizer;
pub mod deprecation;
pub mod pii_scrubber;
#[expect(
    dead_code,
    reason = "superseded by the token bucket limiter; kept until its callers are migrated"
)]
pub mod rate_limit;
/// Sandbox tenant validation and Postgres RLS session binding (issue #468).
pub mod sandbox;
#[expect(
    dead_code,
    reason = "the legacy API-key tier helpers remain staged while tenant headers are active"
)]
pub mod tenant;
pub mod token_bucket;
