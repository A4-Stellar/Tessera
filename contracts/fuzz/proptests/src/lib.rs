//! Shared strategies and helpers for the property tests.
//!
//! The `i128` strategies deliberately bias hard toward the values that actually
//! break contracts rather than sampling `i128` uniformly. A uniform draw over
//! the full range is almost never near `0`, `1`, `-1`, `i128::MIN` or
//! `i128::MAX`, so it almost never reaches an off-by-one boundary, and the
//! interesting cases are exactly the ones on the boundary. `amount()` below
//! mixes a uniform draw with a boundary draw and lets the caller pick the mix,
//! so a test can run most cases over ordinary values while still spending
//! cases on the pathological ones.

#![allow(dead_code)]

use proptest::prelude::*;

/// Values that have historically broken a contract: the sign boundary, the
/// unit boundary, and both `i128` extremes.
fn boundary_i128() -> impl Strategy<Value = i128> {
    prop_oneof![
        Just(0i128),
        Just(1i128),
        Just(-1i128),
        Just(2i128),
        Just(-2i128),
        Just(i128::MAX),
        Just(i128::MAX - 1),
        Just(i128::MAX / 2),
        Just(i128::MAX / 10_000),
        Just(i128::MIN),
        Just(i128::MIN + 1),
        // Common unit-scaling mistakes, e.g. treating a rate as a fraction.
        Just(10_000i128),
        Just(1_000_000i128),
        Just(1_000_000_000i128),
        Just(u32::MAX as i128),
    ]
}

/// A "plausible amount": mostly small and positive, with a steady tail of the
/// values that trigger overflow paths.
pub fn amount() -> impl Strategy<Value = i128> {
    prop_oneof![
        3 => boundary_i128(),
        2 => any::<i16>().prop_map(i128::from),
        5 => (0i128..1_000_000).any::<i128>(),
    ]
}

/// A rate in basis points, including the values just outside the legal range.
pub fn bps() -> impl Strategy<Value = u32> {
    prop_oneof![
        2 => Just(0u32),
        2 => Just(1u32),
        2 => Just(5_000u32),
        2 => Just(9_999u32),
        2 => Just(10_000u32),
        2 => Just(10_001u32),
        1 => Just(u32::MAX),
        4 => any::<u32>(),
    ]
}

/// A non-negative amount, for entry points that legitimately require one.
pub fn non_negative() -> impl Strategy<Value = i128> {
    amount().prop_filter("non-negative", |v| *v >= 0)
}

/// A strictly positive amount, for the same reason.
pub fn positive() -> impl Strategy<Value = i128> {
    amount().prop_filter("positive", |v| *v > 0)
}

/// A jurisdiction string short enough to stay in `String`'s small-object
/// encoding and to read well in a counterexample.
pub fn jurisdiction() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::from("US")),
        Just(String::from("KY")),
        Just(String::from("SC")),
        Just(String::from("BS")),
        "[A-Z]{2,6}",
    ]
}

/// The index of a holder, for choosing a participant out of a fixed set.
pub fn slot(len: usize) -> impl Strategy<Value = usize> {
    (0..len).any::<usize>()
}

/// An operation to apply in a stateful sequence. Deliberately a simple enum
/// rather than a derived representation: a shrunk counterexample should read
/// as "transfer 5 from 1 to 3", not as a byte blob.
#[derive(Clone, Debug)]
pub enum Op {
    Transfer { from: usize, to: usize, amount: i128 },
    Mint { to: usize, amount: i128 },
    Burn { from: usize, amount: i128 },
    Clawback { from: usize, amount: i128 },
}

impl Op {
    pub fn label(&self) -> String {
        match self {
            Op::Transfer { from, to, amount } => {
                format!("transfer {amount} from holder {from} to holder {to}")
            }
            Op::Mint { to, amount } => format!("mint {amount} to holder {to}"),
            Op::Burn { from, amount } => format!("burn {amount} from holder {from}"),
            Op::Clawback { from, amount } => format!("clawback {amount} from holder {from}"),
        }
    }
}

/// A short sequence of operations over `holders` participants.
pub fn op_sequence(holders: usize, max_ops: usize) -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            4 => (slot(holders), slot(holders), amount())
                .prop_map(|(from, to, amount)| Op::Transfer { from, to, amount }),
            2 => (slot(holders), amount()).prop_map(|(to, amount)| Op::Mint { to, amount }),
            2 => (slot(holders), amount()).prop_map(|(from, amount)| Op::Burn { from, amount }),
            1 => (slot(holders), amount()).prop_map(|(from, amount)| Op::Clawback { from, amount }),
        ],
        0..=max_ops,
    )
}
