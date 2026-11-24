//! API version deprecation enforcement (Sunset / Link / 410 Gone).
//!
//! This module is deliberately **self-contained**: it only depends on `std`,
//! `axum` 0.7, `chrono`, and `serde_json`. `api` is a binary crate, so its
//! integration tests include this file directly with
//! `#[path = "../src/middleware/deprecation.rs"] mod deprecation;` — there is
//! no `lib.rs` to import from and no `crate::` path is available there.
//!
//! The configuration is the restricted TOML subset documented in
//! `api/deprecation_map.toml`. No TOML crate is used: a tiny line-oriented
//! parser below understands `[versions]`, `[[deprecated]]`, quoted string
//! values, blank lines, and `#` comments.

use std::{
    collections::BTreeMap,
    fmt,
    fs,
    path::Path,
    sync::Arc,
};

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderName, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use serde_json::{json, Value};

/// One deprecated path and its migration metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationEntry {
    /// Exact request path, or a prefix ending in `/*`.
    pub path: String,
    /// First day on which the endpoint is considered gone.
    pub sunset: NaiveDate,
    /// Replacement path advertised via `Link: ...; rel="successor-version"`.
    pub successor: String,
    /// Documentation URL for the migration guide.
    pub docs_url: String,
}

/// Parsed `deprecation_map.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationMap {
    /// API version name -> lifecycle status (e.g. `v1` -> `active`).
    pub versions: BTreeMap<String, String>,
    /// Deprecated paths, in configuration order.
    pub deprecated: Vec<DeprecationEntry>,
}

/// What to do with a request to a deprecated path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeprecationDecision {
    /// Still served: add `Sunset` and successor `Link` headers.
    Warning {
        sunset: NaiveDate,
        successor: String,
        docs_url: String,
    },
    /// Sunset date reached: answer `410 Gone` with a migration guide.
    Gone { docs_url: String },
}

/// A parse failure, carrying the 1-based line number in the source file.
///
/// I/O failures from [`DeprecationMap::from_file`] are reported with line 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    line: usize,
    message: String,
}

impl ParseError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }

    /// 1-based source line, or `0` when the failure was not line-specific.
    pub fn line(&self) -> usize {
        self.line
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ParseError {}

/// Strip a TOML `#` comment while respecting quoted strings and escapes.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, ch) {
            (Some(q), '\\') if q == '"' => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (None, '"') | (None, '\'') => quote = Some(ch),
            (None, '#') => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Decode a restricted TOML basic/literal string (only `\"`, `\\`, `\n`,
/// `\t` escapes are needed by this config format).
fn unquote(raw: &str, line: usize) -> Result<String, ParseError> {
    let raw = raw.trim();
    let (quote, inner) = match raw.as_bytes().first().copied() {
        Some(b'"') | Some(b'\'') => (raw.as_bytes()[0] as char, &raw[1..]),
        _ => {
            return Err(ParseError::new(
                line,
                format!("expected a quoted string, found `{raw}`"),
            ))
        }
    };
    let inner = inner.strip_suffix(quote).ok_or_else(|| {
        ParseError::new(line, format!("unterminated {quote}-quoted string: `{raw}`"))
    })?;

    if quote == '\'' {
        return Ok(inner.to_string());
    }

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                return Err(ParseError::new(
                    line,
                    format!("unsupported escape sequence `\\{other}`"),
                ))
            }
            None => return Err(ParseError::new(line, "trailing backslash in string")),
        }
    }
    Ok(out)
}

#[derive(Default)]
struct PendingEntry {
    path: Option<String>,
    sunset: Option<String>,
    successor: Option<String>,
    docs_url: Option<String>,
}

impl PendingEntry {
    fn finish(self, line: usize) -> Result<DeprecationEntry, ParseError> {
        let missing = |field: &str| {
            ParseError::new(
                line,
                format!("[[deprecated]] entry at line {line} is missing `{field}`"),
            )
        };
        let path = self.path.ok_or_else(|| missing("path"))?;
        let sunset_raw = self.sunset.ok_or_else(|| missing("sunset"))?;
        let successor = self.successor.ok_or_else(|| missing("successor"))?;
        let docs_url = self.docs_url.ok_or_else(|| missing("docs_url"))?;
        let sunset = NaiveDate::parse_from_str(&sunset_raw, "%Y-%m-%d").map_err(|_| {
            ParseError::new(
                line,
                format!("`sunset` must be an ISO-8601 date (YYYY-MM-DD), found `{sunset_raw}`"),
            )
        })?;
        Ok(DeprecationEntry {
            path,
            sunset,
            successor,
            docs_url,
        })
    }
}

impl DeprecationMap {
    /// Parse the restricted TOML format used by `api/deprecation_map.toml`.
    pub fn from_toml_str(input: &str) -> Result<Self, ParseError> {
        #[derive(PartialEq)]
        enum Section {
            None,
            Versions,
            Deprecated,
        }

        let mut section = Section::None;
        let mut versions = BTreeMap::new();
        let mut deprecated = Vec::new();
        let mut pending: Option<PendingEntry> = None;
        // Line number where the current `[[deprecated]]` table started, used
        // in errors about missing keys.
        let mut pending_line = 0usize;

        for (index, raw_line) in input.lines().enumerate() {
            let line_no = index + 1;
            let text = strip_comment(raw_line).trim();
            if text.is_empty() {
                continue;
            }

            if text == "[versions]" {
                if let Some(entry) = pending.take() {
                    deprecated.push(entry.finish(pending_line)?);
                }
                section = Section::Versions;
                continue;
            }
            if text == "[[deprecated]]" {
                if let Some(entry) = pending.take() {
                    deprecated.push(entry.finish(pending_line)?);
                }
                pending = Some(PendingEntry::default());
                pending_line = line_no;
                section = Section::Deprecated;
                continue;
            }
            if text.starts_with('[') {
                return Err(ParseError::new(
                    line_no,
                    format!("unsupported table header `{text}`"),
                ));
            }

            let (key, value) = text.split_once('=').ok_or_else(|| {
                ParseError::new(
                    line_no,
                    format!("expected `key = \"value\"`, found `{text}`"),
                )
            })?;
            let key = key.trim();
            let value = unquote(value, line_no)?;

            match section {
                Section::Versions => {
                    if key.is_empty() {
                        return Err(ParseError::new(line_no, "empty version name"));
                    }
                    versions.insert(key.to_string(), value);
                }
                Section::Deprecated => {
                    let entry = pending.as_mut().ok_or_else(|| {
                        ParseError::new(line_no, "key outside of a `[[deprecated]]` table")
                    })?;
                    let slot = match key {
                        "path" => &mut entry.path,
                        "sunset" => &mut entry.sunset,
                        "successor" => &mut entry.successor,
                        "docs_url" => &mut entry.docs_url,
                        other => {
                            return Err(ParseError::new(
                                line_no,
                                format!("unknown deprecation key `{other}`"),
                            ))
                        }
                    };
                    if slot.is_some() {
                        return Err(ParseError::new(
                            line_no,
                            format!("duplicate deprecation key `{key}`"),
                        ));
                    }
                    *slot = Some(value);
                }
                Section::None => {
                    return Err(ParseError::new(
                        line_no,
                        "key outside of a `[versions]` or `[[deprecated]]` table",
                    ))
                }
            }
        }

        if let Some(entry) = pending.take() {
            deprecated.push(entry.finish(pending_line)?);
        }

        Ok(Self {
            versions,
            deprecated,
        })
    }

    /// Parse a deprecation map from a file on disk.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ParseError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|err| {
            ParseError::new(
                0,
                format!("failed to read {}: {err}", path.display()),
            )
        })?;
        Self::from_toml_str(&contents)
    }

    /// Find the entry governing `path`: exact matches win, then `/*` prefixes.
    pub fn lookup(&self, path: &str) -> Option<&DeprecationEntry> {
        if let Some(entry) = self.deprecated.iter().find(|entry| entry.path == path) {
            return Some(entry);
        }
        self.deprecated.iter().find(|entry| {
            let Some(prefix) = entry.path.strip_suffix("/*") else {
                return false;
            };
            let Some(rest) = path.strip_prefix(prefix) else {
                return false;
            };
            // `/sep24/*` must not match a sibling path like `/sep24x/y`.
            rest.starts_with('/')
        })
    }

    /// Decide how to treat a request to `path` at the given date.
    pub fn deprecation_decision(
        &self,
        path: &str,
        now: NaiveDate,
    ) -> Option<DeprecationDecision> {
        let entry = self.lookup(path)?;
        if now < entry.sunset {
            Some(DeprecationDecision::Warning {
                sunset: entry.sunset,
                successor: entry.successor.clone(),
                docs_url: entry.docs_url.clone(),
            })
        } else {
            Some(DeprecationDecision::Gone {
                docs_url: entry.docs_url.clone(),
            })
        }
    }

    /// JSON payload for `GET /v1/meta/versions`.
    ///
    /// Each known version carries its lifecycle status and the earliest
    /// sunset date among paths belonging to it (`null` when none are staged).
    /// A `deprecated` array exposes the full per-path schedule so clients can
    /// plan migrations without scraping response headers.
    pub fn versions_response(&self) -> Value {
        let versions: Vec<Value> = self
            .versions
            .iter()
            .map(|(name, status)| {
                let prefix = format!("/{name}/");
                let sunset = self
                    .deprecated
                    .iter()
                    .filter(|entry| entry.path.starts_with(&prefix))
                    .map(|entry| entry.sunset)
                    .min()
                    .map(|date| date.to_string());
                json!({
                    "version": name,
                    "status": status,
                    "sunset": sunset,
                })
            })
            .collect();

        let deprecated: Vec<Value> = self
            .deprecated
            .iter()
            .map(|entry| {
                json!({
                    "path": entry.path,
                    "sunset": entry.sunset.to_string(),
                    "successor": entry.successor,
                    "docs_url": entry.docs_url,
                })
            })
            .collect();

        json!({
            "versions": versions,
            "deprecated": deprecated,
        })
    }
}

fn header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
}

/// `Sunset` is not one of `http` 0.2's predefined header constants.
const SUNSET_HEADER: HeaderName = HeaderName::from_static("sunset");

/// Axum middleware: annotate deprecated-but-served responses, and reject
/// endpoints whose sunset date has passed with `410 Gone`.
pub async fn deprecation_middleware(
    State(map): State<Arc<DeprecationMap>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let decision = map.deprecation_decision(&path, chrono::Utc::now().date_naive());

    match decision {
        None => next.run(req).await,
        Some(DeprecationDecision::Warning {
            sunset,
            successor,
            docs_url: _,
        }) => {
            let mut response = next.run(req).await;
            let headers = response.headers_mut();
            if let Some(value) = header_value(&sunset.to_string()) {
                headers.insert(SUNSET_HEADER, value);
            }
            let link = format!("<{successor}>; rel=\"successor-version\"");
            if let Some(value) = header_value(&link) {
                headers.insert(header::LINK, value);
            }
            response
        }
        Some(DeprecationDecision::Gone { docs_url }) => {
            let body = json!({
                "error": "gone",
                "message": format!(
                    "The endpoint `{path}` is no longer available after its sunset date. \
                     See the migration guide for the current endpoint."
                ),
                "migration_guide": docs_url,
            });
            let mut response = (StatusCode::GONE, Json(body)).into_response();
            let link = format!("<{docs_url}>; rel=\"migration-guide\"");
            if let Some(value) = header_value(&link) {
                response.headers_mut().insert(header::LINK, value);
            }
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOML: &str = r#"
        # leading comment
        [versions]
        v1 = "active" # inline comment
        v2 = "beta"

        [[deprecated]]
        path = "/version"
        sunset = "2027-06-30"
        successor = "/v1/meta/versions"
        docs_url = "https://example.test/migration"

        [[deprecated]]
        path = "/sep24/*"
        sunset = "2027-09-30"
        successor = "/v1/sep24/*"
        docs_url = "https://example.test/sep24"

        [[deprecated]]
        path = "/v1/legacy/reports"
        sunset = "2025-01-01"
        successor = "/v1/reports"
        docs_url = "https://example.test/reports"
    "#;

    fn map() -> DeprecationMap {
        DeprecationMap::from_toml_str(TEST_TOML).expect("valid test TOML")
    }

    #[test]
    fn parses_versions_and_deprecated_entries() {
        let parsed = map();
        assert_eq!(parsed.versions.get("v1").map(String::as_str), Some("active"));
        assert_eq!(parsed.versions.get("v2").map(String::as_str), Some("beta"));
        assert_eq!(parsed.deprecated.len(), 3);
        assert_eq!(parsed.deprecated[0].path, "/version");
        assert_eq!(
            parsed.deprecated[0].sunset,
            NaiveDate::from_ymd_opt(2027, 6, 30).unwrap()
        );
        assert_eq!(parsed.deprecated[0].successor, "/v1/meta/versions");
        assert_eq!(
            parsed.deprecated[0].docs_url,
            "https://example.test/migration"
        );
    }

    #[test]
    fn reports_line_numbers_for_bad_input() {
        let error = DeprecationMap::from_toml_str("[versions]\nv1 = 2\n")
            .expect_err("numbers are not supported");
        assert_eq!(error.line(), 2);
        assert!(error.to_string().contains("line 2"));

        let error = DeprecationMap::from_toml_str("[[deprecated]]\npath = \"/x\"\n")
            .expect_err("missing fields");
        assert_eq!(error.line(), 1);
        assert!(error.message().contains("sunset"));

        let error = DeprecationMap::from_toml_str("[unknown]\nx = \"y\"\n")
            .expect_err("unsupported table");
        assert_eq!(error.line(), 1);
    }

    #[test]
    fn exact_matches_win_over_prefix_matches() {
        let parsed = map();
        assert_eq!(parsed.lookup("/version").unwrap().successor, "/v1/meta/versions");
        assert_eq!(
            parsed.lookup("/sep24/transaction").unwrap().path,
            "/sep24/*"
        );
        assert!(parsed.lookup("/sep24x/transaction").is_none());
        assert!(parsed.lookup("/health").is_none());
        assert!(parsed.lookup("/metrics").is_none());
        assert!(parsed.lookup("/.well-known/stellar.toml").is_none());
        assert!(parsed.lookup("/graphql").is_none());
    }

    #[test]
    fn decisions_flip_on_the_sunset_date() {
        let parsed = map();
        let before = NaiveDate::from_ymd_opt(2027, 6, 29).unwrap();
        let on = NaiveDate::from_ymd_opt(2027, 6, 30).unwrap();
        assert_eq!(
            parsed.deprecation_decision("/version", before),
            Some(DeprecationDecision::Warning {
                sunset: on,
                successor: "/v1/meta/versions".to_string(),
                docs_url: "https://example.test/migration".to_string(),
            })
        );
        assert_eq!(
            parsed.deprecation_decision("/version", on),
            Some(DeprecationDecision::Gone {
                docs_url: "https://example.test/migration".to_string(),
            })
        );
        assert_eq!(parsed.deprecation_decision("/health", on), None);
    }

    #[test]
    fn versions_response_lists_statuses_sunsets_and_paths() {
        let value = map().versions_response();
        assert_eq!(value["versions"][0]["version"], "v1");
        assert_eq!(value["versions"][0]["status"], "active");
        // `/v1/legacy/reports` has the earliest v1 sunset.
        assert_eq!(value["versions"][0]["sunset"], "2025-01-01");
        assert_eq!(value["versions"][1]["version"], "v2");
        assert_eq!(value["versions"][1]["status"], "beta");
        assert!(value["versions"][1]["sunset"].is_null());
        assert_eq!(value["deprecated"][0]["path"], "/version");
        assert_eq!(value["deprecated"][0]["sunset"], "2027-06-30");
    }

    #[test]
    fn parses_the_real_configuration_file() {
        let parsed = DeprecationMap::from_file(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/deprecation_map.toml"
        ))
        .expect("deprecation_map.toml must parse");
        assert!(parsed.deprecated.iter().all(|entry| !entry.docs_url.is_empty()));
        assert!(parsed.lookup("/health").is_none());
        assert!(parsed.lookup("/metrics").is_none());
        assert!(parsed.lookup("/graphql").is_none());
    }
}
