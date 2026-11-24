//! Board-approved (m-of-n) transfers, API side of issue #478.
//!
//! The compliance contract's `MultiSigTransferPolicy` publishes four events,
//! which the indexer feeds into a [`PendingTransferBook`]:
//!
//! | topic | value |
//! |---|---|
//! | `["ms_pol", token]` | `[threshold_amount, required_signatures, board, version, expiry_ledgers]` |
//! | `["ms_req", id]` | `[from, to, amount, expires_at]` |
//! | `["ms_sign", id]` | `[signer, signature_count]` |
//! | `["ms_exec", id]` | `[from, to, amount, signers]` |
//!
//! The book serves `GET /v1/assets/:id/pending-transfers`. Board members sign
//! with their own wallet: `POST /v1/assets/:id/pending-transfers/:tid/sign`
//! takes a signed transaction that calls `co_sign(signer, tid)` on the asset's
//! compliance contract. [`check_co_sign_envelope`] makes sure it does exactly
//! that before it is relayed to Soroban RPC, so the endpoint cannot be used to
//! relay arbitrary transactions.
//!
//! Every multisig event and every relayed signature is appended to the
//! hash-chained audit log.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use stellar_xdr::curr::{
    self as xdr, HostFunction, Limits, OperationBody, ReadXdr, ScAddress, ScVal,
    TransactionEnvelope,
};

use crate::audit::{AuditAction, AuditLog};
use crate::models::Event;

/// Policy as last published by a compliance contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardPolicy {
    pub token: String,
    pub threshold_amount: String,
    pub required_signatures: u32,
    pub board: Vec<String>,
    pub version: u32,
    pub expiry_ledgers: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingStatus {
    Pending,
    Executed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingTransferView {
    pub id: u64,
    pub from: String,
    pub to: String,
    /// Raw i128 amount in base units, as a string.
    pub amount: String,
    pub created_at_ledger: u32,
    pub expires_at_ledger: u32,
    pub required_signatures: u32,
    pub signers: Vec<String>,
    pub status: PendingStatus,
    /// Policy version the request was made under; signing is refused on-chain
    /// once the policy changes.
    pub policy_version: u32,
}

#[derive(Default)]
struct ContractBook {
    policy: Option<BoardPolicy>,
    transfers: BTreeMap<u64, PendingTransferView>,
}

/// Pending board transfers per compliance contract, rebuilt from events.
#[derive(Default)]
pub struct PendingTransferBook {
    contracts: Mutex<HashMap<String, ContractBook>>,
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// An entry to append to the audit trail for an applied event.
#[derive(Debug, Clone, PartialEq)]
pub struct MultisigAudit {
    pub action: &'static str,
    pub target: String,
    pub details: Value,
}

impl PendingTransferBook {
    /// Applies one compliance-contract event. Returns the audit entry to
    /// record, or `None` for events that are not multisig events.
    pub fn apply(&self, event: &Event) -> Option<MultisigAudit> {
        let topics = event.data.get("topics")?.as_array()?;
        let value = event.data.get("value")?.as_array()?;
        let kind = topics.first()?.as_str()?;
        let mut books = self.contracts.lock().unwrap_or_else(|e| e.into_inner());
        let book = books.entry(event.contract.clone()).or_default();
        let target = event.contract.clone();

        match kind {
            "ms_pol" => {
                let policy = BoardPolicy {
                    token: as_string(topics.get(1)?)?,
                    threshold_amount: as_string(value.first()?)?,
                    required_signatures: as_u64(value.get(1)?)? as u32,
                    board: value
                        .get(2)?
                        .as_array()?
                        .iter()
                        .filter_map(as_string)
                        .collect(),
                    version: as_u64(value.get(3)?)? as u32,
                    expiry_ledgers: value.get(4).and_then(as_u64).unwrap_or(0) as u32,
                };
                let details = serde_json::to_value(&policy).unwrap_or_default();
                book.policy = Some(policy);
                Some(MultisigAudit {
                    action: "multisig.policy_set",
                    target,
                    details,
                })
            }
            "ms_req" => {
                let id = as_u64(topics.get(1)?)?;
                let view = PendingTransferView {
                    id,
                    from: as_string(value.first()?)?,
                    to: as_string(value.get(1)?)?,
                    amount: as_string(value.get(2)?)?,
                    created_at_ledger: event.ledger,
                    expires_at_ledger: as_u64(value.get(3)?)? as u32,
                    required_signatures: book.policy.as_ref().map_or(0, |p| p.required_signatures),
                    signers: Vec::new(),
                    status: PendingStatus::Pending,
                    policy_version: book.policy.as_ref().map_or(0, |p| p.version),
                };
                let details = json!({
                    "transfer_id": id, "from": view.from, "to": view.to,
                    "amount": view.amount, "expires_at_ledger": view.expires_at_ledger,
                });
                book.transfers.insert(id, view);
                Some(MultisigAudit {
                    action: "multisig.transfer_requested",
                    target,
                    details,
                })
            }
            "ms_sign" => {
                let id = as_u64(topics.get(1)?)?;
                let signer = as_string(value.first()?)?;
                let count = as_u64(value.get(1)?)?;
                if let Some(t) = book.transfers.get_mut(&id) {
                    if !t.signers.contains(&signer) {
                        t.signers.push(signer.clone());
                    }
                }
                Some(MultisigAudit {
                    action: "multisig.co_signed",
                    target,
                    details: json!({ "transfer_id": id, "signer": signer, "signatures": count }),
                })
            }
            "ms_exec" => {
                let id = as_u64(topics.get(1)?)?;
                let signers: Vec<String> = value
                    .get(3)
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(as_string).collect())
                    .unwrap_or_default();
                if let Some(t) = book.transfers.get_mut(&id) {
                    t.status = PendingStatus::Executed;
                    t.signers = signers.clone();
                }
                Some(MultisigAudit {
                    action: "multisig.transfer_executed",
                    target,
                    details: json!({
                        "transfer_id": id,
                        "from": value.first().cloned().unwrap_or_default(),
                        "to": value.get(1).cloned().unwrap_or_default(),
                        "amount": value.get(2).cloned().unwrap_or_default(),
                        "signers": signers,
                    }),
                })
            }
            _ => None,
        }
    }

    /// Feeds new indexer events and records each multisig action in the
    /// audit trail.
    pub async fn observe_events(&self, events: &[Event], audit: &AuditLog) {
        for event in events {
            if let Some(entry) = self.apply(event) {
                let _ = audit
                    .append(
                        AuditAction::AdministrativeAction {
                            action: entry.action.to_string(),
                            details: entry.details.clone(),
                        },
                        "compliance-contract".to_string(),
                        Some(entry.target),
                        json!({ "ledger": event.ledger, "event_id": event.id }),
                    )
                    .await;
            }
        }
    }

    pub fn policy(&self, compliance: &str) -> Option<BoardPolicy> {
        let books = self.contracts.lock().unwrap_or_else(|e| e.into_inner());
        books.get(compliance).and_then(|b| b.policy.clone())
    }

    /// Transfers still awaiting signatures and not yet expired at `ledger`.
    pub fn pending(&self, compliance: &str, ledger: u32) -> Vec<PendingTransferView> {
        let books = self.contracts.lock().unwrap_or_else(|e| e.into_inner());
        books
            .get(compliance)
            .map(|b| {
                b.transfers
                    .values()
                    .filter(|t| t.status == PendingStatus::Pending && ledger <= t.expires_at_ledger)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, compliance: &str, id: u64) -> Option<PendingTransferView> {
        let books = self.contracts.lock().unwrap_or_else(|e| e.into_inner());
        books
            .get(compliance)
            .and_then(|b| b.transfers.get(&id).cloned())
    }
}

// ── co_sign transaction checks ──────────────────────────────────────────────

/// Why a submitted transaction is not an acceptable `co_sign` call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("signed_transaction_xdr is not a base64 TransactionEnvelope")]
    Malformed,
    #[error("the transaction carries no signatures")]
    Unsigned,
    #[error("the transaction must contain exactly one operation, a contract invocation")]
    NotASingleInvocation,
    #[error("the transaction must call `co_sign` on the asset's compliance contract {0}")]
    WrongCall(String),
    #[error("co_sign arguments must be (signer, transfer_id) = ({0}, {1})")]
    WrongArguments(String, u64),
}

fn sc_address_string(a: &ScAddress) -> Option<String> {
    match a {
        ScAddress::Account(xdr::AccountId(xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256(
            b,
        )))) => Some(stellar_strkey::ed25519::PublicKey(*b).to_string()),
        ScAddress::Contract(xdr::ContractId(xdr::Hash(b))) => {
            Some(stellar_strkey::Contract(*b).to_string())
        }
        _ => None,
    }
}

/// Checks that `envelope_b64` is a signed transaction whose only operation
/// invokes `compliance.co_sign(signer, transfer_id)`.
pub fn check_co_sign_envelope(
    envelope_b64: &str,
    compliance: &str,
    signer: &str,
    transfer_id: u64,
) -> Result<(), EnvelopeError> {
    let envelope =
        TransactionEnvelope::from_xdr_base64(envelope_b64.trim(), Limits::len(256 * 1024))
            .map_err(|_| EnvelopeError::Malformed)?;
    let (operations, signatures) = match &envelope {
        TransactionEnvelope::Tx(e) => (e.tx.operations.as_slice(), e.signatures.len()),
        TransactionEnvelope::TxFeeBump(e) => match &e.tx.inner_tx {
            xdr::FeeBumpTransactionInnerTx::Tx(inner) => {
                (inner.tx.operations.as_slice(), inner.signatures.len())
            }
        },
        TransactionEnvelope::TxV0(_) => return Err(EnvelopeError::NotASingleInvocation),
    };
    if signatures == 0 {
        return Err(EnvelopeError::Unsigned);
    }
    let [op] = operations else {
        return Err(EnvelopeError::NotASingleInvocation);
    };
    let OperationBody::InvokeHostFunction(invoke) = &op.body else {
        return Err(EnvelopeError::NotASingleInvocation);
    };
    let HostFunction::InvokeContract(call) = &invoke.host_function else {
        return Err(EnvelopeError::NotASingleInvocation);
    };
    if sc_address_string(&call.contract_address).as_deref() != Some(compliance)
        || call.function_name.0.as_slice() != b"co_sign"
    {
        return Err(EnvelopeError::WrongCall(compliance.to_string()));
    }
    let args_ok = matches!(
        call.args.as_slice(),
        [ScVal::Address(a), ScVal::U64(id)]
            if sc_address_string(a).as_deref() == Some(signer) && *id == transfer_id
    );
    if !args_ok {
        return Err(EnvelopeError::WrongArguments(
            signer.to_string(),
            transfer_id,
        ));
    }
    Ok(())
}

// ── Relaying to Soroban RPC ─────────────────────────────────────────────────

/// Result of `sendTransaction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Submission {
    pub hash: String,
    /// `PENDING`, `DUPLICATE`, `TRY_AGAIN_LATER` or `ERROR`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_result_xdr: Option<String>,
}

#[async_trait]
pub trait TransactionSubmitter: Send + Sync {
    async fn submit(&self, envelope_b64: &str) -> Result<Submission, String>;
}

/// Submits through the configured Soroban RPC endpoints, in order.
pub struct RpcSubmitter {
    client: reqwest::Client,
    urls: Vec<String>,
}

impl RpcSubmitter {
    pub fn new(urls: Vec<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { client, urls }
    }
}

#[async_trait]
impl TransactionSubmitter for RpcSubmitter {
    async fn submit(&self, envelope_b64: &str) -> Result<Submission, String> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
            "params": { "transaction": envelope_b64 },
        });
        let mut last = "no Soroban RPC endpoint configured".to_string();
        for url in &self.urls {
            let resp = match self.client.post(url).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    last = e.to_string();
                    continue;
                }
            };
            let v: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    last = e.to_string();
                    continue;
                }
            };
            if let Some(err) = v.get("error") {
                last = err.to_string();
                continue;
            }
            let r = &v["result"];
            return Ok(Submission {
                hash: r["hash"].as_str().unwrap_or_default().to_string(),
                status: r["status"].as_str().unwrap_or("ERROR").to_string(),
                error_result_xdr: r["errorResultXdr"].as_str().map(str::to_string),
            });
        }
        Err(last)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use stellar_xdr::curr::{
        ContractId, DecoratedSignature, Hash, InvokeContractArgs, InvokeHostFunctionOp, Memo,
        MuxedAccount, Operation, Preconditions, ScSymbol, SequenceNumber, Signature, SignatureHint,
        Transaction, TransactionExt, TransactionV1Envelope, Uint256, VecM, WriteXdr,
    };

    pub(crate) const COMPLIANCE: &str = "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU";

    pub(crate) fn account(n: u8) -> String {
        stellar_strkey::ed25519::PublicKey([n; 32]).to_string()
    }

    fn contract_bytes(id: &str) -> [u8; 32] {
        stellar_strkey::Contract::from_string(id).unwrap().0
    }

    /// A signed envelope calling `contract.function(args)`.
    pub(crate) fn envelope(
        contract: &str,
        function: &str,
        args: Vec<ScVal>,
        signed: bool,
    ) -> String {
        let op = Operation {
            source_account: None,
            body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
                host_function: HostFunction::InvokeContract(InvokeContractArgs {
                    contract_address: ScAddress::Contract(ContractId(Hash(contract_bytes(
                        contract,
                    )))),
                    function_name: ScSymbol(function.try_into().unwrap()),
                    args: args.try_into().unwrap(),
                }),
                auth: VecM::default(),
            }),
        };
        let signatures: Vec<DecoratedSignature> = if signed {
            vec![DecoratedSignature {
                hint: SignatureHint([1, 2, 3, 4]),
                signature: Signature(vec![7u8; 64].try_into().unwrap()),
            }]
        } else {
            vec![]
        };
        TransactionEnvelope::Tx(TransactionV1Envelope {
            tx: Transaction {
                source_account: MuxedAccount::Ed25519(Uint256([9; 32])),
                fee: 100_000,
                seq_num: SequenceNumber(1),
                cond: Preconditions::None,
                memo: Memo::None,
                operations: vec![op].try_into().unwrap(),
                ext: TransactionExt::V0,
            },
            signatures: signatures.try_into().unwrap(),
        })
        .to_xdr_base64(Limits::none())
        .unwrap()
    }

    pub(crate) fn co_sign_args(signer: &str, id: u64) -> Vec<ScVal> {
        let key = stellar_strkey::ed25519::PublicKey::from_string(signer)
            .unwrap()
            .0;
        vec![
            ScVal::Address(ScAddress::Account(xdr::AccountId(
                xdr::PublicKey::PublicKeyTypeEd25519(Uint256(key)),
            ))),
            ScVal::U64(id),
        ]
    }

    pub(crate) fn event(kind: &str, key: Value, value: Value, ledger: u32) -> Event {
        Event {
            id: u64::from(ledger) * 10 + kind.len() as u64,
            contract: COMPLIANCE.into(),
            event_type: kind.into(),
            ledger,
            timestamp: None,
            data: json!({ "topics": [kind, key], "value": value }),
        }
    }

    pub(crate) fn board() -> Vec<String> {
        (1..=5).map(account).collect()
    }

    /// A 3-of-5 policy and one pending request (id 1) with one signature.
    pub(crate) fn seeded_book() -> PendingTransferBook {
        let book = PendingTransferBook::default();
        book.apply(&event(
            "ms_pol",
            json!("CTOKEN"),
            json!(["10000", 3, board(), 1, 17280]),
            100,
        ));
        book.apply(&event(
            "ms_req",
            json!(1),
            json!([account(20), account(21), "25000", 17_380]),
            100,
        ));
        book.apply(&event("ms_sign", json!(1), json!([account(1), 1]), 101));
        book
    }

    #[test]
    fn events_build_the_pending_list() {
        let book = seeded_book();
        let pending = book.pending(COMPLIANCE, 150);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].required_signatures, 3);
        assert_eq!(pending[0].signers, vec![account(1)]);
        assert_eq!(book.policy(COMPLIANCE).unwrap().board.len(), 5);

        // Expired requests drop out of the list.
        assert!(book.pending(COMPLIANCE, 17_381).is_empty());

        // Executed requests drop out too.
        book.apply(&event(
            "ms_exec",
            json!(1),
            json!([
                account(20),
                account(21),
                "25000",
                [account(1), account(2), account(3)]
            ]),
            102,
        ));
        assert!(book.pending(COMPLIANCE, 150).is_empty());
        assert_eq!(
            book.get(COMPLIANCE, 1).unwrap().status,
            PendingStatus::Executed
        );
    }

    #[test]
    fn non_multisig_events_are_ignored() {
        let book = PendingTransferBook::default();
        assert!(book
            .apply(&event("transfer", json!(account(1)), json!(["5"]), 1))
            .is_none());
    }

    #[test]
    fn only_a_signed_co_sign_call_for_this_transfer_is_accepted() {
        let signer = account(2);
        let ok = envelope(COMPLIANCE, "co_sign", co_sign_args(&signer, 1), true);
        assert_eq!(check_co_sign_envelope(&ok, COMPLIANCE, &signer, 1), Ok(()));

        let unsigned = envelope(COMPLIANCE, "co_sign", co_sign_args(&signer, 1), false);
        assert_eq!(
            check_co_sign_envelope(&unsigned, COMPLIANCE, &signer, 1),
            Err(EnvelopeError::Unsigned)
        );

        let other_fn = envelope(
            COMPLIANCE,
            "set_multisig_policy",
            co_sign_args(&signer, 1),
            true,
        );
        assert!(matches!(
            check_co_sign_envelope(&other_fn, COMPLIANCE, &signer, 1),
            Err(EnvelopeError::WrongCall(_))
        ));

        let other_contract = envelope(
            "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3",
            "co_sign",
            co_sign_args(&signer, 1),
            true,
        );
        assert!(matches!(
            check_co_sign_envelope(&other_contract, COMPLIANCE, &signer, 1),
            Err(EnvelopeError::WrongCall(_))
        ));

        let other_id = envelope(COMPLIANCE, "co_sign", co_sign_args(&signer, 2), true);
        assert!(matches!(
            check_co_sign_envelope(&other_id, COMPLIANCE, &signer, 1),
            Err(EnvelopeError::WrongArguments(..))
        ));

        let other_signer = envelope(COMPLIANCE, "co_sign", co_sign_args(&account(3), 1), true);
        assert!(matches!(
            check_co_sign_envelope(&other_signer, COMPLIANCE, &signer, 1),
            Err(EnvelopeError::WrongArguments(..))
        ));

        assert_eq!(
            check_co_sign_envelope("not-xdr", COMPLIANCE, &signer, 1),
            Err(EnvelopeError::Malformed)
        );
    }
}
