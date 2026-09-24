//! Typed parsing of Soroban diagnostic events (issue #8).
//!
//! `simulateTransaction` responses can carry a base64-XDR-encoded
//! `events` array of [`xdr::DiagnosticEvent`]s alongside the simulated
//! return value. Unlike regular contract events, diagnostic events are
//! emitted for sub-contract invocations and for calls that ultimately
//! failed — exactly the call-tree and error-code information needed to
//! debug a failing transaction — and, before this module, the indexer
//! discarded them entirely.
//!
//! Every event topic name documented across the base contracts
//! (`registry`, `compliance`, `dividend`, `asset-token`) and the events
//! added by the contracts stacked on top of them (compliance hooks,
//! asset-token lockups) is recognized by [`classify_event_type`] below.
//! Anything else decodes as `"unknown"` rather than failing, and any event
//! whose XDR doesn't parse at all is logged and skipped — this module
//! never panics on untrusted RPC input.

use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr};

use crate::models::DiagnosticEventRecord;

use super::{scval_to_json, IndexError};

/// Parse a batch of base64-encoded `DiagnosticEvent` XDR blobs (as returned
/// under `events` in a `simulateTransaction` result) into structured
/// records. Each entry is decoded independently: a malformed entry is
/// logged and skipped, and never aborts the rest of the batch.
pub fn parse_diagnostic_events(events_b64: &[String]) -> Vec<DiagnosticEventRecord> {
    let mut out = Vec::with_capacity(events_b64.len());
    for (index, raw) in events_b64.iter().enumerate() {
        match parse_one(raw) {
            Ok(record) => out.push(record),
            Err(e) => {
                tracing::warn!(
                    index,
                    error = %e,
                    "skipping malformed diagnostic event"
                );
            }
        }
    }
    out
}

fn parse_one(raw: &str) -> Result<DiagnosticEventRecord, IndexError> {
    let event = xdr::DiagnosticEvent::from_xdr_base64(raw, Limits::none())?;

    let contract = event
        .event
        .contract_id
        .as_ref()
        .map(|xdr::ContractId(xdr::Hash(bytes))| stellar_strkey::Contract(*bytes).to_string());

    let xdr::ContractEventBody::V0(body) = &event.event.body;
    let mut topics = Vec::with_capacity(body.topics.len());
    for topic in body.topics.iter() {
        topics.push(scval_to_json(topic)?);
    }
    let data = scval_to_json(&body.data)?;
    let error_code = extract_error_code(&body.data);
    let event_type = classify_event_type(&body.topics);

    Ok(DiagnosticEventRecord {
        contract,
        event_type,
        topics,
        data,
        in_successful_contract_call: event.in_successful_contract_call,
        error_code,
    })
}

/// Contract error code, when the event's data is a Soroban contract error
/// value (`ScVal::Error(ScError::Contract(code))`). Other `ScError`
/// variants (host-level VM/storage/auth/budget errors) don't carry a
/// contract-defined code and decode as `None` here — they're still visible
/// in `data`.
fn extract_error_code(data: &xdr::ScVal) -> Option<u32> {
    match data {
        xdr::ScVal::Error(xdr::ScError::Contract(code)) => Some(*code),
        _ => None,
    }
}

/// Best-effort event kind from the first topic, when it's a symbol
/// matching one of the documented contract event names. Covers every
/// event topic emitted across the base contracts plus the events added on
/// top of them: registry (`register`/`deactivate`), compliance
/// (`approved`/`suspend`/`removed`/`blockjur`/`unblkjur`/`hookreg`/`hookunreg`),
/// dividend (`created`/`claim`), asset-token
/// (`mint`/`transfer`/`burn`/`pause`/`unpause`/`valuation`/`setcomp`/`clawback`/`lockup`),
/// and cap-table (`snapshot`). Anything else is `"unknown"` rather than a
/// parse failure — the raw topic is still available in `topics`.
fn classify_event_type(topics: &xdr::VecM<xdr::ScVal>) -> String {
    const KNOWN: &[&str] = &[
        "register",
        "deactivate",
        "approved",
        "suspend",
        "removed",
        "blockjur",
        "unblkjur",
        "hookreg",
        "hookunreg",
        "created",
        "claim",
        "mint",
        "transfer",
        "burn",
        "pause",
        "unpause",
        "valuation",
        "setcomp",
        "clawback",
        "lockup",
        "snapshot",
    ];

    let Some(xdr::ScVal::Symbol(sym)) = topics.iter().next() else {
        return "unknown".to_string();
    };
    let sym = sym.to_string();
    if KNOWN.contains(&sym.as_str()) {
        sym
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::curr::{
        ContractEvent, ContractEventBody, ContractEventType, ContractEventV0, ContractId,
        ExtensionPoint, Hash, ScError, ScVal, VecM, WriteXdr,
    };

    fn topics_of(names: &[&str]) -> VecM<ScVal> {
        names
            .iter()
            .map(|n| ScVal::Symbol(xdr::ScSymbol((*n).try_into().unwrap())))
            .collect::<Vec<_>>()
            .try_into()
            .unwrap()
    }

    fn encode(
        contract_id: Option<ContractId>,
        topics: VecM<ScVal>,
        data: ScVal,
        in_successful_contract_call: bool,
    ) -> String {
        let event = xdr::DiagnosticEvent {
            in_successful_contract_call,
            event: ContractEvent {
                ext: ExtensionPoint::V0,
                contract_id,
                type_: ContractEventType::Contract,
                body: ContractEventBody::V0(ContractEventV0 { topics, data }),
            },
        };
        event.to_xdr_base64(Limits::none()).unwrap()
    }

    #[test]
    fn parses_known_event_with_contract_and_data() {
        let contract_bytes = [7u8; 32];
        let b64 = encode(
            Some(ContractId(Hash(contract_bytes))),
            topics_of(&["transfer"]),
            ScVal::I128(xdr::Int128Parts { hi: 0, lo: 500 }),
            true,
        );

        let parsed = parse_diagnostic_events(&[b64]);
        assert_eq!(parsed.len(), 1);
        let record = &parsed[0];
        assert_eq!(record.event_type, "transfer");
        assert!(record.in_successful_contract_call);
        assert!(record.contract.is_some());
        assert_eq!(record.error_code, None);
    }

    #[test]
    fn extracts_contract_error_code_from_failed_call() {
        let b64 = encode(
            None,
            topics_of(&["lockup"]),
            ScVal::Error(ScError::Contract(10)),
            false,
        );

        let parsed = parse_diagnostic_events(&[b64]);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].error_code, Some(10));
        assert!(!parsed[0].in_successful_contract_call);
    }

    #[test]
    fn unrecognized_topic_classifies_as_unknown_without_failing() {
        let b64 = encode(None, topics_of(&["totally_custom"]), ScVal::Void, true);
        let parsed = parse_diagnostic_events(&[b64]);
        assert_eq!(parsed[0].event_type, "unknown");
    }

    #[test]
    fn malformed_base64_is_skipped_not_panicked() {
        let parsed = parse_diagnostic_events(&["not valid base64 xdr".to_string()]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn empty_topics_classify_as_unknown() {
        let b64 = encode(None, VecM::default(), ScVal::Void, true);
        let parsed = parse_diagnostic_events(&[b64]);
        assert_eq!(parsed[0].event_type, "unknown");
    }
}
