//! Versioned event-schema registry for Soroban contracts (event schema registry).
//!
//! Soroban emits contract events as untyped `ScVal` maps. To turn those into
//! stable JSON for the API we keep an Avro schema per `(contract_id,
//! event_name)` and bump its version whenever the contract WASM changes.
//!
//! This module owns three things:
//!
//! * [`AvroSchema`] — a thin, allocation-light view over an Avro record
//!   represented as `serde_json::Value`, plus Confluent-style compatibility
//!   checking ([`compat_check`]).
//! * [`SchemaRegistryStore`] — the async persistence contract, with a
//!   fully functional [`InMemorySchemaRegistry`] and a
//!   [`PostgresSchemaRegistry`] backed by migration `0005`.
//! * ABI discovery ([`AbiInspector`], [`CliAbiInspector`],
//!   [`StaticAbiInspector`]) and [`sync_contract_schema`], which ties the two
//!   together: a new contract hash produces new schema versions, an unchanged
//!   hash is a no-op.
//!
//! The module deliberately avoids `crate::` paths so it can also be included
//! directly with `#[path = "..."] mod schema_registry;` from an integration
//! test.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Migration that creates the backing table. Kept here so callers can run the
/// schema bootstrap without reaching into `main.rs`.
pub const MIGRATION_SQL: &str = include_str!("../db/migrations/0005_contract_event_schemas.sql");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why two schemas are not compatible.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Incompatibility {
    #[error("field '{0}' was added without a default value")]
    AddedFieldWithoutDefault(String),
    #[error("field '{0}' was removed but the old schema has no default for it")]
    RemovedFieldWithoutDefault(String),
    #[error("field '{0}' changed type incompatibly (old: {1}, new: {2})")]
    TypeChange(String, String, String),
}

/// Error surface shared by the registry stores and ABI discovery.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("schema compatibility check failed: {0}")]
    Incompatible(#[from] Incompatibility),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("ABI inspection failed: {0}")]
    Abi(String),
    #[error("invalid ABI specification: {0}")]
    InvalidAbi(String),
    #[error("invalid Avro schema: {0}")]
    InvalidSchema(String),
    #[error("registry lock is poisoned")]
    LockPoisoned,
}

pub type SchemaResult<T> = Result<T, SchemaError>;

// ---------------------------------------------------------------------------
// Compatibility modes
// ---------------------------------------------------------------------------

/// Confluent-style compatibility mode stored alongside each schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compatibility {
    /// New schema can read data written with the old schema.
    Backward,
    /// Old schema can read data written with the new schema.
    Forward,
    /// Both directions.
    Full,
    /// No compatibility enforcement.
    None,
}

impl Compatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Compatibility::Backward => "backward",
            Compatibility::Forward => "forward",
            Compatibility::Full => "full",
            Compatibility::None => "none",
        }
    }
}

impl FromStr for Compatibility {
    type Err = SchemaError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "backward" | "backward_transitive" => Ok(Compatibility::Backward),
            "forward" | "forward_transitive" => Ok(Compatibility::Forward),
            "full" | "full_transitive" => Ok(Compatibility::Full),
            "none" | "" => Ok(Compatibility::None),
            other => Err(SchemaError::InvalidSchema(format!(
                "unknown compatibility mode '{other}'"
            ))),
        }
    }
}

impl std::fmt::Display for Compatibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Stored record
// ---------------------------------------------------------------------------

/// One immutable version of a contract event's Avro schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractEventSchema {
    pub contract_id: String,
    pub event_name: String,
    pub schema_version: u32,
    pub schema_json: Value,
    pub compatibility: Compatibility,
    pub contract_hash: String,
    /// RFC3339 timestamp.
    pub created_at: String,
}

impl ContractEventSchema {
    pub fn new(
        contract_id: impl Into<String>,
        event_name: impl Into<String>,
        schema_json: Value,
        compatibility: Compatibility,
        contract_hash: impl Into<String>,
    ) -> Self {
        Self {
            contract_id: contract_id.into(),
            event_name: event_name.into(),
            schema_version: 0,
            schema_json,
            compatibility,
            contract_hash: contract_hash.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Convenience wrapper for compatibility checking against this schema.
    pub fn check_compatibility(
        &self,
        new: &AvroSchema,
    ) -> std::result::Result<(), Incompatibility> {
        compat_check(&AvroSchema::from_json(self.schema_json.clone()), new, self.compatibility)
    }
}

// ---------------------------------------------------------------------------
// Avro schema view + compatibility
// ---------------------------------------------------------------------------

/// A thin view over an Avro `record` encoded as JSON:
/// `{ "type": "record", "name": ..., "fields": [ { "name": ..., "type": ...,
/// "default"?: ... } ] }`.
///
/// `type` may be a primitive string, a named type, a union expressed as a
/// JSON array (e.g. `["null", "string"]`), a nested record object, or an
/// array/map object.
#[derive(Debug, Clone, PartialEq)]
pub struct AvroSchema {
    pub raw: Value,
}

impl From<AvroSchema> for Value {
    fn from(schema: AvroSchema) -> Self {
        schema.raw
    }
}

impl AvroSchema {
    /// Build a record schema from a list of Avro field objects.
    pub fn new(name: impl Into<String>, fields: Vec<Value>) -> Self {
        Self {
            raw: json!({
                "type": "record",
                "name": name.into(),
                "fields": fields,
            }),
        }
    }

    pub fn from_json(raw: Value) -> Self {
        Self { raw }
    }

    pub fn name(&self) -> &str {
        self.raw.get("name").and_then(Value::as_str).unwrap_or("")
    }

    pub fn fields(&self) -> &[Value] {
        self.raw
            .get("fields")
            .and_then(Value::as_array)
            .map(|fields| fields.as_slice())
            .unwrap_or(&[])
    }

    /// Look up a field definition by name.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields()
            .iter()
            .find(|field| field.get("name").and_then(Value::as_str) == Some(name))
    }

    /// The `type` member of an Avro field object.
    pub fn field_type(field: &Value) -> Option<&Value> {
        field.get("type").or_else(|| field.get("type_name"))
    }

    /// The `default` member of an Avro field object, when present.
    pub fn field_default(field: &Value) -> Option<&Value> {
        field.get("default")
    }

    pub fn field_type_by_name(&self, name: &str) -> Option<&Value> {
        self.field(name).and_then(Self::field_type)
    }

    pub fn field_default_by_name(&self, name: &str) -> Option<&Value> {
        self.field(name).and_then(Self::field_default)
    }

    /// Union members, or a single-element slice for a non-union type.
    pub fn union_members(ty: &Value) -> Vec<&Value> {
        match ty {
            Value::Array(members) => members.iter().collect(),
            other => vec![other],
        }
    }

    /// Primitive/named type name of a (possibly record-shaped) type.
    pub fn type_name(ty: &Value) -> Option<&str> {
        match ty {
            Value::String(name) => Some(name.as_str()),
            Value::Object(obj) => obj
                .get("type")
                .and_then(Value::as_str)
                .or_else(|| obj.get("name").and_then(Value::as_str)),
            _ => None,
        }
    }

    /// Whether the type is a union that admits `null`.
    pub fn is_nullable(ty: &Value) -> bool {
        Self::union_members(ty)
            .iter()
            .any(|member| Self::type_name(member) == Some("null"))
    }

    /// Resolve a named type reference by searching the whole schema tree.
    pub fn resolve_named(&self, name: &str) -> Option<&Value> {
        find_named(&self.raw, name)
    }

    /// Check `new` against `self` under `mode`.
    pub fn check(&self, new: &Self, mode: Compatibility) -> std::result::Result<(), Incompatibility> {
        compat_check(self, new, mode)
    }
}

fn find_named<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    match node {
        Value::Object(obj) => {
            if obj.get("name").and_then(Value::as_str) == Some(name) {
                return Some(node);
            }
            obj.values().find_map(|child| find_named(child, name))
        }
        Value::Array(items) => items.iter().find_map(|item| find_named(item, name)),
        _ => None,
    }
}

/// Compare two type expressions structurally for equality of a single
/// member. Unions are decomposed first by [`union_members`].
fn member_key(ty: &Value) -> String {
    match ty {
        Value::String(name) => format!("s:{name}"),
        other => format!("j:{other}"),
    }
}

/// `reader` can decode data written with `writer` iff every writer alternative
/// is understood by the reader. Widening a field to a union that still
/// contains the old type is therefore allowed.
fn accepts(reader: &Value, writer: &Value) -> bool {
    let reader_members = AvroSchema::union_members(reader);
    let writer_members = AvroSchema::union_members(writer);
    let reader_keys: Vec<String> = reader_members.iter().map(|m| member_key(m)).collect();
    writer_members
        .iter()
        .all(|member| reader_keys.contains(&member_key(member)))
}

fn field_names(schema: &AvroSchema) -> HashMap<String, &Value> {
    schema
        .fields()
        .iter()
        .filter_map(|field| {
            field
                .get("name")
                .and_then(Value::as_str)
                .map(|name| (name.to_string(), field))
        })
        .collect()
}

/// Confluent-style compatibility check.
///
/// * **BACKWARD** (new reads old): a field added by `new` needs a default in
///   `new`; a field deleted by `new` is simply skipped; shared fields may only
///   widen (`new` must accept the old type).
/// * **FORWARD** (old reads new): a field deleted by `new` needs a default in
///   `old`; added fields are skipped by the old reader; shared fields may only
///   widen from the old reader's point of view.
/// * **FULL** requires both directions.
/// * **NONE** skips checking.
///
/// A nullable added field (`["null", T]` with `default: null`) therefore
/// passes both BACKWARD and FULL: old data lacks the field and the reader uses
/// the default, while old readers ignore the extra field in new data.
pub fn compat_check(
    old: &AvroSchema,
    new: &AvroSchema,
    mode: Compatibility,
) -> std::result::Result<(), Incompatibility> {
    match mode {
        Compatibility::None => return Ok(()),
        Compatibility::Backward => check_backward(old, new)?,
        Compatibility::Forward => check_forward(old, new)?,
        Compatibility::Full => {
            check_backward(old, new)?;
            check_forward(old, new)?;
        }
    }
    Ok(())
}

fn check_backward(
    old: &AvroSchema,
    new: &AvroSchema,
) -> std::result::Result<(), Incompatibility> {
    let old_fields = field_names(old);
    let new_fields = field_names(new);

    // A field in new that is absent from old must carry a default: readers
    // using `new` have to fill it in for old records.
    for (name, field) in &new_fields {
        if !old_fields.contains_key(name.as_str()) && AvroSchema::field_default(field).is_none() {
            return Err(Incompatibility::AddedFieldWithoutDefault((*name).clone()));
        }
    }

    // Shared fields: the new type must accept every old alternative.
    for (name, old_field) in &old_fields {
        if let Some(new_field) = new_fields.get(name) {
            let old_type = AvroSchema::field_type(old_field)
                .ok_or_else(|| Incompatibility::TypeChange(name.to_string(), "?".into(), "?".into()))?;
            let new_type = AvroSchema::field_type(new_field)
                .ok_or_else(|| Incompatibility::TypeChange(name.to_string(), "?".into(), "?".into()))?;
            if !accepts(new_type, old_type) {
                return Err(Incompatibility::TypeChange(
                    name.to_string(),
                    old_type.to_string(),
                    new_type.to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn check_forward(
    old: &AvroSchema,
    new: &AvroSchema,
) -> std::result::Result<(), Incompatibility> {
    let old_fields = field_names(old);
    let new_fields = field_names(new);

    // A field removed by new must have had a default in old, otherwise old
    // readers cannot materialise it from new records.
    for (name, old_field) in &old_fields {
        if !new_fields.contains_key(name.as_str()) && AvroSchema::field_default(old_field).is_none() {
            return Err(Incompatibility::RemovedFieldWithoutDefault((*name).clone()));
        }
    }

    for (name, old_field) in &old_fields {
        if let Some(new_field) = new_fields.get(name) {
            let old_type = AvroSchema::field_type(old_field)
                .ok_or_else(|| Incompatibility::TypeChange(name.to_string(), "?".into(), "?".into()))?;
            let new_type = AvroSchema::field_type(new_field)
                .ok_or_else(|| Incompatibility::TypeChange(name.to_string(), "?".into(), "?".into()))?;
            if !accepts(old_type, new_type) {
                return Err(Incompatibility::TypeChange(
                    name.to_string(),
                    old_type.to_string(),
                    new_type.to_string(),
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Store trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait SchemaRegistryStore: Send + Sync {
    /// Assign the next version, enforce compatibility against the latest
    /// version, and persist the schema.
    async fn register(&self, schema: ContractEventSchema) -> SchemaResult<()>;
    async fn latest(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Option<ContractEventSchema>>;
    async fn get_version(
        &self,
        contract_id: &str,
        event_name: &str,
        version: u32,
    ) -> SchemaResult<Option<ContractEventSchema>>;
    async fn all_versions(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>>;
    /// All schemas produced from a given contract WASM hash (usually one per
    /// event type). Used for hash-change detection and auditing.
    async fn find_by_hash(
        &self,
        contract_id: &str,
        contract_hash: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>>;
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

type SchemaKey = (String, String);

/// Fully functional registry for tests and single-process deployments.
#[derive(Debug, Default)]
pub struct InMemorySchemaRegistry {
    schemas: RwLock<HashMap<SchemaKey, Vec<ContractEventSchema>>>,
}

impl InMemorySchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Synchronous view of the latest version, used by the schema-aware event
    /// decoder (which is intentionally not async).
    pub fn latest_blocking(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> Option<ContractEventSchema> {
        let guard = self.schemas.read().expect("schema registry lock");
        guard
            .get(&(contract_id.to_string(), event_name.to_string()))
            .and_then(|versions| versions.iter().max_by_key(|schema| schema.schema_version))
            .cloned()
    }

    /// Number of registered versions, for assertions and metrics.
    pub fn len(&self) -> usize {
        self.schemas
            .read()
            .expect("schema registry lock")
            .values()
            .map(Vec::len)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl SchemaRegistryStore for InMemorySchemaRegistry {
    async fn register(&self, mut schema: ContractEventSchema) -> SchemaResult<()> {
        let key = (schema.contract_id.clone(), schema.event_name.clone());
        let next_version = {
            let guard = self.schemas.read().expect("schema registry lock");
            match guard.get(&key).and_then(|v| v.iter().max_by_key(|s| s.schema_version)) {
                Some(latest) => {
                    if schema.compatibility != Compatibility::None {
                        let old = AvroSchema::from_json(latest.schema_json.clone());
                        let new = AvroSchema::from_json(schema.schema_json.clone());
                        compat_check(&old, &new, schema.compatibility)?;
                    }
                    latest.schema_version.saturating_add(1)
                }
                None => 1,
            }
        };

        schema.schema_version = next_version;
        if schema.created_at.is_empty() {
            schema.created_at = chrono::Utc::now().to_rfc3339();
        }

        let mut guard = self.schemas.write().expect("schema registry lock");
        let versions = guard.entry(key).or_default();
        if versions.iter().any(|s| s.schema_version == next_version) {
            return Err(SchemaError::InvalidSchema(format!(
                "schema version {next_version} already exists"
            )));
        }
        let registered_contract = schema.contract_id.clone();
        let registered_name = schema.event_name.clone();
        versions.push(schema);

        metrics::counter!("rwa_schema_registry_registrations_total", "store" => "memory")
            .increment(1);
        tracing::debug!(
            contract_id = %registered_contract,
            event_name = %registered_name,
            version = next_version,
            "registered event schema"
        );
        Ok(())
    }

    async fn latest(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Option<ContractEventSchema>> {
        Ok(self.latest_blocking(contract_id, event_name))
    }

    async fn get_version(
        &self,
        contract_id: &str,
        event_name: &str,
        version: u32,
    ) -> SchemaResult<Option<ContractEventSchema>> {
        let guard = self.schemas.read().expect("schema registry lock");
        Ok(guard
            .get(&(contract_id.to_string(), event_name.to_string()))
            .and_then(|versions| versions.iter().find(|s| s.schema_version == version))
            .cloned())
    }

    async fn all_versions(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>> {
        let guard = self.schemas.read().expect("schema registry lock");
        let mut versions = guard
            .get(&(contract_id.to_string(), event_name.to_string()))
            .cloned()
            .unwrap_or_default();
        versions.sort_by_key(|schema| schema.schema_version);
        Ok(versions)
    }

    async fn find_by_hash(
        &self,
        contract_id: &str,
        contract_hash: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>> {
        let guard = self.schemas.read().expect("schema registry lock");
        let mut found: Vec<ContractEventSchema> = guard
            .values()
            .flatten()
            .filter(|schema| {
                schema.contract_id == contract_id && schema.contract_hash == contract_hash
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            a.event_name
                .cmp(&b.event_name)
                .then(a.schema_version.cmp(&b.schema_version))
        });
        Ok(found)
    }
}

// ---------------------------------------------------------------------------
// Postgres store
// ---------------------------------------------------------------------------

/// Fields selected from `contract_event_schema`.
///
/// `FromRow` is implemented by hand: this crate builds sqlx without the
/// `macros`/`derive` features, so `#[derive(sqlx::FromRow)]` is unavailable.
#[derive(Debug)]
struct SchemaRow {
    contract_id: String,
    event_name: String,
    schema_version: i64,
    schema_json: String,
    compatibility: String,
    contract_hash: String,
    created_at: String,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for SchemaRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            contract_id: row.try_get("contract_id")?,
            event_name: row.try_get("event_name")?,
            schema_version: row.try_get("schema_version")?,
            schema_json: row.try_get("schema_json")?,
            compatibility: row.try_get("compatibility")?,
            contract_hash: row.try_get("contract_hash")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

impl TryFrom<SchemaRow> for ContractEventSchema {
    type Error = SchemaError;

    fn try_from(row: SchemaRow) -> SchemaResult<Self> {
        let schema_json: Value = serde_json::from_str(&row.schema_json)
            .map_err(|e| SchemaError::InvalidSchema(e.to_string()))?;
        Ok(Self {
            contract_id: row.contract_id,
            event_name: row.event_name,
            schema_version: u32::try_from(row.schema_version).unwrap_or(u32::MAX),
            schema_json,
            compatibility: row.compatibility.parse()?,
            contract_hash: row.contract_hash,
            created_at: row.created_at,
        })
    }
}

const SELECT_COLUMNS: &str = "contract_id, event_name, schema_version, schema_json, \
     compatibility, contract_hash, \
     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at";

const INSERT_SCHEMA: &str = "INSERT INTO contract_event_schema \
     (contract_id, event_name, schema_version, schema_json, compatibility, contract_hash, created_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7::timestamptz) \
     ON CONFLICT (contract_id, event_name, schema_version) DO NOTHING";

/// Persistent registry using the table from migration `0005`.
#[derive(Debug, Clone)]
pub struct PostgresSchemaRegistry {
    pool: sqlx::PgPool,
}

impl PostgresSchemaRegistry {
    /// Open the registry, creating the table if it does not exist yet.
    pub async fn open(pool: sqlx::PgPool) -> SchemaResult<Self> {
        sqlx::raw_sql(MIGRATION_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    async fn select_one(&self, sql: &str, id: &str, name: &str) -> SchemaResult<Option<ContractEventSchema>> {
        let row: Option<SchemaRow> = sqlx::query_as(sql)
            .bind(id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(ContractEventSchema::try_from).transpose()
    }
}

#[async_trait]
impl SchemaRegistryStore for PostgresSchemaRegistry {
    async fn register(&self, mut schema: ContractEventSchema) -> SchemaResult<()> {
        let latest = self
            .select_one(
                &format!(
                    "SELECT {SELECT_COLUMNS} FROM contract_event_schema \
                     WHERE contract_id = $1 AND event_name = $2 \
                     ORDER BY schema_version DESC LIMIT 1"
                ),
                &schema.contract_id,
                &schema.event_name,
            )
            .await?;

        let next_version = match latest {
            Some(existing) => {
                if schema.compatibility != Compatibility::None {
                    let old = AvroSchema::from_json(existing.schema_json.clone());
                    let new = AvroSchema::from_json(schema.schema_json.clone());
                    compat_check(&old, &new, schema.compatibility)?;
                }
                existing.schema_version.saturating_add(1)
            }
            None => 1,
        };

        schema.schema_version = next_version;
        if schema.created_at.is_empty() {
            schema.created_at = chrono::Utc::now().to_rfc3339();
        }

        let json_text = schema.schema_json.to_string();
        sqlx::query(INSERT_SCHEMA)
            .bind(&schema.contract_id)
            .bind(&schema.event_name)
            .bind(i64::from(next_version))
            .bind(json_text)
            .bind(schema.compatibility.as_str())
            .bind(&schema.contract_hash)
            .bind(&schema.created_at)
            .execute(&self.pool)
            .await?;

        metrics::counter!("rwa_schema_registry_registrations_total", "store" => "postgres")
            .increment(1);
        Ok(())
    }

    async fn latest(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Option<ContractEventSchema>> {
        self.select_one(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM contract_event_schema \
                 WHERE contract_id = $1 AND event_name = $2 \
                 ORDER BY schema_version DESC LIMIT 1"
            ),
            contract_id,
            event_name,
        )
        .await
    }

    async fn get_version(
        &self,
        contract_id: &str,
        event_name: &str,
        version: u32,
    ) -> SchemaResult<Option<ContractEventSchema>> {
        let row: Option<SchemaRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLUMNS} FROM contract_event_schema \
             WHERE contract_id = $1 AND event_name = $2 AND schema_version = $3"
        ))
        .bind(contract_id)
        .bind(event_name)
        .bind(i64::from(version))
        .fetch_optional(&self.pool)
        .await?;
        row.map(ContractEventSchema::try_from).transpose()
    }

    async fn all_versions(
        &self,
        contract_id: &str,
        event_name: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>> {
        let rows: Vec<SchemaRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLUMNS} FROM contract_event_schema \
             WHERE contract_id = $1 AND event_name = $2 ORDER BY schema_version ASC"
        ))
        .bind(contract_id)
        .bind(event_name)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ContractEventSchema::try_from).collect()
    }

    async fn find_by_hash(
        &self,
        contract_id: &str,
        contract_hash: &str,
    ) -> SchemaResult<Vec<ContractEventSchema>> {
        let rows: Vec<SchemaRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLUMNS} FROM contract_event_schema \
             WHERE contract_id = $1 AND contract_hash = $2 \
             ORDER BY event_name ASC, schema_version ASC"
        ))
        .bind(contract_id)
        .bind(contract_hash)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ContractEventSchema::try_from).collect()
    }
}

// ---------------------------------------------------------------------------
// ABI discovery
// ---------------------------------------------------------------------------

/// One event (or function) entry extracted from a contract's spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSpec {
    pub name: String,
    /// `(field name, Soroban type name)` pairs.
    pub fields: Vec<(String, String)>,
}

/// Normalised contract ABI. Only event-like entries matter to this feature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AbiSpec {
    pub events: Vec<EventSpec>,
}

#[async_trait]
pub trait AbiInspector: Send + Sync {
    async fn inspect(&self, contract_id: &str) -> SchemaResult<AbiSpec>;
}

fn field_pair(entry: &Value) -> Option<(String, String)> {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("field")
        .to_string();
    let raw_type = entry
        .get("type")
        .or_else(|| entry.get("type_name"))
        .or_else(|| entry.get("typeName"))
        .or_else(|| entry.get("type_"));
    let ty = raw_type.map(spec_type_string).unwrap_or_else(|| "String".to_string());
    Some((name, ty))
}

/// Turn a spec type (string or single-key object such as `{"Vec":"U32"}`)
/// into the angle-bracket form understood by [`spec_type_to_avro`].
fn spec_type_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(obj) if obj.len() == 1 => {
            let (key, inner) = obj.iter().next().expect("single-key object");
            match inner {
                Value::String(name) => format!("{key}<{name}>"),
                Value::Null => key.clone(),
                other => format!("{key}<{}>", other),
            }
        }
        other => other.to_string(),
    }
}

/// Parse the JSON printed by `soroban contract inspect --output-format json`.
/// Accepts `events` and falls back to `functions` for older CLI versions.
pub fn parse_abi_json(value: &Value) -> SchemaResult<AbiSpec> {
    const EMPTY: &[Value] = &[];
    let entries: &[Value] = value
        .get("events")
        .and_then(Value::as_array)
        .filter(|events| !events.is_empty())
        .or_else(|| value.get("functions").and_then(Value::as_array))
        .map_or(EMPTY, Vec::as_slice);

    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let params: &[Value] = entry
            .get("params")
            .and_then(Value::as_array)
            .or_else(|| entry.get("inputs").and_then(Value::as_array))
            .or_else(|| entry.get("fields").and_then(Value::as_array))
            .map_or(EMPTY, Vec::as_slice);
        let fields = params.iter().filter_map(field_pair).collect();
        events.push(EventSpec {
            name: name.to_string(),
            fields,
        });
    }
    Ok(AbiSpec { events })
}

/// Inspector that shells out to the Soroban CLI.
#[derive(Debug, Clone)]
pub struct CliAbiInspector {
    program: String,
}

impl Default for CliAbiInspector {
    fn default() -> Self {
        Self {
            program: "soroban".to_string(),
        }
    }
}

impl CliAbiInspector {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

#[async_trait]
impl AbiInspector for CliAbiInspector {
    async fn inspect(&self, contract_id: &str) -> SchemaResult<AbiSpec> {
        let output = tokio::process::Command::new(&self.program)
            .args(["contract", "inspect", "--id", contract_id, "--output-format", "json"])
            .output()
            .await
            .map_err(|e| SchemaError::Abi(format!("could not run {}: {e}", self.program)))?;
        if !output.status.success() {
            return Err(SchemaError::Abi(format!(
                "soroban contract inspect failed for {contract_id}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let json: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| SchemaError::Abi(format!("invalid inspect JSON: {e}")))?;
        parse_abi_json(&json)
    }
}

/// Deterministic inspector for tests and offline fallback.
#[derive(Debug, Clone, Default)]
pub struct StaticAbiInspector {
    spec: AbiSpec,
}

impl StaticAbiInspector {
    pub fn new(spec: AbiSpec) -> Self {
        Self { spec }
    }

    pub fn from_events(events: Vec<EventSpec>) -> Self {
        Self {
            spec: AbiSpec { events },
        }
    }
}

#[async_trait]
impl AbiInspector for StaticAbiInspector {
    async fn inspect(&self, _contract_id: &str) -> SchemaResult<AbiSpec> {
        Ok(self.spec.clone())
    }
}

/// Map a Soroban spec type name to an Avro type. `Option<T>` becomes a
/// `["null", T]` union; containers recurse into their element type.
pub fn spec_type_to_avro(type_name: &str) -> Value {
    let name = type_name.trim();
    if let Some(inner) = generic_inner(name, "Option") {
        return json!(["null", spec_type_to_avro(inner)]);
    }
    if let Some(inner) = generic_inner(name, "Vec") {
        return json!({ "type": "array", "items": spec_type_to_avro(inner) });
    }
    if let Some(inner) = generic_inner(name, "Map") {
        let (key, value) = split_pair(inner).unwrap_or(("String", "String"));
        // Avro map keys are always strings, but we still map the value type.
        let _ = key;
        return json!({ "type": "map", "values": spec_type_to_avro(value) });
    }

    match name.to_ascii_lowercase().as_str() {
        "u32" | "i32" | "u64" | "i64" | "timepoint" | "duration" | "usize" => json!("long"),
        "u128" | "i128" | "u256" | "i256" => json!("string"),
        "bool" | "boolean" => json!("boolean"),
        "symbol" | "string" | "address" => json!("string"),
        "bytes" | "bytesn" => json!("bytes"),
        "void" | "unit" | "null" => json!("null"),
        _ => json!("string"),
    }
}

fn generic_inner<'a>(name: &'a str, wrapper: &str) -> Option<&'a str> {
    if name.len() <= wrapper.len() + 2 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let needle = wrapper.to_ascii_lowercase();
    if lower.starts_with(&needle) && name.as_bytes()[wrapper.len()] == b'<' && name.ends_with('>') {
        return Some(&name[wrapper.len() + 1..name.len() - 1]);
    }
    None
}

fn split_pair(inner: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (index, ch) in inner.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                return Some((inner[..index].trim(), inner[index + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

/// Avro record for one ABI event entry.
pub fn event_schema_from_spec(
    contract_id: &str,
    contract_hash: &str,
    spec: &EventSpec,
    compatibility: Compatibility,
) -> ContractEventSchema {
    let fields: Vec<Value> = spec
        .fields
        .iter()
        .map(|(name, ty)| json!({ "name": name, "type": spec_type_to_avro(ty) }))
        .collect();
    ContractEventSchema::new(
        contract_id,
        &spec.name,
        AvroSchema::new(&spec.name, fields).raw,
        compatibility,
        contract_hash,
    )
}

/// Typical RWA token events used when the ABI exposes no event entries.
pub fn default_event_specs() -> Vec<EventSpec> {
    vec![
        EventSpec {
            name: "Transferred".to_string(),
            fields: vec![
                ("from".to_string(), "Address".to_string()),
                ("to".to_string(), "Address".to_string()),
                ("amount".to_string(), "I128".to_string()),
            ],
        },
        EventSpec {
            name: "Minted".to_string(),
            fields: vec![
                ("to".to_string(), "Address".to_string()),
                ("amount".to_string(), "I128".to_string()),
            ],
        },
        EventSpec {
            name: "Burned".to_string(),
            fields: vec![
                ("from".to_string(), "Address".to_string()),
                ("amount".to_string(), "I128".to_string()),
            ],
        },
    ]
}

/// Register new schema versions when `contract_hash` has not been seen for
/// this contract before. Returns the last registered schema, or `None` when
/// the hash is unchanged (or the ABI exposes no events).
pub async fn sync_contract_schema<S, I>(
    store: &S,
    inspector: &I,
    contract_id: &str,
    contract_hash: &str,
    compatibility: Compatibility,
) -> SchemaResult<Option<ContractEventSchema>>
where
    S: SchemaRegistryStore,
    I: AbiInspector,
{
    // An unchanged WASM hash means the event shapes cannot have changed.
    if !store.find_by_hash(contract_id, contract_hash).await?.is_empty() {
        tracing::debug!(contract_id, contract_hash, "event schemas already current");
        return Ok(None);
    }

    let spec = inspector.inspect(contract_id).await?;
    let events = if spec.events.is_empty() {
        default_event_specs()
    } else {
        spec.events
    };

    let mut last = None;
    for event in &events {
        let schema = event_schema_from_spec(contract_id, contract_hash, event, compatibility);
        store.register(schema.clone()).await?;
        metrics::counter!("rwa_schema_registry_sync_total", "event" => schema.event_name.clone())
            .increment(1);
        last = Some(schema);
    }
    Ok(last)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, ty: Value) -> Value {
        json!({ "name": name, "type": ty })
    }

    fn field_with_default(name: &str, ty: Value, default: Value) -> Value {
        json!({ "name": name, "type": ty, "default": default })
    }

    fn v1() -> AvroSchema {
        AvroSchema::new(
            "Transferred",
            vec![
                field("from", json!("string")),
                field("to", json!("string")),
                field("amount", json!("long")),
            ],
        )
    }

    #[test]
    fn nullable_added_field_passes_backward_forward_and_full() {
        let old = v1();
        let mut fields = old.fields().to_vec();
        fields.push(field_with_default("memo", json!(["null", "string"]), Value::Null));
        let new = AvroSchema::new("Transferred", fields);

        assert!(AvroSchema::is_nullable(new.field_type_by_name("memo").unwrap()));
        assert_eq!(compat_check(&old, &new, Compatibility::Backward), Ok(()));
        assert_eq!(compat_check(&old, &new, Compatibility::Forward), Ok(()));
        assert_eq!(compat_check(&old, &new, Compatibility::Full), Ok(()));
    }

    #[test]
    fn added_field_without_default_fails_backward() {
        let old = v1();
        let mut fields = old.fields().to_vec();
        fields.push(field("memo", json!("string")));
        let new = AvroSchema::new("Transferred", fields);

        assert_eq!(
            compat_check(&old, &new, Compatibility::Backward),
            Err(Incompatibility::AddedFieldWithoutDefault("memo".to_string()))
        );
        // Forward only: old readers ignore the extra field.
        assert_eq!(compat_check(&old, &new, Compatibility::Forward), Ok(()));
    }

    #[test]
    fn removed_field_without_default_fails_forward() {
        let old = v1();
        let new = AvroSchema::new(
            "Transferred",
            vec![field("from", json!("string")), field("to", json!("string"))],
        );
        // New readers simply skip the old field, so BACKWARD is fine.
        assert_eq!(compat_check(&old, &new, Compatibility::Backward), Ok(()));
        // Old readers need a default for the removed field to read new data.
        assert_eq!(
            compat_check(&old, &new, Compatibility::Forward),
            Err(Incompatibility::RemovedFieldWithoutDefault("amount".to_string()))
        );
    }

    #[test]
    fn type_change_requires_widening_union() {
        let old = AvroSchema::new("E", vec![field("value", json!("long"))]);
        let bad = AvroSchema::new("E", vec![field("value", json!("string"))]);
        assert!(compat_check(&old, &bad, Compatibility::Full).is_err());

        let widened = AvroSchema::new("E", vec![field("value", json!(["long", "string"]))]);
        assert_eq!(compat_check(&old, &widened, Compatibility::Backward), Ok(()));
        // Old reader cannot read a string written under the widened schema.
        assert!(compat_check(&old, &widened, Compatibility::Forward).is_err());
    }

    #[test]
    fn compatibility_parses_all_modes() {
        assert_eq!("backward".parse::<Compatibility>().unwrap(), Compatibility::Backward);
        assert_eq!("FULL".parse::<Compatibility>().unwrap(), Compatibility::Full);
        assert_eq!("none".parse::<Compatibility>().unwrap(), Compatibility::None);
        assert!("sideways".parse::<Compatibility>().is_err());
    }

    #[tokio::test]
    async fn in_memory_register_assigns_versions_and_rejects_incompatible() {
        let store = InMemorySchemaRegistry::new();
        let schema = ContractEventSchema::new(
            "C1",
            "Transferred",
            v1().raw,
            Compatibility::Backward,
            "hash-a",
        );
        store.register(schema.clone()).await.unwrap();

        assert_eq!(store.latest("C1", "Transferred").await.unwrap().unwrap().schema_version, 1);
        assert_eq!(store.get_version("C1", "Transferred", 1).await.unwrap().unwrap().schema_version, 1);
        assert!(store.get_version("C1", "Transferred", 2).await.unwrap().is_none());

        // Incompatible: type change with no widening union.
        let bad = ContractEventSchema::new(
            "C1",
            "Transferred",
            AvroSchema::new("Transferred", vec![field("amount", json!("string"))]).raw,
            Compatibility::Backward,
            "hash-b",
        );
        assert!(store.register(bad).await.is_err());
        assert_eq!(store.all_versions("C1", "Transferred").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn in_memory_register_accepts_compatible_next_version() {
        let store = InMemorySchemaRegistry::new();
        store
            .register(ContractEventSchema::new("C1", "E", v1().raw, Compatibility::Full, "h1"))
            .await
            .unwrap();

        let mut fields = v1().fields().to_vec();
        fields.push(field_with_default("memo", json!(["null", "string"]), Value::Null));
        store
            .register(ContractEventSchema::new(
                "C1",
                "E",
                AvroSchema::new("E", fields).raw,
                Compatibility::Full,
                "h2",
            ))
            .await
            .unwrap();

        let versions = store.all_versions("C1", "E").await.unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].schema_version, 2);
        assert_eq!(store.latest_blocking("C1", "E").unwrap().schema_version, 2);
    }

    #[tokio::test]
    async fn hash_change_triggers_exactly_one_registration_and_same_hash_is_noop() {
        let store = InMemorySchemaRegistry::new();
        let inspector = StaticAbiInspector::from_events(vec![EventSpec {
            name: "Transferred".to_string(),
            fields: vec![
                ("from".to_string(), "Address".to_string()),
                ("to".to_string(), "Address".to_string()),
                ("amount".to_string(), "I128".to_string()),
            ],
        }]);

        let first = sync_contract_schema(
            &store,
            &inspector,
            "C1",
            "hash-v1",
            Compatibility::Backward,
        )
        .await
        .unwrap();
        assert!(first.is_some());
        assert_eq!(store.len(), 1);

        let again = sync_contract_schema(
            &store,
            &inspector,
            "C1",
            "hash-v1",
            Compatibility::Backward,
        )
        .await
        .unwrap();
        assert!(again.is_none());
        assert_eq!(store.len(), 1, "unchanged hash must not register anything");

        let upgraded = sync_contract_schema(
            &store,
            &inspector,
            "C1",
            "hash-v2",
            Compatibility::Backward,
        )
        .await
        .unwrap();
        assert!(upgraded.is_some());
        assert_eq!(store.len(), 2, "a new hash registers exactly one schema");
        assert_eq!(store.latest("C1", "Transferred").await.unwrap().unwrap().schema_version, 2);
        assert_eq!(store.find_by_hash("C1", "hash-v2").await.unwrap().len(), 1);
    }

    #[test]
    fn abi_types_map_to_avro_primitives_and_unions() {
        assert_eq!(spec_type_to_avro("U32"), json!("long"));
        assert_eq!(spec_type_to_avro("I64"), json!("long"));
        assert_eq!(spec_type_to_avro("U128"), json!("string"));
        assert_eq!(spec_type_to_avro("Bool"), json!("boolean"));
        assert_eq!(spec_type_to_avro("Symbol"), json!("string"));
        assert_eq!(spec_type_to_avro("Bytes"), json!("bytes"));
        assert_eq!(spec_type_to_avro("Address"), json!("string"));
        assert_eq!(spec_type_to_avro("Option<String>"), json!(["null", "string"]));
        assert_eq!(
            spec_type_to_avro("Vec<U32>"),
            json!({ "type": "array", "items": "long" })
        );
        assert_eq!(
            spec_type_to_avro("Map<String, I128>"),
            json!({ "type": "map", "values": "string" })
        );
    }

    #[test]
    fn abi_json_is_parsed_into_event_specs() {
        let spec = parse_abi_json(&json!({
            "functions": [{"name": "version"}],
            "events": [{
                "name": "Minted",
                "params": [
                    {"name": "to", "type": "Address"},
                    {"name": "memo", "type": {"Option": "String"}}
                ]
            }]
        }))
        .unwrap();
        assert_eq!(spec.events.len(), 1);
        assert_eq!(spec.events[0].name, "Minted");
        assert_eq!(spec.events[0].fields[1].1, "Option<String>");

        let schema = event_schema_from_spec("C1", "h", &spec.events[0], Compatibility::Backward);
        let avro = AvroSchema::from_json(schema.schema_json);
        assert_eq!(avro.field_type_by_name("to"), Some(&json!("string")));
        assert_eq!(avro.field_type_by_name("memo"), Some(&json!(["null", "string"])));
    }
}
