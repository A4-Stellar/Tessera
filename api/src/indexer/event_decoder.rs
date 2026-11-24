//! Schema-aware decoding of Soroban contract events (event schema registry).
//!
//! The zero-copy [`xdr_parser`] gives us borrowed `ScValView`s. This module
//! overlays an Avro record on top of the event's data `ScVal` map and produces
//! a typed `serde_json::Value`:
//!
//! * every Avro field name is looked up in the `ScVal` map (extra keys are
//!   ignored, so new events remain readable with old schemas);
//! * numeric `ScVal`s become JSON numbers, widening to decimal strings above
//!   `i64::MAX` so JavaScript consumers keep full precision;
//! * addresses become strkeys (hex for exotic variants), bytes become hex
//!   strings, and `["null", T]` unions accept `Void` as `null`;
//! * nested records, arrays and maps recurse, and missing nullable fields
//!   resolve to their default (usually `null`).
//!
//! Decoding is intentionally synchronous and generic over [`SchemaLookup`] so
//! the in-memory registry can be used directly in tests without a database.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::schema_registry::{AvroSchema, InMemorySchemaRegistry};
use super::xdr_parser::{parse_contract_event, ScValView, XdrError};

/// Errors produced while decoding a contract event with a schema.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("XDR parse error: {0}")]
    Xdr(#[from] XdrError),
    #[error("no event schema for contract '{contract_id}' event '{event_name}'")]
    SchemaNotFound { contract_id: String, event_name: String },
    #[error("invalid Avro schema: {0}")]
    InvalidSchema(String),
    #[error("event data must be an ScVal map, found {0}")]
    DataNotAMap(&'static str),
    #[error("missing required field '{0}'")]
    MissingField(String),
    #[error("type mismatch for field '{field}': expected {expected}, got {actual}")]
    TypeMismatch { field: String, expected: String, actual: String },
    #[error("invalid UTF-8 in field '{0}'")]
    InvalidUtf8(String),
}

/// Supplies the current Avro schema for an event type.
pub trait SchemaLookup: Send + Sync {
    fn schema_for(&self, contract_id: &str, event_name: &str) -> Option<AvroSchema>;
}

impl SchemaLookup for InMemorySchemaRegistry {
    fn schema_for(&self, contract_id: &str, event_name: &str) -> Option<AvroSchema> {
        self.latest_blocking(contract_id, event_name)
            .map(|schema| AvroSchema::from_json(schema.schema_json))
    }
}

/// Small standalone lookup for callers that do not use a registry.
#[derive(Debug, Default)]
pub struct StaticSchemaLookup {
    schemas: HashMap<(String, String), AvroSchema>,
}

impl StaticSchemaLookup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        contract_id: impl Into<String>,
        event_name: impl Into<String>,
        schema: AvroSchema,
    ) {
        self.schemas.insert((contract_id.into(), event_name.into()), schema);
    }
}

impl SchemaLookup for StaticSchemaLookup {
    fn schema_for(&self, contract_id: &str, event_name: &str) -> Option<AvroSchema> {
        self.schemas
            .get(&(contract_id.to_string(), event_name.to_string()))
            .cloned()
    }
}

/// Decode a whole `ContractEvent` XDR buffer using the schema registered for
/// `(contract_id, event_name)`.
pub fn decode_event(
    lookup: &impl SchemaLookup,
    contract_id: &str,
    event_name: &str,
    xdr_bytes: &[u8],
) -> Result<Value, DecodeError> {
    let schema = lookup
        .schema_for(contract_id, event_name)
        .ok_or_else(|| DecodeError::SchemaNotFound {
            contract_id: contract_id.to_string(),
            event_name: event_name.to_string(),
        })?;

    let event = parse_contract_event(xdr_bytes)?;
    let entries = match event.data {
        ScValView::Map(Some(map)) => collect_entries(map)?,
        ScValView::Map(None) => HashMap::new(),
        ScValView::Bool(_) => return Err(DecodeError::DataNotAMap("bool")),
        ScValView::Void => return Err(DecodeError::DataNotAMap("void")),
        ScValView::Error(..) => return Err(DecodeError::DataNotAMap("error")),
        ScValView::U32(_) => return Err(DecodeError::DataNotAMap("u32")),
        ScValView::I32(_) => return Err(DecodeError::DataNotAMap("i32")),
        ScValView::U64(_) => return Err(DecodeError::DataNotAMap("u64")),
        ScValView::I64(_) => return Err(DecodeError::DataNotAMap("i64")),
        ScValView::Timepoint(_) => return Err(DecodeError::DataNotAMap("timepoint")),
        ScValView::Duration(_) => return Err(DecodeError::DataNotAMap("duration")),
        ScValView::U128(_) => return Err(DecodeError::DataNotAMap("u128")),
        ScValView::I128(_) => return Err(DecodeError::DataNotAMap("i128")),
        ScValView::U256(_) => return Err(DecodeError::DataNotAMap("u256")),
        ScValView::I256(_) => return Err(DecodeError::DataNotAMap("i256")),
        ScValView::Bytes(_) => return Err(DecodeError::DataNotAMap("bytes")),
        ScValView::String(_) => return Err(DecodeError::DataNotAMap("string")),
        ScValView::Symbol(_) => return Err(DecodeError::DataNotAMap("symbol")),
        ScValView::Vec(_) => return Err(DecodeError::DataNotAMap("vec")),
        ScValView::Address(_) => return Err(DecodeError::DataNotAMap("address")),
        ScValView::LedgerKeyNonce(_) => return Err(DecodeError::DataNotAMap("ledger_key_nonce")),
    };

    let decoder = Decoder { root: &schema };
    decoder.record_value(&schema.raw, &entries)
}

fn collect_entries<'a>(map: super::xdr_parser::MapView<'a>) -> Result<HashMap<String, ScValView<'a>>, DecodeError> {
    let mut entries = HashMap::with_capacity(map.len());
    for pair in map.iter() {
        let (key, value) = pair?;
        let name = match key {
            ScValView::Symbol(bytes) | ScValView::String(bytes) => {
                std::str::from_utf8(bytes)
                    .map(str::to_string)
                    .map_err(|_| DecodeError::InvalidUtf8("map key".to_string()))?
            }
            ScValView::Bytes(bytes) => hex::encode(bytes),
            ScValView::U256(bytes) | ScValView::I256(bytes) => hex::encode(bytes),
            other => format!("{other:?}"),
        };
        entries.insert(name, value);
    }
    Ok(entries)
}

struct Decoder<'s> {
    root: &'s AvroSchema,
}

impl<'s> Decoder<'s> {
    /// Materialise an Avro record (an inline `type: record` value) from the
    /// event's data map. Missing fields use their declared default, and
    /// nullable fields without an explicit default resolve to `null`.
    fn record_value(
        &self,
        record_ty: &Value,
        entries: &HashMap<String, ScValView<'_>>,
    ) -> Result<Value, DecodeError> {
        let fields = record_ty
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| DecodeError::InvalidSchema("record without fields".to_string()))?;
        let mut object = Map::new();
        for field in fields {
            let name = field
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| DecodeError::InvalidSchema("field without a name".to_string()))?;
            let ty = AvroSchema::field_type(field)
                .ok_or_else(|| DecodeError::InvalidSchema(format!("field '{name}' has no type")))?;

            let value = match entries.get(name) {
                Some(scval) => self.value(ty, *scval, name)?,
                None => match AvroSchema::field_default(field) {
                    Some(default) => default.clone(),
                    None if AvroSchema::is_nullable(ty) => Value::Null,
                    None => return Err(DecodeError::MissingField(name.to_string())),
                },
            };
            object.insert(name.to_string(), value);
        }
        Ok(Value::Object(object))
    }

    fn value(
        &self,
        ty: &Value,
        scval: ScValView<'_>,
        field: &str,
    ) -> Result<Value, DecodeError> {
        // Unions: `Void` maps to the `null` branch, otherwise pick the first
        // branch whose shape matches the observed ScVal.
        if let Value::Array(branches) = ty {
            if matches!(scval, ScValView::Void) {
                if branches.iter().any(|branch| branch.as_str() == Some("null")) {
                    return Ok(Value::Null);
                }
                return Err(self.mismatch(field, ty, scval));
            }
            let branch = branches
                .iter()
                .find(|branch| self.matches(branch, &scval))
                .ok_or_else(|| self.mismatch(field, ty, scval))?;
            return self.value(branch, scval, field);
        }

        // Named type reference: resolve against the root schema.
        if let Value::String(name) = ty {
            if !is_primitive(name) {
                if let Some(named) = self.root.resolve_named(name) {
                    return self.value(named, scval, field);
                }
            }
        }

        let type_name = AvroSchema::type_name(ty).unwrap_or("unknown");
        match type_name {
            "null" => match scval {
                ScValView::Void => Ok(Value::Null),
                _ => Err(self.mismatch(field, ty, scval)),
            },
            "boolean" => match scval {
                ScValView::Bool(b) => Ok(Value::Bool(b)),
                _ => Err(self.mismatch(field, ty, scval)),
            },
            "long" | "int" | "float" | "double" => self.number(scval, field, ty),
            "string" => self.string(scval, field),
            "bytes" | "fixed" => match scval {
                ScValView::Bytes(bytes) => Ok(Value::String(hex::encode(bytes))),
                ScValView::U256(bytes) | ScValView::I256(bytes) => {
                    Ok(Value::String(hex::encode(bytes)))
                }
                _ => Err(self.mismatch(field, ty, scval)),
            },
            "array" => {
                let items = ty
                    .get("items")
                    .ok_or_else(|| DecodeError::InvalidSchema("array without items".to_string()))?;
                match scval {
                    ScValView::Vec(None) => Ok(Value::Array(Vec::new())),
                    ScValView::Vec(Some(seq)) => {
                        let mut out = Vec::with_capacity(seq.len());
                        for item in seq.iter() {
                            out.push(self.value(items, item?, field)?);
                        }
                        Ok(Value::Array(out))
                    }
                    _ => Err(self.mismatch(field, ty, scval)),
                }
            }
            "map" => {
                let values = ty.get("values").ok_or_else(|| {
                    DecodeError::InvalidSchema("map without values".to_string())
                })?;
                let map = match scval {
                    ScValView::Map(None) => return Ok(Value::Object(Map::new())),
                    ScValView::Map(Some(map)) => map,
                    _ => return Err(self.mismatch(field, ty, scval)),
                };
                let mut out = Map::new();
                for pair in map.iter() {
                    let (key, val) = pair?;
                    let key = self.map_key(key)?;
                    out.insert(key, self.value(values, val, field)?);
                }
                Ok(Value::Object(out))
            }
            "record" => {
                let map = match scval {
                    ScValView::Map(None) => return self.record_value(ty, &HashMap::new()),
                    ScValView::Map(Some(map)) => map,
                    _ => return Err(self.mismatch(field, ty, scval)),
                };
                self.record_value(ty, &collect_entries(map)?)
            }
            other => Err(DecodeError::InvalidSchema(format!(
                "unsupported Avro type '{other}' for field '{field}'"
            ))),
        }
    }

    fn number(
        &self,
        scval: ScValView<'_>,
        field: &str,
        ty: &Value,
    ) -> Result<Value, DecodeError> {
        let value: i128 = match scval {
            ScValView::U32(n) => n as i128,
            ScValView::I32(n) => n as i128,
            ScValView::U64(n) => n as i128,
            ScValView::I64(n) => n as i128,
            ScValView::Timepoint(n) | ScValView::Duration(n) => n as i128,
            ScValView::U128(n) => n as i128,
            ScValView::I128(n) => n,
            _ => return Err(self.mismatch(field, ty, scval)),
        };
        Ok(match i64::try_from(value) {
            Ok(small) => Value::from(small),
            Err(_) => Value::String(value.to_string()),
        })
    }

    fn string(&self, scval: ScValView<'_>, field: &str) -> Result<Value, DecodeError> {
        let text = match scval {
            ScValView::String(bytes) | ScValView::Symbol(bytes) => std::str::from_utf8(bytes)
                .map(str::to_string)
                .map_err(|_| DecodeError::InvalidUtf8(field.to_string()))?,
            ScValView::Address(address) => match address {
                super::xdr_parser::AddressView::Account(bytes) => {
                    stellar_strkey::ed25519::PublicKey(*bytes).to_string()
                }
                super::xdr_parser::AddressView::Contract(bytes) => {
                    stellar_strkey::Contract(*bytes).to_string()
                }
                super::xdr_parser::AddressView::Other(_, bytes) => hex::encode(bytes),
            },
            ScValView::Bytes(bytes) => hex::encode(bytes),
            _ => return Err(DecodeError::InvalidSchema(format!(
                "cannot read field '{field}' as a string"
            ))),
        };
        Ok(Value::String(text))
    }

    fn map_key(&self, key: ScValView<'_>) -> Result<String, DecodeError> {
        Ok(match key {
            ScValView::Symbol(bytes) | ScValView::String(bytes) => std::str::from_utf8(bytes)
                .map(str::to_string)
                .map_err(|_| DecodeError::InvalidUtf8("map key".to_string()))?,
            ScValView::Bytes(bytes) => hex::encode(bytes),
            ScValView::U32(n) => n.to_string(),
            ScValView::I32(n) => n.to_string(),
            ScValView::U64(n) => n.to_string(),
            ScValView::I64(n) => n.to_string(),
            other => format!("{other:?}"),
        })
    }

    fn matches(&self, ty: &Value, scval: &ScValView<'_>) -> bool {
        match ty {
            Value::Array(branches) => branches.iter().any(|branch| self.matches(branch, scval)),
            Value::String(name) => {
                if is_primitive(name) {
                    primitive_matches(name, scval)
                } else {
                    self.root
                        .resolve_named(name)
                        .is_some_and(|named| self.matches(named, scval))
                }
            }
            Value::Object(_) => match AvroSchema::type_name(ty) {
                Some("record") => matches!(scval, ScValView::Map(_)),
                Some("array") => matches!(scval, ScValView::Vec(_)),
                Some("map") => matches!(scval, ScValView::Map(_)),
                Some(primitive) => primitive_matches(primitive, scval),
                None => false,
            },
            _ => false,
        }
    }

    fn mismatch(&self, field: &str, ty: &Value, scval: ScValView<'_>) -> DecodeError {
        DecodeError::TypeMismatch {
            field: field.to_string(),
            expected: ty.to_string(),
            actual: format!("{scval:?}"),
        }
    }
}

fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "null" | "boolean" | "int" | "long" | "float" | "double" | "string" | "bytes"
    )
}

fn primitive_matches(name: &str, scval: &ScValView<'_>) -> bool {
    match name {
        "null" => matches!(scval, ScValView::Void),
        "boolean" => matches!(scval, ScValView::Bool(_)),
        "int" | "long" | "float" | "double" => matches!(
            scval,
            ScValView::U32(_)
                | ScValView::I32(_)
                | ScValView::U64(_)
                | ScValView::I64(_)
                | ScValView::Timepoint(_)
                | ScValView::Duration(_)
                | ScValView::U128(_)
                | ScValView::I128(_)
        ),
        "string" => matches!(
            scval,
            ScValView::String(_) | ScValView::Symbol(_) | ScValView::Address(_)
        ),
        "bytes" | "fixed" => matches!(
            scval,
            ScValView::Bytes(_) | ScValView::U256(_) | ScValView::I256(_)
        ),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::schema_registry::{
        Compatibility, ContractEventSchema, SchemaRegistryStore,
    };
    use serde_json::json;
    use stellar_xdr::curr as xdr;
    use stellar_xdr::curr::{Limits, WriteXdr};

    fn sym(s: &str) -> xdr::ScVal {
        xdr::ScVal::Symbol(xdr::ScSymbol(s.try_into().unwrap()))
    }

    fn string(s: &str) -> xdr::ScVal {
        xdr::ScVal::String(xdr::ScString(s.try_into().unwrap()))
    }

    /// Build a complete `ContractEvent` whose data is the supplied map.
    fn event_bytes(entries: Vec<(&str, xdr::ScVal)>) -> Vec<u8> {
        let data = xdr::ScVal::Map(Some(xdr::ScMap(
            entries
                .into_iter()
                .map(|(key, val)| xdr::ScMapEntry {
                    key: sym(key),
                    val,
                })
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        )));
        xdr::ContractEvent {
            ext: xdr::ExtensionPoint::V0,
            contract_id: Some(xdr::ContractId(xdr::Hash([3; 32]))),
            type_: xdr::ContractEventType::Contract,
            body: xdr::ContractEventBody::V0(xdr::ContractEventV0 {
                topics: vec![sym("Transferred")].try_into().unwrap(),
                data,
            }),
        }
        .to_xdr(Limits::none())
        .unwrap()
    }

    fn transferred_v1() -> AvroSchema {
        AvroSchema::new(
            "Transferred",
            vec![
                serde_json::json!({"name": "from", "type": "string"}),
                serde_json::json!({"name": "to", "type": "string"}),
                serde_json::json!({"name": "amount", "type": "long"}),
            ],
        )
    }

    #[test]
    fn nullable_field_addition_decode_old_event() {
        let registry = InMemorySchemaRegistry::new();
        let old = transferred_v1();

        // v1: register and decode an event that has no `memo` key.
        futures::executor::block_on(registry.register(ContractEventSchema::new(
            "C1",
            "Transferred",
            old.raw.clone(),
            Compatibility::Backward,
            "hash-v1",
        )))
        .unwrap();

        let bytes = event_bytes(vec![
            ("from", string("alice")),
            ("to", string("bob")),
            ("amount", xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 1_000 })),
        ]);

        let v1_decoded = decode_event(&registry, "C1", "Transferred", &bytes).unwrap();
        assert_eq!(v1_decoded["from"], json!("alice"));
        assert_eq!(v1_decoded["to"], json!("bob"));
        assert_eq!(v1_decoded["amount"], json!(1000));
        assert!(v1_decoded.get("memo").is_none());

        // v2 adds a nullable `memo` with a null default. The compatibility
        // check must pass in both directions.
        let mut fields = old.fields().to_vec();
        fields.push(json!({
            "name": "memo",
            "type": ["null", "string"],
            "default": null,
        }));
        let new = AvroSchema::new("Transferred", fields);
        assert_eq!(old.check(&new, Compatibility::Backward), Ok(()));
        assert_eq!(old.check(&new, Compatibility::Full), Ok(()));

        futures::executor::block_on(registry.register(ContractEventSchema::new(
            "C1",
            "Transferred",
            new.raw,
            Compatibility::Backward,
            "hash-v2",
        )))
        .unwrap();
        assert_eq!(registry.latest_blocking("C1", "Transferred").unwrap().schema_version, 2);

        // The SAME old event now decodes with `"memo": null`.
        let v2_decoded = decode_event(&registry, "C1", "Transferred", &bytes).unwrap();
        assert_eq!(v2_decoded["from"], json!("alice"));
        assert_eq!(v2_decoded["amount"], json!(1000));
        assert_eq!(v2_decoded["memo"], Value::Null);
        assert_eq!(v2_decoded.as_object().unwrap().len(), 4);
    }

    #[test]
    fn unknown_extra_fields_are_ignored() {
        let mut lookup = StaticSchemaLookup::new();
        lookup.insert(
            "C1",
            "E",
            AvroSchema::new(
                "E",
                vec![serde_json::json!({"name": "kept", "type": "long"})],
            ),
        );
        let bytes = event_bytes(vec![
            ("kept", xdr::ScVal::U32(7)),
            ("surprise", string("ignored")),
        ]);
        let decoded = decode_event(&lookup, "C1", "E", &bytes).unwrap();
        assert_eq!(decoded, serde_json::json!({"kept": 7}));
    }

    #[test]
    fn missing_required_field_is_decode_error() {
        let mut lookup = StaticSchemaLookup::new();
        lookup.insert(
            "C1",
            "E",
            AvroSchema::new(
                "E",
                vec![serde_json::json!({"name": "required", "type": "string"})],
            ),
        );
        let bytes = event_bytes(vec![("other", string("x"))]);
        assert!(matches!(
            decode_event(&lookup, "C1", "E", &bytes),
            Err(DecodeError::MissingField(name)) if name == "required"
        ));
    }

    #[test]
    fn numbers_widen_to_strings_above_i64() {
        let mut lookup = StaticSchemaLookup::new();
        lookup.insert(
            "C1",
            "E",
            AvroSchema::new(
                "E",
                vec![serde_json::json!({"name": "big", "type": "long"})],
            ),
        );
        let bytes = event_bytes(vec![(
            "big",
            xdr::ScVal::U128(xdr::UInt128Parts { hi: 1, lo: 0 }),
        )]);
        let decoded = decode_event(&lookup, "C1", "E", &bytes).unwrap();
        assert_eq!(
            decoded["big"],
            serde_json::json!((1u128 << 64).to_string())
        );
    }

    #[test]
    fn no_schema_is_an_error() {
        let lookup = StaticSchemaLookup::new();
        let bytes = event_bytes(vec![]);
        assert!(matches!(
            decode_event(&lookup, "C1", "E", &bytes),
            Err(DecodeError::SchemaNotFound { .. })
        ));
    }
}
