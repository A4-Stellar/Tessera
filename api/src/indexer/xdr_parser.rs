//! Zero-copy XDR reader for Soroban contract events (issue #102).
//!
//! `stellar_xdr`'s `ReadXdr` builds a fully owned tree: every `ScVal::Symbol`,
//! `Bytes`, `Vec`, `Map` and topic list is a heap allocation. For the indexer's
//! hot path (walking thousands of contract events per ledger) most of that work
//! is thrown away immediately after a few fields are read.
//!
//! This module instead reads the wire bytes *in place*. Parsing returns views
//! that borrow from the input buffer:
//!
//! * `Bytes` / `String` / `Symbol` are `&'a [u8]` slices of the input.
//! * Contract IDs and account keys are `&'a [u8; 32]` slices.
//! * `Vec` / `Map` values are lazy: they carry the element count and the body
//!   slice and decode elements only when iterated.
//!
//! Parsing a [`ContractEventView`] and iterating its topics/data performs **no
//! heap allocation** (verified by `api/tests/xdr_parser_allocs.rs`, which
//! counts allocator calls). Malformed input is rejected with an [`XdrError`]:
//! truncated buffers, non-zero padding, invalid booleans/discriminants,
//! oversized lengths and excessive nesting - never a panic.
//!
//! Only the shapes needed by the indexer are decoded (`ContractEvent` with a
//! v0 body and the common `ScVal` variants). Unknown `ScVal` variants return
//! [`XdrError::Unsupported`] so callers can fall back to the allocating
//! `stellar_xdr` decoder.
//!
//! This file is self-contained (no `crate::` imports) so the criterion bench
//! and the allocation test can include it directly.

#![allow(dead_code)]

use std::fmt;

/// Maximum `ScVal` nesting accepted (matches stellar-xdr's default depth
/// limit order of magnitude; protects against stack exhaustion).
pub const MAX_DEPTH: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdrError {
    /// The buffer ended before the value did.
    UnexpectedEof,
    /// Bytes remained after the top-level value.
    TrailingBytes,
    /// Opaque/string padding bytes were not zero.
    NonZeroPadding,
    /// A discriminant or boolean had a value the schema does not allow.
    InvalidDiscriminant(u32),
    /// A length prefix exceeded the remaining buffer.
    LengthOutOfBounds,
    /// `ScVal` nesting exceeded [`MAX_DEPTH`].
    TooDeep,
    /// A valid-but-unhandled variant; fall back to the owning decoder.
    Unsupported(u32),
}

impl fmt::Display for XdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XdrError::UnexpectedEof => write!(f, "unexpected end of XDR input"),
            XdrError::TrailingBytes => write!(f, "trailing bytes after XDR value"),
            XdrError::NonZeroPadding => write!(f, "non-zero XDR padding"),
            XdrError::InvalidDiscriminant(d) => write!(f, "invalid XDR discriminant {d}"),
            XdrError::LengthOutOfBounds => write!(f, "XDR length exceeds input"),
            XdrError::TooDeep => write!(f, "XDR nesting too deep"),
            XdrError::Unsupported(d) => write!(f, "unsupported XDR variant {d}"),
        }
    }
}

impl std::error::Error for XdrError {}

pub type Result<T> = std::result::Result<T, XdrError>;

/// Cursor over a borrowed byte slice; every read returns data tied to `'a`.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(XdrError::UnexpectedEof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<&'a [u8; N]> {
        // `take` guarantees the slice length, so the conversion cannot fail.
        self.take(N)?.try_into().map_err(|_| XdrError::UnexpectedEof)
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(*self.array::<4>()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(*self.array::<4>()?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(*self.array::<8>()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(*self.array::<8>()?))
    }

    pub fn bool(&mut self) -> Result<bool> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            d => Err(XdrError::InvalidDiscriminant(d)),
        }
    }

    /// Variable-length opaque/string: length prefix, bytes, zero padding to a
    /// multiple of four. Returns a slice of the input (no copy).
    pub fn opaque(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        if len > self.remaining() {
            return Err(XdrError::LengthOutOfBounds);
        }
        let data = self.take(len)?;
        let pad = (4 - len % 4) % 4;
        if self.take(pad)?.iter().any(|b| *b != 0) {
            return Err(XdrError::NonZeroPadding);
        }
        Ok(data)
    }

    pub fn finish(&self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(XdrError::TrailingBytes)
        }
    }
}

/// A Soroban address, borrowed from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressView<'a> {
    /// ed25519 account public key (`G...` strkey).
    Account(&'a [u8; 32]),
    /// Contract hash (`C...` strkey).
    Contract(&'a [u8; 32]),
    /// Muxed account / claimable balance / liquidity pool: discriminant plus
    /// the raw payload bytes, left undecoded.
    Other(u32, &'a [u8]),
}

/// A lazily decoded `ScVec` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqView<'a> {
    count: u32,
    body: &'a [u8],
    depth: u8,
}

/// A lazily decoded `ScMap` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapView<'a> {
    count: u32,
    body: &'a [u8],
    depth: u8,
}

/// A borrowed `ScVal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScValView<'a> {
    Bool(bool),
    Void,
    /// `ScError`: error type discriminant and code.
    Error(u32, u32),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    Timepoint(u64),
    Duration(u64),
    U128(u128),
    I128(i128),
    U256(&'a [u8; 32]),
    I256(&'a [u8; 32]),
    Bytes(&'a [u8]),
    String(&'a [u8]),
    Symbol(&'a [u8]),
    Vec(Option<SeqView<'a>>),
    Map(Option<MapView<'a>>),
    Address(AddressView<'a>),
    LedgerKeyNonce(i64),
}

impl<'a> ScValView<'a> {
    /// Decode one `ScVal` from the reader. Vec/Map bodies are validated
    /// (structure, lengths, padding) but not materialised.
    pub fn read(r: &mut Reader<'a>) -> Result<Self> {
        Self::read_at(r, 0)
    }

    /// Decode a whole buffer holding exactly one `ScVal`.
    pub fn from_xdr(buf: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        let v = Self::read(&mut r)?;
        r.finish()?;
        Ok(v)
    }

    fn read_at(r: &mut Reader<'a>, depth: u8) -> Result<Self> {
        if depth > MAX_DEPTH {
            return Err(XdrError::TooDeep);
        }
        let disc = r.u32()?;
        Ok(match disc {
            0 => ScValView::Bool(r.bool()?),
            1 => ScValView::Void,
            2 => {
                let kind = r.u32()?;
                // Every ScErrorType carries a u32 code except `Contract`,
                // which carries a u32 contract error code - same width.
                ScValView::Error(kind, r.u32()?)
            }
            3 => ScValView::U32(r.u32()?),
            4 => ScValView::I32(r.i32()?),
            5 => ScValView::U64(r.u64()?),
            6 => ScValView::I64(r.i64()?),
            7 => ScValView::Timepoint(r.u64()?),
            8 => ScValView::Duration(r.u64()?),
            9 => {
                let hi = r.u64()? as u128;
                ScValView::U128((hi << 64) | r.u64()? as u128)
            }
            10 => {
                let hi = r.i64()? as i128;
                ScValView::I128((hi << 64) | r.u64()? as i128)
            }
            11 => ScValView::U256(r.array::<32>()?),
            12 => ScValView::I256(r.array::<32>()?),
            13 => ScValView::Bytes(r.opaque()?),
            14 => ScValView::String(r.opaque()?),
            15 => {
                let s = r.opaque()?;
                if s.len() > 32 {
                    return Err(XdrError::LengthOutOfBounds);
                }
                ScValView::Symbol(s)
            }
            16 => {
                if !r.bool()? {
                    ScValView::Vec(None)
                } else {
                    let count = r.u32()?;
                    let start = r.pos;
                    for _ in 0..count {
                        skip_scval(r, depth + 1)?;
                    }
                    ScValView::Vec(Some(SeqView {
                        count,
                        body: &r.buf[start..r.pos],
                        depth: depth + 1,
                    }))
                }
            }
            17 => {
                if !r.bool()? {
                    ScValView::Map(None)
                } else {
                    let count = r.u32()?;
                    let start = r.pos;
                    for _ in 0..count {
                        skip_scval(r, depth + 1)?;
                        skip_scval(r, depth + 1)?;
                    }
                    ScValView::Map(Some(MapView {
                        count,
                        body: &r.buf[start..r.pos],
                        depth: depth + 1,
                    }))
                }
            }
            18 => ScValView::Address(read_address(r)?),
            21 => ScValView::LedgerKeyNonce(r.i64()?),
            // 19 ContractInstance, 20 LedgerKeyContractInstance and any
            // future variant: hand back to the owning decoder.
            d => return Err(XdrError::Unsupported(d)),
        })
    }
}

fn read_address<'a>(r: &mut Reader<'a>) -> Result<AddressView<'a>> {
    match r.u32()? {
        0 => match r.u32()? {
            0 => Ok(AddressView::Account(r.array::<32>()?)),
            d => Err(XdrError::InvalidDiscriminant(d)),
        },
        1 => Ok(AddressView::Contract(r.array::<32>()?)),
        d @ (2 | 3 | 4) => {
            let start = r.pos;
            match d {
                2 => {
                    r.u64()?;
                    r.array::<32>()?;
                }
                3 => {
                    r.u32()?;
                    r.array::<32>()?;
                }
                _ => {
                    r.array::<32>()?;
                }
            }
            Ok(AddressView::Other(d, &r.buf[start..r.pos]))
        }
        d => Err(XdrError::InvalidDiscriminant(d)),
    }
}

/// Validate and skip one `ScVal` without building a view.
fn skip_scval(r: &mut Reader<'_>, depth: u8) -> Result<()> {
    ScValView::read_at(r, depth).map(|_| ())
}

impl<'a> SeqView<'a> {
    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn iter(&self) -> SeqIter<'a> {
        SeqIter {
            r: Reader::new(self.body),
            left: self.count,
            depth: self.depth,
        }
    }
}

impl<'a> MapView<'a> {
    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn iter(&self) -> MapIter<'a> {
        MapIter {
            r: Reader::new(self.body),
            left: self.count,
            depth: self.depth,
        }
    }
}

pub struct SeqIter<'a> {
    r: Reader<'a>,
    left: u32,
    depth: u8,
}

impl<'a> Iterator for SeqIter<'a> {
    type Item = Result<ScValView<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        Some(ScValView::read_at(&mut self.r, self.depth))
    }
}

pub struct MapIter<'a> {
    r: Reader<'a>,
    left: u32,
    depth: u8,
}

impl<'a> Iterator for MapIter<'a> {
    type Item = Result<(ScValView<'a>, ScValView<'a>)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let k = ScValView::read_at(&mut self.r, self.depth);
        let v = ScValView::read_at(&mut self.r, self.depth);
        Some(k.and_then(|k| v.map(|v| (k, v))))
    }
}

/// `ContractEventType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    System,
    Contract,
    Diagnostic,
}

/// A borrowed `ContractEvent` (v0 body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractEventView<'a> {
    pub contract_id: Option<&'a [u8; 32]>,
    pub kind: EventKind,
    pub topics: SeqView<'a>,
    pub data: ScValView<'a>,
}

/// Parse a `ContractEvent` directly from `buf` without allocating.
pub fn parse_contract_event(buf: &[u8]) -> Result<ContractEventView<'_>> {
    let mut r = Reader::new(buf);
    // ExtensionPoint: only v0 (void) exists.
    match r.u32()? {
        0 => {}
        d => return Err(XdrError::InvalidDiscriminant(d)),
    }
    let contract_id = if r.bool()? { Some(r.array::<32>()?) } else { None };
    let kind = match r.u32()? {
        0 => EventKind::System,
        1 => EventKind::Contract,
        2 => EventKind::Diagnostic,
        d => return Err(XdrError::InvalidDiscriminant(d)),
    };
    // ContractEventBody: union switch (int v) { case 0: ContractEventV0 }
    match r.u32()? {
        0 => {}
        d => return Err(XdrError::InvalidDiscriminant(d)),
    }
    let count = r.u32()?;
    let start = r.pos;
    for _ in 0..count {
        skip_scval(&mut r, 1)?;
    }
    let topics = SeqView {
        count,
        body: &buf[start..r.pos],
        depth: 1,
    };
    let data = ScValView::read_at(&mut r, 0)?;
    r.finish()?;
    Ok(ContractEventView {
        contract_id,
        kind,
        topics,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_xdr::curr as xdr;
    use stellar_xdr::curr::{Limits, WriteXdr};

    fn sym(s: &str) -> xdr::ScVal {
        xdr::ScVal::Symbol(xdr::ScSymbol(s.try_into().unwrap()))
    }

    fn sample_event() -> xdr::ContractEvent {
        let topics: Vec<xdr::ScVal> = vec![
            sym("transfer"),
            xdr::ScVal::Address(xdr::ScAddress::Account(xdr::AccountId(
                xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256([7; 32])),
            ))),
            xdr::ScVal::Address(xdr::ScAddress::Contract(xdr::ContractId(xdr::Hash([9; 32])))),
        ];
        let data = xdr::ScVal::Map(Some(xdr::ScMap(
            vec![
                xdr::ScMapEntry {
                    key: sym("amount"),
                    val: xdr::ScVal::I128(xdr::Int128Parts {
                        hi: -1,
                        lo: 12345,
                    }),
                },
                xdr::ScMapEntry {
                    key: sym("memo"),
                    val: xdr::ScVal::String(xdr::ScString("hello".try_into().unwrap())),
                },
                xdr::ScMapEntry {
                    key: sym("blob"),
                    val: xdr::ScVal::Bytes(xdr::ScBytes(vec![1, 2, 3].try_into().unwrap())),
                },
                xdr::ScMapEntry {
                    key: sym("list"),
                    val: xdr::ScVal::Vec(Some(xdr::ScVec(
                        vec![xdr::ScVal::U32(1), xdr::ScVal::Bool(true), xdr::ScVal::Void]
                            .try_into()
                            .unwrap(),
                    ))),
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
    }

    #[test]
    fn matches_stellar_xdr_on_a_realistic_event() {
        let ev = sample_event();
        let bytes = ev.to_xdr(Limits::none()).unwrap();
        let view = parse_contract_event(&bytes).unwrap();

        assert_eq!(view.contract_id, Some(&[3u8; 32]));
        assert_eq!(view.kind, EventKind::Contract);
        assert_eq!(view.topics.len(), 3);
        let topics: Vec<_> = view.topics.iter().collect::<Result<_>>().unwrap();
        assert_eq!(topics[0], ScValView::Symbol(b"transfer"));
        assert_eq!(topics[1], ScValView::Address(AddressView::Account(&[7; 32])));
        assert_eq!(topics[2], ScValView::Address(AddressView::Contract(&[9; 32])));

        let ScValView::Map(Some(map)) = view.data else {
            panic!("data should be a map")
        };
        let entries: Vec<_> = map.iter().collect::<Result<_>>().unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].0, ScValView::Symbol(b"amount"));
        assert_eq!(entries[0].1, ScValView::I128(-(1i128 << 64) + 12345));
        assert_eq!(entries[1].1, ScValView::String(b"hello"));
        assert_eq!(entries[2].1, ScValView::Bytes(&[1, 2, 3]));
        let ScValView::Vec(Some(list)) = entries[3].1 else {
            panic!("list")
        };
        let items: Vec<_> = list.iter().collect::<Result<_>>().unwrap();
        assert_eq!(
            items,
            vec![ScValView::U32(1), ScValView::Bool(true), ScValView::Void]
        );
    }

    #[test]
    fn views_borrow_from_the_input_buffer() {
        let bytes = sample_event().to_xdr(Limits::none()).unwrap();
        let view = parse_contract_event(&bytes).unwrap();
        let ScValView::Symbol(s) = view.topics.iter().next().unwrap().unwrap() else {
            panic!()
        };
        let range = bytes.as_ptr_range();
        assert!(range.contains(&s.as_ptr()), "symbol must point into input");
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let bytes = sample_event().to_xdr(Limits::none()).unwrap();
        for n in 0..bytes.len() {
            assert!(parse_contract_event(&bytes[..n]).is_err(), "prefix {n}");
        }
    }

    #[test]
    fn rejects_trailing_bytes_bad_padding_and_bad_discriminants() {
        let mut bytes = sample_event().to_xdr(Limits::none()).unwrap();
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(parse_contract_event(&trailing), Err(XdrError::TrailingBytes));

        // Symbol "transfer" is 8 bytes (no pad); use a 3-byte string instead.
        let s = xdr::ScVal::String(xdr::ScString("abc".try_into().unwrap()));
        let mut sb = s.to_xdr(Limits::none()).unwrap();
        assert_eq!(ScValView::from_xdr(&sb), Ok(ScValView::String(b"abc")));
        *sb.last_mut().unwrap() = 1;
        assert_eq!(ScValView::from_xdr(&sb), Err(XdrError::NonZeroPadding));

        bytes[3] = 9; // ExtensionPoint discriminant
        assert_eq!(
            parse_contract_event(&bytes),
            Err(XdrError::InvalidDiscriminant(9))
        );
        assert_eq!(
            ScValView::from_xdr(&[0, 0, 0, 0, 0, 0, 0, 2]),
            Err(XdrError::InvalidDiscriminant(2))
        );
    }

    #[test]
    fn huge_length_prefix_does_not_allocate_or_panic() {
        // Bytes with a 4 GiB length claim and no body.
        let buf = [0, 0, 0, 13, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(ScValView::from_xdr(&buf), Err(XdrError::LengthOutOfBounds));
        // Vec claiming u32::MAX elements with no body fails on the first read.
        let buf = [0, 0, 0, 16, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(ScValView::from_xdr(&buf), Err(XdrError::UnexpectedEof));
    }

    #[test]
    fn nesting_is_bounded() {
        // Vec(Some([Vec(Some([ ... ]))])) deeper than MAX_DEPTH.
        let mut buf = Vec::new();
        for _ in 0..(MAX_DEPTH as usize + 5) {
            buf.extend_from_slice(&[0, 0, 0, 16, 0, 0, 0, 1, 0, 0, 0, 1]);
        }
        buf.extend_from_slice(&[0, 0, 0, 1]);
        assert_eq!(ScValView::from_xdr(&buf), Err(XdrError::TooDeep));
    }

    #[test]
    fn unsupported_variants_are_reported() {
        assert_eq!(
            ScValView::from_xdr(&[0, 0, 0, 20]),
            Err(XdrError::Unsupported(20))
        );
    }

    #[test]
    fn scalars_round_trip_against_stellar_xdr() {
        let cases = vec![
            xdr::ScVal::Bool(false),
            xdr::ScVal::U32(u32::MAX),
            xdr::ScVal::I32(-5),
            xdr::ScVal::U64(u64::MAX),
            xdr::ScVal::I64(i64::MIN),
            xdr::ScVal::U128(xdr::UInt128Parts { hi: 1, lo: 2 }),
            xdr::ScVal::Timepoint(xdr::TimePoint(77)),
            xdr::ScVal::Duration(xdr::Duration(88)),
        ];
        let expect = [
            ScValView::Bool(false),
            ScValView::U32(u32::MAX),
            ScValView::I32(-5),
            ScValView::U64(u64::MAX),
            ScValView::I64(i64::MIN),
            ScValView::U128((1u128 << 64) | 2),
            ScValView::Timepoint(77),
            ScValView::Duration(88),
        ];
        for (c, e) in cases.iter().zip(expect) {
            let b = c.to_xdr(Limits::none()).unwrap();
            assert_eq!(ScValView::from_xdr(&b), Ok(e));
        }
    }
}
