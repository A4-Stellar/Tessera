//! Kafka Schema Registry adapter for Soroban contract events.
//!
//! The API's indexer registers an Avro schema per `(contract_id, event_name,
//! schema_version)` in PostgreSQL (see `api/src/indexer/schema_registry.rs`).
//! This module serves those schemas over the Confluent Schema Registry HTTP
//! protocol so Kafka consumers — notably the Python analytics SDK — can
//! deserialize events with the exact writer schema and apply reader-schema
//! evolution rules.
//!
//! Supported endpoints:
//!
//! | Method | Path | Purpose |
//! |---|---|---|
//! | GET | `/subjects` | registered subjects |
//! | GET | `/subjects/{subject}/versions` | version numbers |
//! | GET | `/subjects/{subject}/versions/latest` | newest schema |
//! | GET | `/subjects/{subject}/versions/{v}` | one version |
//! | POST | `/subjects/{subject}/versions` | register `{"schema": "..."}` |
//! | GET | `/schemas/ids/{id}` | schema by global id |
//! | POST | `/compatibility/subjects/{subject}/versions/latest` | `{"schema": "..."}` |
//! | GET | `/config/{subject}` / `/config` | compatibility level |
//! | GET | `/health` | liveness |
//!
//! The transport is a deliberately small HTTP/1.1 implementation built on
//! `tokio::net`; the crate has no web-framework dependency. The store is a
//! trait, so the same protocol can be served from PostgreSQL in production
//! and from an in-memory map in tests.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};
use sqlx::PgPool;

/// Migration that creates the backing table. Shared with the API so both
/// processes agree on the schema of registered events.
pub const MIGRATION_SQL: &str = concat!(
    include_str!("../../../api/src/db/migrations/0005_contract_event_schemas.sql"),
    // Global schema ids are allocated by PostgreSQL so every replica and the
    // Python SDK see the same id for a given registered schema. The API's
    // migration intentionally stays storage-agnostic; the registry service
    // adds the column when it boots.
    "ALTER TABLE contract_event_schema ADD COLUMN IF NOT EXISTS id BIGSERIAL UNIQUE;",
);

/// Confluent-style compatibility levels, mirroring the API indexer's modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityLevel {
    Backward,
    Forward,
    Full,
    None,
}

impl CompatibilityLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backward => "BACKWARD",
            Self::Forward => "FORWARD",
            Self::Full => "FULL",
            Self::None => "NONE",
        }
    }

    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "FORWARD" | "FORWARD_TRANSITIVE" => Self::Forward,
            "FULL" | "FULL_TRANSITIVE" => Self::Full,
            "NONE" => Self::None,
            _ => Self::Backward,
        }
    }
}

/// One registered schema version, matching `contract_event_schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredSchema {
    pub subject: String,
    pub version: u32,
    pub global_id: u64,
    pub schema: Value,
    pub contract_id: String,
    pub event_name: String,
    pub contract_hash: String,
    pub compatibility: CompatibilityLevel,
    pub created_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("schema not found: {0}")]
    NotFound(String),
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
    #[error("schema is incompatible with the registered version: {0}")]
    Incompatible(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("store lock poisoned")]
    LockPoisoned,
}

pub type RegistryResult<T> = Result<T, RegistryError>;

/// Async persistence contract for the registry.
#[async_trait::async_trait]
pub trait SchemaStore: Send + Sync {
    async fn register(&self, schema: RegisteredSchema) -> RegistryResult<RegisteredSchema>;
    async fn latest(&self, subject: &str) -> RegistryResult<Option<RegisteredSchema>>;
    async fn version(
        &self,
        subject: &str,
        version: u32,
    ) -> RegistryResult<Option<RegisteredSchema>>;
    async fn versions(&self, subject: &str) -> RegistryResult<Vec<u32>>;
    async fn subjects(&self) -> RegistryResult<Vec<String>>;
    async fn by_id(&self, global_id: u64) -> RegistryResult<Option<RegisteredSchema>>;
    async fn set_compatibility(
        &self,
        subject: &str,
        compatibility: CompatibilityLevel,
    ) -> RegistryResult<()>;
    async fn compatibility(&self, subject: &str) -> RegistryResult<CompatibilityLevel>;
}

/// Fully functional in-memory store, used by tests and single-node deployments.
#[derive(Debug, Default)]
pub struct InMemorySchemaStore {
    inner: RwLock<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    subjects: BTreeMap<String, Vec<RegisteredSchema>>,
    by_id: BTreeMap<u64, RegisteredSchema>,
    compatibility: BTreeMap<String, CompatibilityLevel>,
    next_id: u64,
}

#[async_trait::async_trait]
impl SchemaStore for InMemorySchemaStore {
    async fn register(&self, schema: RegisteredSchema) -> RegistryResult<RegisteredSchema> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        let next_version = state
            .subjects
            .get(&schema.subject)
            .map_or(1, |versions| versions.len() as u32 + 1);
        if let Some(previous) = state.subjects.get(&schema.subject).and_then(|v| v.last()) {
            check_compatibility(&previous.schema, &schema.schema, schema.compatibility)
                .map_err(RegistryError::Incompatible)?;
        }
        let mut stored = schema;
        stored.global_id = state.next_id + 1;
        if stored.version == 0 {
            stored.version = next_version;
        }
        state.next_id += 1;
        state
            .subjects
            .entry(stored.subject.clone())
            .or_default()
            .push(stored.clone());
        state.by_id.insert(stored.global_id, stored.clone());
        Ok(stored)
    }

    async fn latest(&self, subject: &str) -> RegistryResult<Option<RegisteredSchema>> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state.subjects.get(subject).and_then(|v| v.last()).cloned())
    }

    async fn version(
        &self,
        subject: &str,
        version: u32,
    ) -> RegistryResult<Option<RegisteredSchema>> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state
            .subjects
            .get(subject)
            .and_then(|v| v.iter().find(|s| s.version == version))
            .cloned())
    }

    async fn versions(&self, subject: &str) -> RegistryResult<Vec<u32>> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state
            .subjects
            .get(subject)
            .map(|v| v.iter().map(|s| s.version).collect())
            .unwrap_or_default())
    }

    async fn subjects(&self) -> RegistryResult<Vec<String>> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state.subjects.keys().cloned().collect())
    }

    async fn by_id(&self, global_id: u64) -> RegistryResult<Option<RegisteredSchema>> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state.by_id.get(&global_id).cloned())
    }

    async fn set_compatibility(
        &self,
        subject: &str,
        compatibility: CompatibilityLevel,
    ) -> RegistryResult<()> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        state
            .compatibility
            .insert(subject.to_string(), compatibility);
        Ok(())
    }

    async fn compatibility(&self, subject: &str) -> RegistryResult<CompatibilityLevel> {
        let state = self.inner.read().map_err(|_| RegistryError::LockPoisoned)?;
        Ok(state
            .compatibility
            .get(subject)
            .copied()
            .unwrap_or(CompatibilityLevel::Backward))
    }
}

/// PostgreSQL-backed store over the API's `contract_event_schema` table.
#[derive(Debug, Clone)]
pub struct PostgresSchemaStore {
    pool: PgPool,
}

impl PostgresSchemaStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl SchemaStore for PostgresSchemaStore {
    async fn register(&self, schema: RegisteredSchema) -> RegistryResult<RegisteredSchema> {
        let latest = self.latest(&schema.subject).await?;
        if let Some(previous) = &latest {
            check_compatibility(&previous.schema, &schema.schema, schema.compatibility)
                .map_err(RegistryError::Incompatible)?;
        }
        let version = latest.map_or(1, |s| s.version + 1);
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO contract_event_schema \
             (contract_id, event_name, schema_version, schema_json, compatibility, contract_hash) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&schema.contract_id)
        .bind(&schema.event_name)
        .bind(i64::from(version))
        .bind(&schema.schema)
        .bind(schema.compatibility.as_str().to_ascii_lowercase())
        .bind(&schema.contract_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(RegisteredSchema {
            version,
            global_id: id as u64,
            ..schema
        })
    }

    async fn latest(&self, subject: &str) -> RegistryResult<Option<RegisteredSchema>> {
        let (contract_id, event_name) = parse_subject(subject)?;
        let row = sqlx::query_as::<_, (String, i64, Value, String, String, String)>(
            "SELECT contract_id, schema_version, schema_json, compatibility, contract_hash, created_at::TEXT \
             FROM contract_event_schema WHERE contract_id = $1 AND event_name = $2 \
             ORDER BY schema_version DESC LIMIT 1",
        )
        .bind(contract_id)
        .bind(event_name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| RegisteredSchema {
            subject: subject.to_string(),
            version: row.1 as u32,
            global_id: 0,
            schema: row.2,
            contract_id: row.0,
            event_name: event_name.to_string(),
            contract_hash: row.4,
            compatibility: CompatibilityLevel::parse(&row.3),
            created_at: row.5,
        })
        .pipe(Ok)
    }

    async fn version(
        &self,
        subject: &str,
        version: u32,
    ) -> RegistryResult<Option<RegisteredSchema>> {
        let (contract_id, event_name) = parse_subject(subject)?;
        let row = sqlx::query_as::<_, (Value, String, String, String)>(
            "SELECT schema_json, compatibility, contract_hash, created_at::TEXT \
             FROM contract_event_schema \
             WHERE contract_id = $1 AND event_name = $2 AND schema_version = $3",
        )
        .bind(contract_id)
        .bind(event_name)
        .bind(i64::from(version))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| RegisteredSchema {
            subject: subject.to_string(),
            version,
            global_id: 0,
            schema: row.0,
            contract_id: contract_id.to_string(),
            event_name: event_name.to_string(),
            contract_hash: row.2,
            compatibility: CompatibilityLevel::parse(&row.1),
            created_at: row.3,
        }))
    }

    async fn versions(&self, subject: &str) -> RegistryResult<Vec<u32>> {
        let (contract_id, event_name) = parse_subject(subject)?;
        let rows = sqlx::query_scalar::<_, i64>(
            "SELECT schema_version FROM contract_event_schema \
             WHERE contract_id = $1 AND event_name = $2 ORDER BY schema_version",
        )
        .bind(contract_id)
        .bind(event_name)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|v| v as u32).collect())
    }

    async fn subjects(&self) -> RegistryResult<Vec<String>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT DISTINCT contract_id, event_name FROM contract_event_schema \
             ORDER BY contract_id, event_name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(contract_id, event_name)| subject_name(&contract_id, &event_name))
            .collect())
    }

    async fn by_id(&self, global_id: u64) -> RegistryResult<Option<RegisteredSchema>> {
        let row = sqlx::query_as::<_, (String, String, i64, Value)>(
            "SELECT contract_id, event_name, schema_version, schema_json \
             FROM contract_event_schema WHERE id = $1",
        )
        .bind(global_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| RegisteredSchema {
            subject: subject_name(&row.0, &row.1),
            version: row.2 as u32,
            global_id,
            schema: row.3,
            contract_id: row.0,
            event_name: row.1,
            contract_hash: String::new(),
            compatibility: CompatibilityLevel::Backward,
            created_at: String::new(),
        }))
    }

    async fn set_compatibility(
        &self,
        _subject: &str,
        _compatibility: CompatibilityLevel,
    ) -> RegistryResult<()> {
        // Compatibility is stored with each schema version by the API indexer;
        // changing it here would not affect registered versions. Accepted for
        // protocol compatibility, with no persistent mutation.
        Ok(())
    }

    async fn compatibility(&self, subject: &str) -> RegistryResult<CompatibilityLevel> {
        let (contract_id, event_name) = parse_subject(subject)?;
        let level = sqlx::query_scalar::<_, String>(
            "SELECT compatibility FROM contract_event_schema \
             WHERE contract_id = $1 AND event_name = $2 ORDER BY schema_version DESC LIMIT 1",
        )
        .bind(contract_id)
        .bind(event_name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(level
            .map(|level| CompatibilityLevel::parse(&level))
            .unwrap_or(CompatibilityLevel::Backward))
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// `subject` is `<contract_id>-<event_name>-value`, matching Confluent's
/// TopicNameStrategy and the Python SDK's default subject.
pub fn subject_name(contract_id: &str, event_name: &str) -> String {
    format!("{contract_id}-{event_name}-value")
}

fn parse_subject(subject: &str) -> RegistryResult<(&str, &str)> {
    let rest = subject
        .strip_suffix("-value")
        .ok_or_else(|| RegistryError::InvalidSchema(format!("invalid subject '{subject}'")))?;
    let (contract_id, event_name) = rest
        .split_once('-')
        .ok_or_else(|| RegistryError::InvalidSchema(format!("invalid subject '{subject}'")))?;
    Ok((contract_id, event_name))
}

fn fields(schema: &Value) -> BTreeMap<String, &Value> {
    schema
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    let name = field.get("name")?.as_str()?;
                    Some((name.to_string(), field))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_nullable(ty: &Value) -> bool {
    ty.as_array()
        .is_some_and(|types| types.iter().any(|t| t == "null"))
}

/// Confluent-compatible compatibility check for record schemas.
///
/// A field added in the new schema must have a default (backward: new readers
/// can read old data). A field removed from the new schema must have had a
/// default in the old schema (forward: old readers can read new data). Full
/// requires both. This is the rule the schema-evolution test exercises with
/// the nullable `memo` field on `Transferred`.
pub fn check_compatibility(
    old: &Value,
    new: &Value,
    level: CompatibilityLevel,
) -> Result<(), String> {
    if level == CompatibilityLevel::None {
        return Ok(());
    }
    let old_fields = fields(old);
    let new_fields = fields(new);

    if matches!(
        level,
        CompatibilityLevel::Backward | CompatibilityLevel::Full
    ) {
        for (name, field) in &new_fields {
            if !old_fields.contains_key(name) {
                let has_default = field.get("default").is_some() || is_nullable(&field["type"]);
                if !has_default {
                    return Err(format!("field '{name}' was added without a default"));
                }
            }
        }
    }
    if matches!(
        level,
        CompatibilityLevel::Forward | CompatibilityLevel::Full
    ) {
        for (name, field) in &old_fields {
            if !new_fields.contains_key(name) {
                let has_default = field.get("default").is_some() || is_nullable(&field["type"]);
                if !has_default {
                    return Err(format!("field '{name}' was removed without a default"));
                }
            }
        }
    }
    Ok(())
}

/// Protocol request/response, decoupled from the HTTP transport for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRequest {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryResponse {
    pub status: u16,
    pub body: Value,
}

impl RegistryResponse {
    fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }
    fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({"error_code": status, "message": message.into()}),
        }
    }
}

/// Dispatch a Confluent Schema Registry request to the store.
pub async fn handle<S: SchemaStore>(store: &S, request: RegistryRequest) -> RegistryResponse {
    let path = request.path.as_str();
    let result: RegistryResult<RegistryResponse> = async {
        if path == "/health" {
            return Ok(RegistryResponse::ok(json!({"status": "ok"})));
        }
        if path == "/subjects" {
            return Ok(RegistryResponse::ok(json!(store.subjects().await?)));
        }
        if let Some(id) = path.strip_prefix("/schemas/ids/") {
            let id = id
                .parse::<u64>()
                .map_err(|_| RegistryError::InvalidSchema("invalid schema id".into()))?;
            let schema = store
                .by_id(id)
                .await?
                .ok_or_else(|| RegistryError::NotFound(format!("schema id {id}")))?;
            return Ok(RegistryResponse::ok(json!({
                "schema": schema.schema.to_string(),
            })));
        }
        if let Some(rest) = path.strip_prefix("/compatibility/subjects/") {
            let subject = rest.strip_suffix("/versions/latest").ok_or_else(|| {
                RegistryError::NotFound(format!("unsupported compatibility path {path}"))
            })?;
            let candidate = extract_schema(&request.body)?;
            let previous = store
                .latest(subject)
                .await?
                .ok_or_else(|| RegistryError::NotFound(subject.to_string()))?;
            let compatible =
                check_compatibility(&previous.schema, &candidate, previous.compatibility).is_ok();
            return Ok(RegistryResponse::ok(json!({"is_compatible": compatible})));
        }
        if path == "/config" || path.starts_with("/config/") {
            if request.method == "PUT" {
                let subject = path.strip_prefix("/config/").unwrap_or("");
                let level = request
                    .body
                    .as_ref()
                    .and_then(|body| body.get("compatibilityLevel"))
                    .and_then(Value::as_str)
                    .unwrap_or("BACKWARD");
                store
                    .set_compatibility(subject, CompatibilityLevel::parse(level))
                    .await?;
            }
            let subject = path.strip_prefix("/config/").unwrap_or("");
            return Ok(RegistryResponse::ok(json!({
                "compatibilityLevel": store.compatibility(subject).await?.as_str(),
            })));
        }
        if let Some(rest) = path.strip_prefix("/subjects/") {
            if request.method == "POST" {
                let (subject, suffix) = rest
                    .split_once("/versions")
                    .ok_or_else(|| RegistryError::NotFound(path.to_string()))?;
                if !suffix.is_empty() && suffix != "/" {
                    return Err(RegistryError::NotFound(path.to_string()));
                }
                let schema = extract_schema(&request.body)?;
                let (contract_id, event_name) = parse_subject(subject)?;
                let stored = store
                    .register(RegisteredSchema {
                        subject: subject.to_string(),
                        version: 0,
                        global_id: 0,
                        compatibility: store.compatibility(subject).await?,
                        schema,
                        contract_id: contract_id.to_string(),
                        event_name: event_name.to_string(),
                        contract_hash: String::new(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    })
                    .await?;
                return Ok(RegistryResponse::ok(json!({"id": stored.global_id})));
            }
            if let Some(subject) = rest.strip_suffix("/versions") {
                return Ok(RegistryResponse::ok(json!(store.versions(subject).await?)));
            }
            if let Some(subject) = rest.strip_suffix("/versions/latest") {
                let schema = store
                    .latest(subject)
                    .await?
                    .ok_or_else(|| RegistryError::NotFound(subject.to_string()))?;
                return Ok(RegistryResponse::ok(schema_view(&schema)));
            }
            if let Some((subject, version)) = rest.split_once("/versions/") {
                let version = version
                    .parse::<u32>()
                    .map_err(|_| RegistryError::InvalidSchema("invalid version".into()))?;
                let schema = store
                    .version(subject, version)
                    .await?
                    .ok_or_else(|| RegistryError::NotFound(path.to_string()))?;
                return Ok(RegistryResponse::ok(schema_view(&schema)));
            }
        }
        Err(RegistryError::NotFound(path.to_string()))
    }
    .await;

    match result {
        Ok(response) => response,
        Err(RegistryError::NotFound(message)) => RegistryResponse::error(404, message),
        Err(RegistryError::InvalidSchema(message)) => RegistryResponse::error(400, message),
        Err(RegistryError::Incompatible(message)) => RegistryResponse::error(409, message),
        Err(error) => RegistryResponse::error(500, error.to_string()),
    }
}

fn extract_schema(body: &Option<Value>) -> RegistryResult<Value> {
    let raw = body
        .as_ref()
        .and_then(|body| body.get("schema"))
        .and_then(Value::as_str)
        .ok_or_else(|| RegistryError::InvalidSchema("missing 'schema' string in body".into()))?;
    serde_json::from_str(raw).map_err(|e| RegistryError::InvalidSchema(e.to_string()))
}

fn schema_view(schema: &RegisteredSchema) -> Value {
    json!({
        "subject": schema.subject,
        "version": schema.version,
        "id": schema.global_id,
        "schema": schema.schema.to_string(),
    })
}

/// Serve the Confluent-compatible protocol over HTTP/1.1.
pub async fn serve<S: SchemaStore + 'static>(
    store: Arc<S>,
    addr: SocketAddr,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Kafka Schema Registry listening");
    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "schema registry accept failed");
                continue;
            }
        };
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(&mut socket, store.as_ref()).await {
                tracing::debug!(%error, "schema registry connection ended");
            }
        });
    }
}

async fn serve_connection<S: SchemaStore>(
    socket: &mut tokio::net::TcpStream,
    store: &S,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = position + 4;
            break;
        }
        if buffer.len() > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    }
    let header = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body_bytes = &buffer[header_end..(header_end + content_length).min(buffer.len())];
    let body = if body_bytes.is_empty() {
        None
    } else {
        serde_json::from_slice(body_bytes).ok()
    };

    let response = handle(store, RegistryRequest { method, path, body }).await;
    let payload = serde_json::to_vec(&response.body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/vnd.schemaregistry.v1+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status, reason, payload.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&payload).await?;
    socket.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = r#"{"type":"record","name":"Transferred","fields":[{"name":"from","type":"string"},{"name":"to","type":"string"},{"name":"amount","type":"long"}]}"#;
    const V2: &str = r#"{"type":"record","name":"Transferred","fields":[{"name":"from","type":"string"},{"name":"to","type":"string"},{"name":"amount","type":"long"},{"name":"memo","type":["null","string"],"default":null}]}"#;

    fn schema(body: &str) -> Value {
        json!({"schema": body})
    }

    async fn store_with_v1() -> InMemorySchemaStore {
        let store = InMemorySchemaStore::default();
        let response = handle(
            &store,
            RegistryRequest {
                method: "POST".into(),
                path: "/subjects/CCONTRACT-Transferred-value/versions".into(),
                body: Some(schema(V1)),
            },
        )
        .await;
        assert_eq!(response.status, 200, "{:?}", response.body);
        store
    }

    #[test]
    fn subject_round_trip() {
        assert_eq!(
            parse_subject(&subject_name("CCONTRACT", "Transferred")).unwrap(),
            ("CCONTRACT", "Transferred")
        );
    }

    #[test]
    fn nullable_added_field_is_compatible_in_full_mode() {
        let old: Value = serde_json::from_str(V1).unwrap();
        let new: Value = serde_json::from_str(V2).unwrap();
        assert_eq!(
            check_compatibility(&old, &new, CompatibilityLevel::Backward),
            Ok(())
        );
        assert_eq!(
            check_compatibility(&old, &new, CompatibilityLevel::Forward),
            Ok(())
        );
        assert_eq!(
            check_compatibility(&old, &new, CompatibilityLevel::Full),
            Ok(())
        );
    }

    #[tokio::test]
    async fn registered_schema_is_served_by_global_id() {
        let store = store_with_v1().await;
        let id = handle(
            &store,
            RegistryRequest {
                method: "GET".into(),
                path: "/schemas/ids/1".into(),
                body: None,
            },
        )
        .await;
        assert_eq!(id.status, 200);
        let served: Value = serde_json::from_str(id.body["schema"].as_str().unwrap()).unwrap();
        assert_eq!(served, serde_json::from_str::<Value>(V1).unwrap());
        assert_eq!(
            handle(
                &store,
                RegistryRequest {
                    method: "GET".into(),
                    path: "/schemas/ids/99".into(),
                    body: None,
                },
            )
            .await
            .status,
            404
        );
    }

    #[tokio::test]
    async fn nullable_field_registers_as_version_two() {
        let store = store_with_v1().await;
        let response = handle(
            &store,
            RegistryRequest {
                method: "POST".into(),
                path: "/subjects/CCONTRACT-Transferred-value/versions".into(),
                body: Some(schema(V2)),
            },
        )
        .await;
        assert_eq!(response.status, 200, "{:?}", response.body);
        assert_eq!(response.body["id"], 2);

        let latest = handle(
            &store,
            RegistryRequest {
                method: "GET".into(),
                path: "/subjects/CCONTRACT-Transferred-value/versions/latest".into(),
                body: None,
            },
        )
        .await;
        assert_eq!(latest.status, 200);
        let served: Value = serde_json::from_str(latest.body["schema"].as_str().unwrap()).unwrap();
        assert_eq!(served, serde_json::from_str::<Value>(V2).unwrap());
        assert_eq!(latest.body["version"], 2);

        let compatibility = handle(
            &store,
            RegistryRequest {
                method: "POST".into(),
                path: "/compatibility/subjects/CCONTRACT-Transferred-value/versions/latest".into(),
                body: Some(schema(V2)),
            },
        )
        .await;
        assert_eq!(compatibility.body["is_compatible"], true);
    }

    #[tokio::test]
    async fn incompatible_schema_is_rejected() {
        let store = store_with_v1().await;
        // A required field with no default breaks backward compatibility:
        // new readers would not know how to read old events.
        let bad = r#"{"type":"record","name":"Transferred","fields":[{"name":"from","type":"string"},{"name":"to","type":"string"},{"name":"amount","type":"long"},{"name":"tax","type":"string"}]}"#;
        let response = handle(
            &store,
            RegistryRequest {
                method: "POST".into(),
                path: "/subjects/CCONTRACT-Transferred-value/versions".into(),
                body: Some(schema(bad)),
            },
        )
        .await;
        assert_eq!(response.status, 409, "{:?}", response.body);
    }
}
