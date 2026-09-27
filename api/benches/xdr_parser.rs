//! Issue #102 benchmark: zero-copy `xdr_parser` vs the standard owning
//! `stellar_xdr` decoder, doing the *same* work per event (contract id, topic
//! symbols, map keys and the i128 amount).
//!
//! Run with `cargo bench --bench xdr_parser` from `api/`.

#[path = "../src/indexer/xdr_parser.rs"]
#[allow(dead_code)]
mod xdr_parser;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use stellar_xdr::curr as xdr;
use stellar_xdr::curr::{Limits, ReadXdr, WriteXdr};
use xdr_parser::{parse_contract_event, ScValView};

fn events() -> Vec<Vec<u8>> {
    let sym = |s: &str| xdr::ScVal::Symbol(xdr::ScSymbol(s.try_into().unwrap()));
    let acct = |b: u8| {
        xdr::ScVal::Address(xdr::ScAddress::Account(xdr::AccountId(
            xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256([b; 32])),
        )))
    };
    (0..256u32)
        .map(|i| {
            let topics: Vec<xdr::ScVal> = vec![sym("transfer"), acct(i as u8), acct(!(i as u8))];
            let data = xdr::ScVal::Map(Some(xdr::ScMap(
                vec![
                    xdr::ScMapEntry {
                        key: sym("amount"),
                        val: xdr::ScVal::I128(xdr::Int128Parts { hi: 0, lo: 1000 + i as u64 }),
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
                contract_id: Some(xdr::ContractId(xdr::Hash([i as u8; 32]))),
                type_: xdr::ContractEventType::Contract,
                body: xdr::ContractEventBody::V0(xdr::ContractEventV0 {
                    topics: topics.try_into().unwrap(),
                    data,
                }),
            }
            .to_xdr(Limits::none())
            .unwrap()
        })
        .collect()
}

fn standard(bytes: &[u8]) -> u64 {
    let ev = xdr::ContractEvent::from_xdr(bytes, Limits::none()).unwrap();
    let mut sum = ev.contract_id.map_or(0, |c| c.0 .0[0] as u64);
    let xdr::ContractEventBody::V0(b) = ev.body;
    for t in b.topics.iter() {
        if let xdr::ScVal::Symbol(s) = t {
            sum += s.0.len() as u64;
        }
    }
    if let xdr::ScVal::Map(Some(m)) = &b.data {
        for e in m.iter() {
            if let (xdr::ScVal::Symbol(k), xdr::ScVal::I128(a)) = (&e.key, &e.val) {
                sum += k.0.len() as u64 + a.lo;
            }
        }
    }
    sum
}

fn zero_copy(bytes: &[u8]) -> u64 {
    let ev = parse_contract_event(bytes).unwrap();
    let mut sum = ev.contract_id.map_or(0, |c| c[0] as u64);
    for t in ev.topics.iter() {
        if let ScValView::Symbol(s) = t.unwrap() {
            sum += s.len() as u64;
        }
    }
    if let ScValView::Map(Some(m)) = ev.data {
        for kv in m.iter() {
            if let (ScValView::Symbol(k), ScValView::I128(a)) = kv.unwrap() {
                sum += k.len() as u64 + a as u64;
            }
        }
    }
    sum
}

fn bench(c: &mut Criterion) {
    let evs = events();
    // Both implementations must agree before we compare speed.
    for e in &evs {
        assert_eq!(standard(e), zero_copy(e));
    }
    let bytes: u64 = evs.iter().map(|e| e.len() as u64).sum();
    let mut g = c.benchmark_group("xdr_contract_event");
    g.throughput(Throughput::Bytes(bytes));
    g.bench_function("stellar_xdr_owning", |b| {
        b.iter(|| evs.iter().map(|e| standard(black_box(e))).sum::<u64>())
    });
    g.bench_function("zero_copy", |b| {
        b.iter(|| evs.iter().map(|e| zero_copy(black_box(e))).sum::<u64>())
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
