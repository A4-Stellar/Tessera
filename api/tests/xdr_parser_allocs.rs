//! Issue #102: proves the zero-copy parser performs no heap allocation, and
//! quantifies the allocations of the standard `stellar_xdr` decoder for the
//! same event. The parser module is self-contained so it is included directly
//! (the API is a binary crate with no library target).

#[path = "../src/indexer/xdr_parser.rs"]
#[allow(dead_code)]
mod xdr_parser;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr, WriteXdr};
use xdr_parser::{parse_contract_event, ScValView};

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

fn allocs<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = ALLOCS.with(|c| c.get());
    let r = f();
    (r, ALLOCS.with(|c| c.get()) - before)
}

fn sample() -> Vec<u8> {
    let sym = |s: &str| xdr::ScVal::Symbol(xdr::ScSymbol(s.try_into().unwrap()));
    let acct = |b: u8| {
        xdr::ScVal::Address(xdr::ScAddress::Account(xdr::AccountId(
            xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256([b; 32])),
        )))
    };
    let topics: Vec<xdr::ScVal> = vec![sym("transfer"), acct(1), acct(2)];
    let data = xdr::ScVal::Map(Some(xdr::ScMap(
        vec![
            xdr::ScMapEntry {
                key: sym("amount"),
                val: xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 1000 }),
            },
            xdr::ScMapEntry {
                key: sym("memo"),
                val: xdr::ScVal::String(xdr::ScString("invoice 42".try_into().unwrap())),
            },
        ]
        .try_into()
        .unwrap(),
    )));
    xdr::ContractEvent {
        ext: xdr::ExtensionPoint::V0,
        contract_id: Some(xdr::ContractId(xdr::Hash([3; 32]))),
        type_: xdr::ContractEventType::Contract,
        body: xdr::ContractEventBody::V0(xdr::ContractEventV0 {
            topics: topics.try_into().unwrap(),
            data,
        }),
    }
    .to_xdr(Limits::none())
    .unwrap()
}

#[test]
fn zero_copy_parse_and_walk_allocates_nothing() {
    let bytes = sample();
    // Warm up thread-locals so first-use initialisation is not counted.
    let _ = allocs(|| 0);

    let (checksum, n) = allocs(|| {
        let ev = parse_contract_event(&bytes).unwrap();
        let mut sum = ev.contract_id.map_or(0, |c| c[0] as usize);
        for t in ev.topics.iter() {
            if let ScValView::Symbol(s) = t.unwrap() {
                sum += s.len();
            }
        }
        if let ScValView::Map(Some(m)) = ev.data {
            for kv in m.iter() {
                let (k, v) = kv.unwrap();
                if let (ScValView::Symbol(k), ScValView::I128(a)) = (k, v) {
                    sum += k.len() + a as usize;
                }
            }
        }
        sum
    });
    assert!(checksum > 1000);
    assert_eq!(n, 0, "zero-copy path must not touch the heap");
}

#[test]
fn standard_decoder_allocates_for_the_same_event() {
    let bytes = sample();
    let (_ev, n) = allocs(|| xdr::ContractEvent::from_xdr(&bytes, Limits::none()).unwrap());
    // Topic Vec, each symbol/string, the map, its entries...: many allocations.
    assert!(n >= 8, "expected the owning decoder to allocate, got {n}");
    eprintln!("stellar_xdr allocations for one event: {n}; zero-copy: 0");
}
