//! SEC EDGAR Form 4 import and cap-table reconciliation (issue #476).
//!
//! `POST /v1/admin/cap-table/import-edgar` fetches the issuer's recent Form 4
//! filings from EDGAR, takes each reporting owner's direct holding after their
//! latest reported transaction, maps owners to Stellar addresses
//! (`holder_map`, CIK → address), and reconciles that against the indexed
//! on-chain holders:
//!
//! * `new`: on-chain holder with no filing,
//! * `departed`: filing holder with no on-chain balance,
//! * `changed`: both, with a different balance (absolute and % change),
//! * `unchanged`.
//!
//! A row is highlighted `yellow` above a 5 % change and `red` above 10 %. Any
//! row whose discrepancy exceeds 1 % of total supply is sent to the compliance
//! webhook. The report is signed with Ed25519 over the SHA-256 of its
//! canonical JSON, so an exported PDF can be verified against the API's
//! public key.
//!
//! EDGAR requires a descriptive `User-Agent` with a contact address
//! (`RWA_SEC_USER_AGENT`), and allows at most 10 requests per second.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// ── Form 4 parsing ──────────────────────────────────────────────────────────

/// One reporting owner's holding as of a Form 4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Form4Holding {
    pub owner_cik: String,
    pub owner_name: String,
    /// Direct holding after the owner's last transaction in the filing.
    pub shares: f64,
    /// Indirect holding (trusts, family members), reported for context only.
    pub indirect_shares: f64,
    pub period_of_report: String,
    pub accession_number: String,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EdgarError {
    #[error("not a Form 4 ownershipDocument")]
    NotForm4,
    #[error("EDGAR request failed: {0}")]
    Http(String),
    #[error("unexpected EDGAR response: {0}")]
    Format(String),
}

fn first<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

/// `<tag><value>x</value></tag>` or `<tag>x</tag>`.
fn value_of<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let inner = first(xml, tag)?;
    Some(first(inner, "value").unwrap_or(inner))
}

fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let re = Regex::new(&format!(r"(?s)<{tag}>(.*?)</{tag}>")).expect("static tag regex");
    re.captures_iter(xml)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect()
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Parses a Form 4 (or 4/A) `ownershipDocument`. Each reporting owner gets
/// the `sharesOwnedFollowingTransaction` of their last non-derivative
/// transaction or holding, split into direct (`D`) and indirect (`I`).
pub fn parse_form4(xml: &str, accession_number: &str) -> Result<Vec<Form4Holding>, EdgarError> {
    let doc = first(xml, "ownershipDocument").ok_or(EdgarError::NotForm4)?;
    let doc_type = first(doc, "documentType").unwrap_or_default();
    if doc_type != "4" && doc_type != "4/A" {
        return Err(EdgarError::NotForm4);
    }
    let period = first(doc, "periodOfReport").unwrap_or_default().to_string();

    // Positions after each transaction, in document order. A filing reports a
    // single position for all of its reporting owners (joint filers), so the
    // same values apply to each of them.
    let mut direct = None;
    let mut indirect = None;
    let table = first(doc, "nonDerivativeTable").unwrap_or_default();
    let mut rows = blocks(table, "nonDerivativeTransaction");
    rows.extend(blocks(table, "nonDerivativeHolding"));
    for row in rows {
        let Some(shares) = value_of(row, "sharesOwnedFollowingTransaction")
            .and_then(|v| v.replace(',', "").parse::<f64>().ok())
        else {
            continue;
        };
        match value_of(row, "directOrIndirectOwnership") {
            Some("I") => indirect = Some(shares),
            _ => direct = Some(shares),
        }
    }

    let owners: Vec<Form4Holding> = blocks(doc, "reportingOwner")
        .into_iter()
        .filter_map(|owner| {
            let id = first(owner, "reportingOwnerId")?;
            Some(Form4Holding {
                owner_cik: normalize_cik(first(id, "rptOwnerCik")?),
                owner_name: unescape(first(id, "rptOwnerName").unwrap_or_default()),
                shares: direct.unwrap_or(0.0),
                indirect_shares: indirect.unwrap_or(0.0),
                period_of_report: period.clone(),
                accession_number: accession_number.to_string(),
            })
        })
        .collect();
    if owners.is_empty() {
        return Err(EdgarError::Format(
            "Form 4 without a reporting owner".into(),
        ));
    }
    Ok(owners)
}

/// CIKs are compared as 10-digit, zero-padded strings.
pub fn normalize_cik(cik: &str) -> String {
    let digits: String = cik.chars().filter(char::is_ascii_digit).collect();
    format!("{:0>10}", digits.trim_start_matches('0'))
}

/// A Form 4 listed in the EDGAR submissions index.
#[derive(Debug, Clone, PartialEq)]
pub struct FilingRef {
    pub accession_number: String,
    pub filing_date: String,
    /// The raw XML document (the `xslF345X0n/` rendering prefix removed).
    pub document: String,
}

/// Form 4 and 4/A filings from `https://data.sec.gov/submissions/CIK##########.json`,
/// newest first.
pub fn form4_filings(submissions: &Value, limit: usize) -> Result<Vec<FilingRef>, EdgarError> {
    let recent = submissions
        .pointer("/filings/recent")
        .ok_or_else(|| EdgarError::Format("missing filings.recent".into()))?;
    let col = |name: &str| -> Result<Vec<String>, EdgarError> {
        recent[name]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .ok_or_else(|| EdgarError::Format(format!("missing filings.recent.{name}")))
    };
    let (acc, dates, forms, docs) = (
        col("accessionNumber")?,
        col("filingDate")?,
        col("form")?,
        col("primaryDocument")?,
    );
    let mut out: Vec<FilingRef> = (0..acc.len().min(dates.len()).min(forms.len()).min(docs.len()))
        .filter(|&i| forms[i] == "4" || forms[i] == "4/A")
        .map(|i| FilingRef {
            accession_number: acc[i].clone(),
            filing_date: dates[i].clone(),
            document: docs[i].rsplit('/').next().unwrap_or(&docs[i]).to_string(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.filing_date
            .cmp(&a.filing_date)
            .then_with(|| b.accession_number.cmp(&a.accession_number))
    });
    out.truncate(limit);
    Ok(out)
}

/// Latest holding per owner across filings (by period of report, then
/// accession number, so a later amendment wins).
pub fn latest_holdings(filings: impl IntoIterator<Item = Form4Holding>) -> Vec<Form4Holding> {
    let mut by_owner: BTreeMap<String, Form4Holding> = BTreeMap::new();
    for h in filings {
        let newer = by_owner.get(&h.owner_cik).is_none_or(|cur| {
            (h.period_of_report.as_str(), h.accession_number.as_str())
                > (cur.period_of_report.as_str(), cur.accession_number.as_str())
        });
        if newer {
            by_owner.insert(h.owner_cik.clone(), h);
        }
    }
    by_owner.into_values().collect()
}

#[async_trait]
pub trait EdgarSource: Send + Sync {
    /// Latest Form 4 holdings per reporting owner for an issuer.
    async fn holdings(
        &self,
        issuer_cik: &str,
        limit: usize,
    ) -> Result<Vec<Form4Holding>, EdgarError>;
}

/// The public EDGAR API.
pub struct SecEdgar {
    client: reqwest::Client,
    data_base: String,
    archive_base: String,
}

impl SecEdgar {
    pub fn new(user_agent: String) -> Self {
        Self::with_bases(
            user_agent,
            "https://data.sec.gov".into(),
            "https://www.sec.gov".into(),
        )
    }

    pub fn with_bases(user_agent: String, data_base: String, archive_base: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            client,
            data_base,
            archive_base,
        }
    }

    async fn get(&self, url: &str) -> Result<String, EdgarError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| EdgarError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(EdgarError::Http(format!(
                "GET {url}: HTTP {}",
                resp.status()
            )));
        }
        resp.text()
            .await
            .map_err(|e| EdgarError::Http(e.to_string()))
    }
}

/// Used until `RWA_SEC_USER_AGENT` is set: EDGAR rejects anonymous clients.
pub struct UnconfiguredEdgar;

#[async_trait]
impl EdgarSource for UnconfiguredEdgar {
    async fn holdings(
        &self,
        _issuer_cik: &str,
        _limit: usize,
    ) -> Result<Vec<Form4Holding>, EdgarError> {
        Err(EdgarError::Http(
            "EDGAR import is disabled: set RWA_SEC_USER_AGENT to \"<company> <contact email>\""
                .into(),
        ))
    }
}

#[async_trait]
impl EdgarSource for SecEdgar {
    async fn holdings(
        &self,
        issuer_cik: &str,
        limit: usize,
    ) -> Result<Vec<Form4Holding>, EdgarError> {
        let cik = normalize_cik(issuer_cik);
        let index = self
            .get(&format!("{}/submissions/CIK{cik}.json", self.data_base))
            .await?;
        let index: Value =
            serde_json::from_str(&index).map_err(|e| EdgarError::Format(e.to_string()))?;
        let cik_number = cik.trim_start_matches('0');
        let mut all = Vec::new();
        for filing in form4_filings(&index, limit)? {
            let url = format!(
                "{}/Archives/edgar/data/{cik_number}/{}/{}",
                self.archive_base,
                filing.accession_number.replace('-', ""),
                filing.document
            );
            let xml = self.get(&url).await?;
            all.extend(parse_form4(&xml, &filing.accession_number)?);
            // Stay well under EDGAR's 10 requests per second.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        Ok(latest_holdings(all))
    }
}

// ── Reconciliation ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    New,
    Departed,
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Highlight {
    None,
    Yellow,
    Red,
}

/// Yellow above 5 % change, red above 10 % (issue #476). New and departed
/// holders are 100 % changes.
pub fn highlight(percent_change: f64) -> Highlight {
    let p = percent_change.abs();
    if p > 10.0 {
        Highlight::Red
    } else if p > 5.0 {
        Highlight::Yellow
    } else {
        Highlight::None
    }
}

/// Discrepancies above this share of total supply go to the compliance webhook.
pub const COMPLIANCE_DISCREPANCY_PERCENT: f64 = 1.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationRow {
    pub address: Option<String>,
    pub owner_cik: Option<String>,
    pub owner_name: Option<String>,
    /// Token units (whole tokens, decimals applied).
    pub on_chain: f64,
    /// Filing shares converted to tokens.
    pub filing: f64,
    pub kind: DiffKind,
    pub absolute_change: f64,
    /// Change relative to the filing (100 for new and departed holders).
    pub percent_change: f64,
    pub highlight: Highlight,
    /// `|absolute_change|` as a share of total supply.
    pub supply_percent: f64,
    pub compliance_alert: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub asset_id: u64,
    pub issuer_cik: String,
    pub total_supply: f64,
    pub tokens_per_share: f64,
    pub generated_at: String,
    pub rows: Vec<ReconciliationRow>,
    pub counts: BTreeMap<String, usize>,
    pub compliance_alerts: usize,
    pub unmapped_owners: Vec<String>,
}

pub struct OnChainHolder<'a> {
    pub address: &'a str,
    pub tokens: f64,
}

/// Reconciles on-chain holders (in tokens) against filing holdings.
/// `holder_map` maps owner CIK → Stellar address. Owners without a mapping
/// are listed in `unmapped_owners` and not compared.
#[allow(clippy::too_many_arguments)]
pub fn reconcile(
    asset_id: u64,
    issuer_cik: &str,
    on_chain: &[OnChainHolder<'_>],
    filings: &[Form4Holding],
    holder_map: &HashMap<String, String>,
    tokens_per_share: f64,
    total_supply: f64,
    generated_at: String,
) -> ReconciliationReport {
    let map: HashMap<String, String> = holder_map
        .iter()
        .map(|(k, v)| (normalize_cik(k), v.clone()))
        .collect();
    let mut by_address: BTreeMap<String, (f64, Option<&Form4Holding>)> = BTreeMap::new();
    let mut unmapped = Vec::new();
    for f in filings {
        match map.get(&f.owner_cik) {
            Some(addr) => {
                let entry = by_address.entry(addr.clone()).or_insert((0.0, None));
                entry.1 = Some(f);
            }
            None => unmapped.push(format!("{} ({})", f.owner_name, f.owner_cik)),
        }
    }
    for h in on_chain.iter().filter(|h| h.tokens > 0.0) {
        by_address
            .entry(h.address.to_string())
            .or_insert((0.0, None))
            .0 = h.tokens;
    }

    let rows: Vec<ReconciliationRow> = by_address
        .into_iter()
        .map(|(address, (chain, filing))| {
            let filed = filing.map_or(0.0, |f| f.shares * tokens_per_share);
            let kind = match (chain > 0.0, filed > 0.0) {
                (true, false) => DiffKind::New,
                (false, true) => DiffKind::Departed,
                _ if (chain - filed).abs() < 1e-9 => DiffKind::Unchanged,
                _ => DiffKind::Changed,
            };
            let absolute_change = chain - filed;
            let percent_change = match kind {
                DiffKind::New => 100.0,
                DiffKind::Departed => -100.0,
                _ if filed > 0.0 => absolute_change / filed * 100.0,
                _ => 0.0,
            };
            let supply_percent = if total_supply > 0.0 {
                absolute_change.abs() / total_supply * 100.0
            } else {
                0.0
            };
            ReconciliationRow {
                address: Some(address),
                owner_cik: filing.map(|f| f.owner_cik.clone()),
                owner_name: filing.map(|f| f.owner_name.clone()),
                on_chain: chain,
                filing: filed,
                kind,
                absolute_change,
                percent_change,
                highlight: highlight(percent_change),
                supply_percent,
                compliance_alert: supply_percent > COMPLIANCE_DISCREPANCY_PERCENT,
            }
        })
        .collect();

    let mut counts = BTreeMap::new();
    for r in &rows {
        let k = serde_json::to_value(r.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        *counts.entry(k).or_insert(0) += 1;
    }
    let compliance_alerts = rows.iter().filter(|r| r.compliance_alert).count();
    ReconciliationReport {
        asset_id,
        issuer_cik: normalize_cik(issuer_cik),
        total_supply,
        tokens_per_share,
        generated_at,
        rows,
        counts,
        compliance_alerts,
        unmapped_owners: unmapped,
    }
}

// ── Compliance webhook ──────────────────────────────────────────────────────

/// Receives reconciliation discrepancies above 1 % of supply.
#[async_trait]
pub trait ComplianceAlerts: Send + Sync {
    async fn discrepancy(&self, report: &ReconciliationReport, row: &ReconciliationRow);
}

/// Used when no webhook is configured; discrepancies are still in the
/// response and the audit trail.
pub struct NoAlerts;

#[async_trait]
impl ComplianceAlerts for NoAlerts {
    async fn discrepancy(&self, _report: &ReconciliationReport, _row: &ReconciliationRow) {}
}

// ── Signing ─────────────────────────────────────────────────────────────────

/// Signature over a report, embedded in the exported PDF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSignature {
    pub algorithm: String,
    /// Hex SHA-256 of the canonical (key-sorted, compact) report JSON.
    pub digest: String,
    pub signature: String,
    pub public_key: String,
    pub signed_at: String,
}

/// Canonical JSON: object keys sorted, no whitespace. `serde_json::Value`
/// objects are `BTreeMap`s (no `preserve_order`), so re-serializing sorts them.
pub fn canonical_json(value: &impl Serialize) -> Vec<u8> {
    let v: Value = serde_json::to_value(value).unwrap_or(Value::Null);
    serde_json::to_vec(&v).unwrap_or_default()
}

pub struct ReportSigner {
    key: ed25519_dalek::SigningKey,
}

impl ReportSigner {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            key: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    /// `RWA_REPORT_SIGNING_KEY`: hex Ed25519 seed (32 bytes).
    pub fn from_env() -> Option<Self> {
        let hex_seed = std::env::var("RWA_REPORT_SIGNING_KEY").ok()?;
        let bytes: [u8; 32] = hex::decode(hex_seed.trim()).ok()?.try_into().ok()?;
        Some(Self::from_seed(bytes))
    }

    /// A key for this process only. Signatures still verify against the
    /// embedded public key, but the key changes on restart; set
    /// `RWA_REPORT_SIGNING_KEY` in production.
    pub fn ephemeral() -> Self {
        Self::from_seed(rand::random())
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }

    pub fn sign(&self, value: &impl Serialize, signed_at: String) -> ReportSignature {
        use ed25519_dalek::Signer;
        let digest = Sha256::digest(canonical_json(value));
        ReportSignature {
            algorithm: "ed25519-sha256".into(),
            digest: hex::encode(digest),
            signature: hex::encode(self.key.sign(&digest).to_bytes()),
            public_key: self.public_key_hex(),
            signed_at,
        }
    }
}

/// Checks a signature produced by [`ReportSigner::sign`].
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by tests and by integrators verifying exported reports"
    )
)]
pub fn verify_report(value: &impl Serialize, sig: &ReportSignature) -> bool {
    let digest = Sha256::digest(canonical_json(value));
    if hex::encode(digest) != sig.digest {
        return false;
    }
    let (Ok(pk), Ok(s)) = (hex::decode(&sig.public_key), hex::decode(&sig.signature)) else {
        return false;
    };
    let (Ok(pk), Ok(s)) = (<[u8; 32]>::try_from(pk), <[u8; 64]>::try_from(s)) else {
        return false;
    };
    let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    pk.verify_strict(&digest, &ed25519_dalek::Signature::from_bytes(&s))
        .is_ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Synthetic fixtures in the shape of SEC EDGAR data; see the README there.
    pub(crate) const SUBMISSIONS: &str =
        include_str!("../../tests/fixtures/edgar/submissions-CIK0009990001.json");
    pub(crate) const FORM4_DOE_1: &str =
        include_str!("../../tests/fixtures/edgar/0009990101-26-000011.xml");
    pub(crate) const FORM4_DOE_2: &str =
        include_str!("../../tests/fixtures/edgar/0009990101-26-000027.xml");
    pub(crate) const FORM4_ROE: &str =
        include_str!("../../tests/fixtures/edgar/0009990102-26-000004.xml");
    pub(crate) const FORM4_JOINT: &str =
        include_str!("../../tests/fixtures/edgar/0009990103-26-000009.xml");

    pub(crate) fn fixture_holdings() -> Vec<Form4Holding> {
        let mut all = Vec::new();
        for (xml, acc) in [
            (FORM4_DOE_1, "0009990101-26-000011"),
            (FORM4_DOE_2, "0009990101-26-000027"),
            (FORM4_ROE, "0009990102-26-000004"),
            (FORM4_JOINT, "0009990103-26-000009"),
        ] {
            all.extend(parse_form4(xml, acc).unwrap());
        }
        latest_holdings(all)
    }

    #[test]
    fn submissions_index_lists_form4s_newest_first() {
        let v: Value = serde_json::from_str(SUBMISSIONS).unwrap();
        let filings = form4_filings(&v, 10).unwrap();
        let acc: Vec<&str> = filings
            .iter()
            .map(|f| f.accession_number.as_str())
            .collect();
        assert_eq!(
            acc,
            [
                "0009990101-26-000027",
                "0009990103-26-000009",
                "0009990102-26-000004",
                "0009990101-26-000011"
            ]
        );
        // The rendered-HTML prefix is dropped to fetch the raw XML.
        assert_eq!(filings[0].document, "form4.xml");
        assert_eq!(form4_filings(&v, 2).unwrap().len(), 2);
    }

    #[test]
    fn form4_position_is_the_last_shares_owned_following_transaction() {
        let h = parse_form4(FORM4_DOE_2, "0009990101-26-000027").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].owner_cik, "0009990101");
        assert_eq!(h[0].owner_name, "DOE JANE");
        assert_eq!(
            h[0].shares, 115_000.0,
            "sale after purchase: last position wins"
        );
        assert_eq!(h[0].indirect_shares, 20_000.0);

        // Joint filers share one reported position.
        let joint = parse_form4(FORM4_JOINT, "0009990103-26-000009").unwrap();
        assert_eq!(joint.len(), 2);
        assert!(joint.iter().all(|o| o.shares == 60_000.0));
        assert_eq!(joint[1].owner_name, "SMITH & CO HOLDINGS LLC");

        assert_eq!(
            parse_form4(
                "<ownershipDocument><documentType>3</documentType></ownershipDocument>",
                "x"
            ),
            Err(EdgarError::NotForm4)
        );
    }

    #[test]
    fn latest_filing_per_owner_wins() {
        let h = fixture_holdings();
        let doe = h.iter().find(|h| h.owner_cik == "0009990101").unwrap();
        assert_eq!(doe.accession_number, "0009990101-26-000027");
        assert_eq!(h.len(), 4);
    }

    #[test]
    fn reconciliation_classifies_and_highlights_against_fixtures() {
        let map: HashMap<String, String> = [
            ("9990101", "GDOE"),
            ("0009990102", "GROE"),
            ("0009990103", "GSMITH"),
            // 0009990104 (the joint LLC) is deliberately unmapped.
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let chain = [
            OnChainHolder {
                address: "GDOE",
                tokens: 115_000.0,
            }, // matches
            OnChainHolder {
                address: "GROE",
                tokens: 44_000.0,
            }, // filing 40 000: +10 %
            OnChainHolder {
                address: "GNEW",
                tokens: 12_000.0,
            }, // no filing
        ];
        let report = reconcile(
            1,
            "9990001",
            &chain,
            &fixture_holdings(),
            &map,
            1.0,
            1_000_000.0,
            "2026-11-24T00:00:00Z".into(),
        );
        let row = |a: &str| {
            report
                .rows
                .iter()
                .find(|r| r.address.as_deref() == Some(a))
                .unwrap()
        };

        assert_eq!(row("GDOE").kind, DiffKind::Unchanged);
        assert_eq!(row("GROE").kind, DiffKind::Changed);
        assert!((row("GROE").percent_change - 10.0).abs() < 1e-9);
        assert_eq!(
            row("GROE").highlight,
            Highlight::Yellow,
            "exactly 10 % is not above 10 %"
        );
        assert_eq!(row("GNEW").kind, DiffKind::New);
        assert_eq!(row("GNEW").highlight, Highlight::Red);
        assert!(
            row("GNEW").compliance_alert,
            "12 000 of 1 000 000 is 1.2 % of supply"
        );
        assert_eq!(row("GSMITH").kind, DiffKind::Departed);
        assert_eq!(row("GSMITH").absolute_change, -60_000.0);
        assert!(!row("GROE").compliance_alert, "4 000 is 0.4 % of supply");
        assert_eq!(report.compliance_alerts, 2);
        assert_eq!(
            report.unmapped_owners,
            ["SMITH & CO HOLDINGS LLC (0009990104)"]
        );
        assert_eq!(report.counts["changed"], 1);
    }

    #[test]
    fn highlight_thresholds() {
        assert_eq!(highlight(5.0), Highlight::None);
        assert_eq!(highlight(5.01), Highlight::Yellow);
        assert_eq!(highlight(-10.5), Highlight::Red);
    }

    #[test]
    fn signatures_verify_and_detect_tampering() {
        let signer = ReportSigner::from_seed([3; 32]);
        let report = reconcile(1, "1", &[], &[], &HashMap::new(), 1.0, 1.0, "t".into());
        let sig = signer.sign(&report, "t".into());
        assert!(verify_report(&report, &sig));
        let mut tampered = report.clone();
        tampered.total_supply = 2.0;
        assert!(!verify_report(&tampered, &sig));
        assert_eq!(sig.digest.len(), 64);
        assert_eq!(normalize_cik("320193"), "0000320193");
    }
}
