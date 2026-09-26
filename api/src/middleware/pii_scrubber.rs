//! Automated data scrubbing & PII redaction (issue #103).
//!
//! Contract memos and other free-text fields are user controlled, so an
//! email address, IP address or key can end up in an API response or a log
//! line by accident. This module redacts them *before* they leave the process:
//!
//! * [`scrub`] - the pure pattern-matching scrubber.
//! * [`pii_scrub_middleware`] - an axum middleware that scrubs outgoing
//!   `application/json` and `text/*` response bodies.
//! * [`ScrubbingMakeWriter`] - a `tracing_subscriber` writer that scrubs every
//!   log line before it reaches stdout.
//!
//! What is redacted:
//!
//! | Kind | Example | Result |
//! |------|---------|--------|
//! | Email | `alice@example.com` | `a***e@example.com` |
//! | IPv4 / IPv6 | `203.0.113.7`, `2001:db8::1` | `[REDACTED_IP]` |
//! | Stellar secret seed | `S` + 55 base32 chars | `[REDACTED_PRIVATE_KEY]` |
//! | PEM private key block | `-----BEGIN ... PRIVATE KEY-----` | `[REDACTED_PRIVATE_KEY]` |
//! | Labelled hex key | `"secret_key":"<64 hex>"` | value replaced |
//!
//! Loopback and unspecified addresses (`127.0.0.1`, `0.0.0.0`, `::1`, `::`)
//! are left alone: they identify no person and appear in operational logs.
//! Public Stellar identifiers (`G...` accounts, `C...` contracts, hashes) are
//! *not* PII secrets and are preserved so the API stays usable.
//!
//! Limits, stated plainly: this is pattern based. It cannot recognise
//! free-form personal *names* or postal addresses (there is no reliable regex
//! for them), and an email/IP split across JSON string boundaries is not
//! reassembled. Set `RWA_PII_SCRUBBING=off` to disable (default: on).

use std::borrow::Cow;
use std::io::{self, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use regex::{Captures, Regex};

pub const REDACTED_IP: &str = "[REDACTED_IP]";
pub const REDACTED_KEY: &str = "[REDACTED_PRIVATE_KEY]";

/// Largest response body the middleware will buffer for scrubbing.
const MAX_SCRUB_BODY_BYTES: usize = 16 * 1024 * 1024;

struct Patterns {
    pem: Regex,
    labelled_hex: Regex,
    stellar_secret: Regex,
    email: Regex,
    ipv4: Regex,
    ipv6: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        pem: Regex::new(
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        )
        .expect("valid regex"),
        // `secret_key: <64 hex>` / `"private_key":"<64 hex>"` / `seed=<64 hex>`
        labelled_hex: Regex::new(
            r#"(?i)((?:secret|private|priv|seed)[a-z_\- ]{0,12}["']?\s*[:=]\s*["']?)[0-9a-f]{64}\b"#,
        )
        .expect("valid regex"),
        stellar_secret: Regex::new(r"\bS[A-Z2-7]{55}\b").expect("valid regex"),
        // `*` is in the local-part class so already-masked emails re-match
        // and re-mask to themselves (idempotence).
        email: Regex::new(r"[A-Za-z0-9._%+*\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}")
            .expect("valid regex"),
        ipv4: Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").expect("valid regex"),
        ipv6: Regex::new(r"[0-9A-Fa-f:]*:[0-9A-Fa-f:]*:[0-9A-Fa-f:.]*").expect("valid regex"),
    })
}

/// Mask an email address as `a***b@domain.com` (first and last character of
/// the local part kept). One-character local parts become `a***@domain`.
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return email.to_string();
    };
    let chars: Vec<char> = local.chars().collect();
    let masked = match chars.as_slice() {
        [] => "***".to_string(),
        [c] => format!("{c}***"),
        [first, rest @ ..] if rest.len() <= 4 && local[first.len_utf8()..].starts_with("***")
            && rest.iter().filter(|c| **c != '*').count() <= 1 =>
        {
            // Already masked (`a***` / `a***b`): keep as is (idempotence).
            local.to_string()
        }
        [first, .., last] => format!("{first}***{last}"),
    };
    format!("{masked}@{domain}")
}

fn should_redact_v4(s: &str) -> bool {
    match s.parse::<Ipv4Addr>() {
        Ok(ip) => !(ip.is_loopback() || ip.is_unspecified()),
        // e.g. `999.1.1.1` or a leading-zero quad: not an address.
        Err(_) => false,
    }
}

fn should_redact_v6(s: &str) -> bool {
    // At least two colons are guaranteed by the regex; anything that is not a
    // valid IPv6 literal (times such as `12:30:45`, ratios) is left alone.
    match s.parse::<Ipv6Addr>() {
        Ok(ip) => !(ip.is_loopback() || ip.is_unspecified()),
        Err(_) => false,
    }
}

/// Redact PII from `input`. Returns the input unchanged (borrowed) when
/// nothing matched. Idempotent: `scrub(scrub(x)) == scrub(x)`.
pub fn scrub(input: &str) -> Cow<'_, str> {
    let p = patterns();
    let mut out: Cow<'_, str> = Cow::Borrowed(input);

    // Secrets first, so a key is never partially matched by a later pattern.
    out = replace(out, &p.pem, |_| REDACTED_KEY.to_string());
    out = replace(out, &p.labelled_hex, |c| format!("{}{}", &c[1], REDACTED_KEY));
    out = replace(out, &p.stellar_secret, |_| REDACTED_KEY.to_string());
    out = replace(out, &p.email, |c| mask_email(&c[0]));
    out = replace(out, &p.ipv4, |c| {
        if should_redact_v4(&c[0]) {
            REDACTED_IP.to_string()
        } else {
            c[0].to_string()
        }
    });
    out = replace(out, &p.ipv6, |c| {
        let m = &c[0];
        if should_redact_v6(m) {
            REDACTED_IP.to_string()
        } else {
            m.to_string()
        }
    });
    out
}

fn replace<'a>(
    text: Cow<'a, str>,
    re: &Regex,
    f: impl Fn(&Captures<'_>) -> String,
) -> Cow<'a, str> {
    match re.replace_all(&text, |c: &Captures<'_>| f(c)) {
        Cow::Borrowed(_) => text,
        Cow::Owned(s) => Cow::Owned(s),
    }
}

fn scrubbing_enabled() -> bool {
    !matches!(
        std::env::var("RWA_PII_SCRUBBING").as_deref(),
        Ok("off") | Ok("false") | Ok("0")
    )
}

fn is_scrubbable(content_type: Option<&HeaderValue>) -> bool {
    content_type
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v.starts_with("application/json")
                || v.starts_with("text/")
                || v.contains("+json")
        })
        .unwrap_or(false)
}

/// Axum middleware: scrub outgoing JSON/text response bodies.
pub async fn pii_scrub_middleware(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    if !scrubbing_enabled()
        || resp.status() == StatusCode::SWITCHING_PROTOCOLS
        || resp.headers().contains_key(header::CONTENT_ENCODING)
        || !is_scrubbable(resp.headers().get(header::CONTENT_TYPE))
    {
        return resp;
    }

    let (mut parts, body) = resp.into_parts();
    let bytes = match to_bytes(body, MAX_SCRUB_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "response body too large or unreadable for PII scrubbing");
            // Fail closed: never emit a body we could not scrub.
            return (StatusCode::INTERNAL_SERVER_ERROR, "response could not be scrubbed")
                .into_response();
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        // Declared text/JSON but not UTF-8: fail closed rather than leak.
        return (StatusCode::INTERNAL_SERVER_ERROR, "response could not be scrubbed")
            .into_response();
    };
    match scrub(text) {
        Cow::Borrowed(_) => {
            parts.headers.remove(header::CONTENT_LENGTH);
            let len = bytes.len();
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            Response::from_parts(parts, Body::from(bytes))
        }
        Cow::Owned(clean) => {
            metrics::counter!("rwa_pii_responses_scrubbed_total").increment(1);
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(clean.len()));
            Response::from_parts(parts, Body::from(clean))
        }
    }
}

/// `tracing_subscriber` writer factory that scrubs each log record.
#[derive(Clone, Copy, Default)]
pub struct ScrubbingMakeWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ScrubbingMakeWriter {
    type Writer = ScrubbingWriter<io::Stdout>;
    fn make_writer(&'a self) -> Self::Writer {
        ScrubbingWriter::new(io::stdout())
    }
}

/// Buffers one log record and writes the scrubbed text to `inner` on flush /
/// drop. `tracing_subscriber` creates one writer per event, so a record is
/// never split across scrub boundaries.
pub struct ScrubbingWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> ScrubbingWriter<W> {
    pub fn new(inner: W) -> Self {
        ScrubbingWriter {
            inner,
            buf: Vec::new(),
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf);
        let out: Cow<'_, str> = if scrubbing_enabled() { scrub(&text) } else { text };
        let res = self.inner.write_all(out.as_bytes());
        self.buf.clear();
        res
    }
}

impl<W: Write> Write for ScrubbingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for ScrubbingWriter<W> {
    fn drop(&mut self) {
        let _ = self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt as _;

    const SECRET_SEED: &str = "SAV76USXIJOBMEQXPANUOQM6F5LIOTLPDIDVRJBFFE2MDJXG24TAPUU7";

    #[test]
    fn masks_emails_in_required_format() {
        assert_eq!(scrub("alice@example.com"), "a***e@example.com");
        assert_eq!(scrub("contact: bob.smith+tag@mail.example.co.uk!"), "contact: b***g@mail.example.co.uk!");
        assert_eq!(scrub("x@ab.io"), "x***@ab.io");
        assert_eq!(scrub("ab@ab.io"), "a***b@ab.io");
    }

    #[test]
    fn masks_ipv4_and_ipv6_but_keeps_loopback() {
        assert_eq!(scrub("from 203.0.113.7 ok"), format!("from {REDACTED_IP} ok"));
        assert_eq!(scrub("client=2001:db8::ff00:42:8329"), format!("client={REDACTED_IP}"));
        assert_eq!(scrub("fe80::1%eth0"), format!("{REDACTED_IP}%eth0"));
        assert_eq!(scrub("listening 127.0.0.1:8080 and 0.0.0.0:80 and ::1"), "listening 127.0.0.1:8080 and 0.0.0.0:80 and ::1");
    }

    #[test]
    fn leaves_non_addresses_alone() {
        for s in [
            "2026-09-26T08:48:13.123Z",
            "12:30:45",
            "version 1.2.3",
            "999.999.999.999",
            "ratio 3:2:1",
        ] {
            assert_eq!(scrub(s), s, "{s}");
        }
    }

    #[test]
    fn redacts_private_keys() {
        let out = scrub(&format!("seed {SECRET_SEED} end"));
        assert_eq!(out, format!("seed {REDACTED_KEY} end"));
        let pem = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkq\n-----END PRIVATE KEY-----";
        assert_eq!(scrub(pem), REDACTED_KEY);
        let hex = "a".repeat(64);
        let j = format!(r#"{{"secret_key":"{hex}","tx":"{hex}"}}"#);
        let out = scrub(&j);
        assert!(out.contains(&format!(r#""secret_key":"{REDACTED_KEY}""#)));
        assert!(out.contains(&format!(r#""tx":"{hex}""#)), "unlabelled hashes preserved");
    }

    #[test]
    fn preserves_public_stellar_identifiers() {
        let g = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA";
        let c = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3";
        let h = "0123456789abcdef".repeat(4);
        let s = format!("{g} {c} {h}");
        assert_eq!(scrub(&s), s);
    }

    #[test]
    fn scrubbing_is_idempotent_and_keeps_json_valid() {
        let raw = format!(
            r#"{{"memo":"mail alice@example.com from 198.51.100.4 / 2001:db8::1 key {SECRET_SEED}","n":[1,2,{{"e":"z@q.io"}}]}}"#
        );
        let once = scrub(&raw).into_owned();
        let twice = scrub(&once).into_owned();
        assert_eq!(once, twice);
        let v: serde_json::Value = serde_json::from_str(&once).expect("still valid JSON");
        assert_eq!(v["n"][2]["e"], "z***@q.io");
    }

    /// GDPR Art. 5(1)(c) / CCPA data minimisation: no raw personal data may
    /// survive in output, across many representative shapes.
    #[test]
    fn compliance_no_raw_pii_survives() {
        let emails = ["jane.doe@example.com", "j@x.org", "first_last+news@sub.domain.io"];
        let ips = ["8.8.8.8", "192.168.1.20", "2606:4700:4700::1111", "2001:db8:85a3:0:0:8a2e:370:7334"];
        let mut samples = Vec::new();
        for e in emails {
            samples.push(format!("memo={e}"));
            samples.push(format!(r#"{{"note":"pay {e} now"}}"#));
            samples.push(format!("user <{e}> logged in"));
        }
        for ip in ips {
            samples.push(format!("peer {ip}"));
            samples.push(format!(r#"{{"ip":"{ip}"}}"#));
            samples.push(format!("addr=[{ip}]:443"));
        }
        samples.push(format!("key={SECRET_SEED}"));
        for s in samples {
            let out = scrub(&s);
            for e in emails {
                assert!(!out.contains(e), "email survived in {out}");
            }
            for ip in ips {
                assert!(!out.contains(ip), "ip survived in {out}");
            }
            assert!(!out.contains(SECRET_SEED), "seed survived in {out}");
        }
    }

    #[test]
    fn compliance_scrubbing_only_removes_never_adds_pii() {
        let out = scrub("mail alice@example.com").into_owned();
        // Only the first/last local-part characters and the domain remain.
        assert!(!out.contains("alice"));
        assert!(out.contains("a***e@example.com"));
    }

    #[tokio::test]
    async fn middleware_scrubs_json_responses_and_fixes_content_length() {
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    Json(serde_json::json!({"memo": "hi bob@example.com from 203.0.113.9"}))
                }),
            )
            .route("/bin", get(|| async { ([(header::CONTENT_TYPE, "application/octet-stream")], "bob@example.com") }))
            .layer(axum::middleware::from_fn(pii_scrub_middleware));

        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let declared: usize = resp.headers()[header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap();
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert_eq!(declared, body.len());
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("b***b@example.com"), "{text}");
        assert!(text.contains(REDACTED_IP));
        assert!(!text.contains("203.0.113.9"));

        // Non-text content types are not touched.
        let resp = app
            .oneshot(Request::builder().uri("/bin").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..], b"bob@example.com");
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_writer_scrubs_records() {
        let sink = Sink::default();
        {
            let mut w = ScrubbingWriter::new(sink.clone());
            // A record arriving in several writes is scrubbed as one unit.
            w.write_all(b"INFO login carol@exa").unwrap();
            w.write_all(b"mple.com from 198.51.100.77\n").unwrap();
        }
        let out = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(out, format!("INFO login c***l@example.com from {REDACTED_IP}\n"));
    }

    #[test]
    fn tracing_output_is_scrubbed_end_to_end() {
        use tracing_subscriber::fmt::MakeWriter;
        #[derive(Clone)]
        struct MW(Sink);
        impl<'a> MakeWriter<'a> for MW {
            type Writer = ScrubbingWriter<Sink>;
            fn make_writer(&'a self) -> Self::Writer {
                ScrubbingWriter::new(self.0.clone())
            }
        }
        let sink = Sink::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(MW(sink.clone()))
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(sub, || {
            tracing::info!(email = "dave@example.com", ip = "203.0.113.55", "user event");
        });
        let out = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert!(out.contains("user event"));
        assert!(!out.contains("dave@example.com") && !out.contains("203.0.113.55"), "{out}");
    }
}
