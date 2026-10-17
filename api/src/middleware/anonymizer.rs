//! Response anonymizer for non-admin callers (issue #284).
//!
//! Complements [`super::pii_scrubber`]: where the scrubber pattern-matches
//! free text, this middleware drops *named* personal-data fields from JSON
//! responses unless the caller presents `Authorization: Bearer
//! <RWA_ADMIN_TOKEN>`.
//!
//! * String values of [`SENSITIVE_KEYS`] are replaced by a salt-free
//!   truncated SHA-256 pseudonym (`anon:<16 hex>`), so equal inputs still
//!   correlate (GDPR Art. 4(5) pseudonymisation) without exposing the value.
//! * Non-string values of those keys become `"[REDACTED]"`.
//! * Keys are matched case-insensitively at any depth.
//!
//! Limits, stated plainly: matching is by field name, so a personal value
//! under an unlisted key is not caught (the pattern scrubber still masks
//! emails and IPs), and the same rules apply in every region; there is no
//! per-jurisdiction rule set. Set `RWA_ANONYMIZER=off` to disable.

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::routes::dlq::token_matches;

/// Largest response body the middleware will buffer.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Lower-case JSON keys holding personal or investor identity data.
const SENSITIVE_KEYS: &[&str] = &[
    "email",
    "phone",
    "full_name",
    "first_name",
    "last_name",
    "investor_name",
    "date_of_birth",
    "dob",
    "national_id",
    "tax_id",
    "ssn",
    "passport_number",
    "postal_address",
    "kyc_data",
    "ip_address",
];

/// Stable pseudonym for `value`: `anon:` + first 16 hex chars of SHA-256.
pub fn pseudonymize(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("anon:{}", hex::encode(&digest[..8]))
}

/// Anonymize every sensitive field in `value`, recursively.
pub fn anonymize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if SENSITIVE_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                    let redacted = match &*v {
                        Value::String(s) => Value::String(pseudonymize(s)),
                        Value::Null => Value::Null,
                        _ => Value::String("[REDACTED]".into()),
                    };
                    *v = redacted;
                } else {
                    anonymize(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(anonymize),
        _ => {}
    }
}

fn is_admin(headers: &HeaderMap, expected: &str) -> bool {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    token_matches(expected, supplied)
}

fn enabled() -> bool {
    !matches!(
        std::env::var("RWA_ANONYMIZER").as_deref(),
        Ok("off") | Ok("false") | Ok("0")
    )
}

/// Axum middleware: anonymize JSON response bodies for non-admin callers.
pub async fn anonymizer_middleware(req: Request, next: Next) -> Response {
    let expected = std::env::var("RWA_ADMIN_TOKEN").unwrap_or_default();
    let admin = is_admin(req.headers(), &expected);
    let resp = next.run(req).await;
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
    if admin
        || !enabled()
        || !is_json
        || resp.status() == StatusCode::SWITCHING_PROTOCOLS
        || resp.headers().contains_key(header::CONTENT_ENCODING)
    {
        return resp;
    }

    let (mut parts, body) = resp.into_parts();
    // Fail closed: never emit a body we could not anonymize.
    let Ok(bytes) = to_bytes(body, MAX_BODY_BYTES).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "response could not be anonymized",
        )
            .into_response();
    };
    let Ok(mut json) = serde_json::from_slice::<Value>(&bytes) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "response could not be anonymized",
        )
            .into_response();
    };
    anonymize(&mut json);
    let out = json.to_string();
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
    Response::from_parts(parts, Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use serde_json::json;
    use tower::ServiceExt as _;

    fn sample() -> Value {
        json!({
            "asset": "RWA1",
            "holders": [{
                "address": "GABC",
                "Email": "alice@example.com",
                "kyc_data": {"tax_id": "123-45-6789"},
                "ssn": 123456789,
                "nested": {"phone": "+1 555 0100"}
            }]
        })
    }

    #[test]
    fn sensitive_fields_are_pseudonymized_or_redacted_at_any_depth() {
        let mut v = sample();
        anonymize(&mut v);
        let text = v.to_string();
        for raw in ["alice@example.com", "123-45-6789", "123456789", "555 0100"] {
            assert!(!text.contains(raw), "{raw} leaked: {text}");
        }
        assert_eq!(v["holders"][0]["Email"], pseudonymize("alice@example.com"));
        assert_eq!(v["holders"][0]["ssn"], "[REDACTED]");
        // Public data is untouched.
        assert_eq!(v["holders"][0]["address"], "GABC");
        assert_eq!(v["asset"], "RWA1");
    }

    #[test]
    fn pseudonyms_are_stable_and_distinct() {
        assert_eq!(pseudonymize("a@b.co"), pseudonymize("a@b.co"));
        assert_ne!(pseudonymize("a@b.co"), pseudonymize("c@d.co"));
        assert!(pseudonymize("x").starts_with("anon:"));
    }

    #[test]
    fn admin_requires_exact_configured_bearer_token() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer tok"));
        assert!(is_admin(&h, "tok"));
        assert!(!is_admin(&h, "other"));
        assert!(!is_admin(&h, ""));
        assert!(!is_admin(&HeaderMap::new(), "tok"));
    }

    #[tokio::test]
    async fn middleware_anonymizes_json_for_anonymous_callers() {
        let app = Router::new()
            .route("/", get(|| async { Json(sample()) }))
            .layer(axum::middleware::from_fn(anonymizer_middleware));
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let declared: usize = resp.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert_eq!(declared, body.len());
        let text = std::str::from_utf8(&body).unwrap();
        assert!(!text.contains("alice@example.com"), "{text}");
        assert!(text.contains("anon:"));
    }
}
