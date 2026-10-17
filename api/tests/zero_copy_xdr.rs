//! Issue #276: zero-copy `ContractDataEntry` parsing allocates nothing and
//! rejects malformed input without panicking. The modules are self-contained
//! so they are included directly (the API is a binary crate).

#[path = "../src/indexer/xdr_parser.rs"]
mod xdr_parser;
#[path = "../src/indexer/zero_copy_xdr.rs"]
mod zero_copy_xdr;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, WriteXdr};
use xdr_parser::{AddressView, ScValView};
use zero_copy_xdr::{parse_contract_data_change, parse_contract_data_entry, Durability};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn entry() -> xdr::ContractDataEntry {
    xdr::ContractDataEntry {
        ext: xdr::ExtensionPoint::V0,
        contract: xdr::ScAddress::Contract(xdr::ContractId(xdr::Hash([9; 32]))),
        key: xdr::ScVal::Symbol(xdr::ScSymbol("Balance".try_into().unwrap())),
        durability: xdr::ContractDataDurability::Persistent,
        val: xdr::ScVal::String(xdr::ScString("hello".try_into().unwrap())),
    }
}

fn entry_bytes() -> Vec<u8> {
    entry().to_xdr(Limits::none()).unwrap()
}

fn change_bytes() -> Vec<u8> {
    xdr::LedgerEntryChange::Updated(xdr::LedgerEntry {
        last_modified_ledger_seq: 7,
        data: xdr::LedgerEntryData::ContractData(entry()),
        ext: xdr::LedgerEntryExt::V0,
    })
    .to_xdr(Limits::none())
    .unwrap()
}

#[test]
fn parses_entry_and_change_without_allocating() {
    let (entry, change) = (entry_bytes(), change_bytes());
    let before = ALLOCS.with(|c| c.get());

    let e = parse_contract_data_entry(&entry).unwrap();
    let c = parse_contract_data_change(&change).unwrap();

    assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
    assert_eq!(e, c);
    assert_eq!(e.contract, AddressView::Contract(&[9; 32]));
    assert_eq!(e.key, ScValView::Symbol(b"Balance"));
    assert_eq!(e.durability, Durability::Persistent);
    assert_eq!(e.val, ScValView::String(b"hello"));
}

#[test]
fn removed_changes_are_unsupported() {
    let removed = xdr::LedgerEntryChange::Removed(xdr::LedgerKey::ContractData(
        xdr::LedgerKeyContractData {
            contract: xdr::ScAddress::Contract(xdr::ContractId(xdr::Hash([9; 32]))),
            key: xdr::ScVal::Void,
            durability: xdr::ContractDataDurability::Persistent,
        },
    ))
    .to_xdr(Limits::none())
    .unwrap();
    assert!(parse_contract_data_change(&removed).is_err());
}

/// Deterministic fuzz: every truncation and many byte-mutated copies of valid
/// blobs must return `Ok`/`Err`, never panic or read out of bounds.
#[test]
fn malformed_input_never_panics() {
    for valid in [entry_bytes(), change_bytes()] {
        for len in 0..valid.len() {
            assert!(parse_contract_data_entry(&valid[..len]).is_err());
            assert!(parse_contract_data_change(&valid[..len]).is_err());
        }
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..20_000 {
            let mut blob = valid.clone();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let i = (seed as usize) % blob.len();
                blob[i] = (seed >> 32) as u8;
            }
            let _ = parse_contract_data_entry(&blob);
            let _ = parse_contract_data_change(&blob);
        }
    }
}
