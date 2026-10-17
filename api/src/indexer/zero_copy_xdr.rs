//! Zero-copy views of `LedgerEntryChange` / `ContractDataEntry` XDR (issue #276).
//!
//! Builds on the borrowed [`Reader`] and [`ScValView`] of `xdr_parser`: the
//! contract id, key and value are slices of the input buffer, so parsing a
//! blob allocates nothing. Malformed input returns an [`XdrError`], and entry
//! kinds this view does not handle return [`XdrError::Unsupported`] so the
//! caller can fall back to the owning `stellar_xdr` decoder.
//!
//! Like `xdr_parser`, this file is included directly by `tests/zero_copy_xdr.rs`.

#![allow(dead_code)]

use super::xdr_parser::{read_address, AddressView, Reader, Result, ScValView, XdrError};

const LEDGER_ENTRY_CONTRACT_DATA: u32 = 6;

/// `ContractDataDurability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Temporary,
    Persistent,
}

/// A borrowed `ContractDataEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractDataEntryView<'a> {
    pub contract: AddressView<'a>,
    pub key: ScValView<'a>,
    pub durability: Durability,
    pub val: ScValView<'a>,
}

fn read_entry<'a>(r: &mut Reader<'a>) -> Result<ContractDataEntryView<'a>> {
    // ExtensionPoint: only v0 (void) exists.
    match r.u32()? {
        0 => {}
        d => return Err(XdrError::InvalidDiscriminant(d)),
    }
    let contract = read_address(r)?;
    let key = ScValView::read(r)?;
    let durability = match r.u32()? {
        0 => Durability::Temporary,
        1 => Durability::Persistent,
        d => return Err(XdrError::InvalidDiscriminant(d)),
    };
    let val = ScValView::read(r)?;
    Ok(ContractDataEntryView {
        contract,
        key,
        durability,
        val,
    })
}

/// Parse a whole buffer holding exactly one `ContractDataEntry`.
pub fn parse_contract_data_entry(buf: &[u8]) -> Result<ContractDataEntryView<'_>> {
    let mut r = Reader::new(buf);
    let entry = read_entry(&mut r)?;
    r.finish()?;
    Ok(entry)
}

/// Parse a whole buffer holding one `LedgerEntryChange` that carries a
/// `ContractDataEntry` (created, updated, state or restored). Removals and
/// other entry types are `Unsupported`.
pub fn parse_contract_data_change(buf: &[u8]) -> Result<ContractDataEntryView<'_>> {
    let mut r = Reader::new(buf);
    match r.u32()? {
        0 | 1 | 3 | 4 => {}
        d => return Err(XdrError::Unsupported(d)),
    }
    let _last_modified_ledger_seq = r.u32()?;
    match r.u32()? {
        LEDGER_ENTRY_CONTRACT_DATA => {}
        d => return Err(XdrError::Unsupported(d)),
    }
    let entry = read_entry(&mut r)?;
    // LedgerEntry.ext: only v0 (void) is handled.
    match r.u32()? {
        0 => {}
        d => return Err(XdrError::Unsupported(d)),
    }
    r.finish()?;
    Ok(entry)
}
