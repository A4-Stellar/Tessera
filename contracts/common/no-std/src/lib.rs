#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Zero-cost, `no_std`, wasm-bindgen-free helpers shared by Tessera contracts.
//!
//! This crate exists so that arithmetic and bit-packing logic can be reused by
//! Soroban contracts **without** pulling in `std`, JS glue (`wasm-bindgen`), or
//! any third-party dependency. Everything here is built from `core` primitives
//! and compiles to a small amount of straightforward WASM.
//!
//! Design goals
//! ------------
//! * **Correctness first.** Money math must be exact. [`checked_mul_div`]
//!   computes `value * mul / div` with a full 256-bit intermediate, so it never
//!   loses precision to an intermediate `u128` overflow and returns `None`
//!   rather than wrapping.
//! * **No hidden allocation.** No `Vec`, no `String`, no panicking arithmetic on
//!   attacker-controlled input (all public entry points that can fail return
//!   `Option`).
//! * **Zero-cost abstractions.** Public helpers are `#[inline]`, and the ones
//!   that can be `const` are `const`, so they fold into constants or a handful
//!   of instructions under the workspace's `opt-level = "z"` / LTO profile.
//!
//! # Examples
//!
//! ```
//! use tessera_no_std::{bps, checked_mul_div, pack_u16_pair, unpack_u16_pair};
//!
//! // Exact 256-bit intermediate: no precision loss for large inputs.
//! assert_eq!(checked_mul_div(u128::MAX, u128::MAX, u128::MAX), Some(u128::MAX));
//! assert_eq!(checked_mul_div(10, 3, 4), Some(7)); // floor(30 / 4)
//!
//! // Basis points with round-half-away-from-zero.
//! assert_eq!(bps(10_000, 2_500), Some(2_500)); // 25% of 10_000
//! assert_eq!(bps(1, 5_000), Some(1));          // 0.5 rounds away from zero
//!
//! // Pack two u16 ids into a single storage-friendly u32 key.
//! assert_eq!(unpack_u16_pair(pack_u16_pair(7, 9)), (7, 9));
//! ```

use core::cmp::Ordering;

// ---------------------------------------------------------------------------
// Private 256-bit arithmetic primitives
// ---------------------------------------------------------------------------

/// Add a `u64` at limb `index` of a little-endian `[u64; 4]`, propagating carry.
#[inline]
fn add_limb(limbs: &mut [u64; 4], index: usize, value: u64) {
    let mut carry = value;
    let mut i = index;
    while carry != 0 && i < 4 {
        let (sum, overflow) = limbs[i].overflowing_add(carry);
        limbs[i] = sum;
        carry = u64::from(overflow);
        i += 1;
    }
}

/// Exact 128x128 -> 256-bit multiplication, returned as little-endian limbs.
///
/// Each 64x64 partial product fits in a `u128`, so this is entirely safe Rust.
#[inline]
fn mul_u128_u128(a: u128, b: u128) -> [u64; 4] {
    let a_lo = a as u64;
    let a_hi = (a >> 64) as u64;
    let b_lo = b as u64;
    let b_hi = (b >> 64) as u64;

    let p00 = u128::from(a_lo) * u128::from(b_lo);
    let p01 = u128::from(a_lo) * u128::from(b_hi);
    let p10 = u128::from(a_hi) * u128::from(b_lo);
    let p11 = u128::from(a_hi) * u128::from(b_hi);

    let mut limbs = [0u64; 4];
    add_limb(&mut limbs, 0, p00 as u64);
    add_limb(&mut limbs, 1, (p00 >> 64) as u64);
    add_limb(&mut limbs, 1, p01 as u64);
    add_limb(&mut limbs, 2, (p01 >> 64) as u64);
    add_limb(&mut limbs, 1, p10 as u64);
    add_limb(&mut limbs, 2, (p10 >> 64) as u64);
    add_limb(&mut limbs, 2, p11 as u64);
    add_limb(&mut limbs, 3, (p11 >> 64) as u64);
    limbs
}

/// Shift a 256-bit remainder left by one bit, injecting `bit` into the LSB.
#[inline]
fn shl1(rem: &mut [u64; 4], bit: u64) {
    let mut carry = bit;
    for limb in rem.iter_mut() {
        let next = *limb >> 63;
        *limb = (*limb << 1) | carry;
        carry = next;
    }
}

/// `true` when the 256-bit `rem` is greater than or equal to the 128-bit divisor.
///
/// Any nonzero limb above the divisor's 64..127 bit range already implies
/// `rem >= 2^128 > divisor`.
#[inline]
fn cmp_ge_u128(rem: &[u64; 4], divisor: u128) -> bool {
    if rem[3] != 0 || rem[2] != 0 {
        return true;
    }
    let divisor_hi = (divisor >> 64) as u64;
    let divisor_lo = divisor as u64;
    match rem[1].cmp(&divisor_hi) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => rem[0] >= divisor_lo,
    }
}

/// Subtract a 128-bit divisor from a 256-bit remainder known to be >= divisor.
#[inline]
fn sub_u128(rem: &mut [u64; 4], divisor: u128) {
    let divisor_lo = divisor as u64;
    let divisor_hi = (divisor >> 64) as u64;

    let (r0, borrow0) = rem[0].overflowing_sub(divisor_lo);
    rem[0] = r0;

    let (mid, borrow1) = rem[1].overflowing_sub(divisor_hi);
    let (r1, borrow2) = mid.overflowing_sub(u64::from(borrow0));
    rem[1] = r1;

    if borrow1 || borrow2 {
        let (r2, borrow3) = rem[2].overflowing_sub(1);
        rem[2] = r2;
        if borrow3 {
            // The caller guarantees rem >= divisor, so limb 3 cannot underflow.
            rem[3] = rem[3].wrapping_sub(1);
        }
    }
}

/// Divide a 256-bit product by a 128-bit divisor using binary long division.
///
/// Returns `None` on `divisor == 0`, on a quotient that does not fit in `u128`,
/// or when rounding carries the quotient past `u128::MAX`.
fn div_round_256_by_u128(product: &[u64; 4], divisor: u128, round_up: bool) -> Option<u128> {
    if divisor == 0 {
        return None;
    }

    let mut rem = [0u64; 4];
    let mut quotient = 0u128;

    // MSB -> LSB: rem = rem * 2 + next_bit; subtract divisor when possible.
    for bit_index in (0..256).rev() {
        let bit = (product[bit_index / 64] >> (bit_index % 64)) & 1;
        shl1(&mut rem, bit);
        if cmp_ge_u128(&rem, divisor) {
            sub_u128(&mut rem, divisor);
            if bit_index >= 128 {
                // Setting this quotient bit means quotient >= 2^128.
                return None;
            }
            quotient |= 1u128 << bit_index;
        }
    }

    if round_up {
        // Round half away from zero: add one when 2 * remainder >= divisor.
        // `rem < divisor <= u128::MAX`, so doubling it still fits in 256 bits.
        let mut twice = rem;
        shl1(&mut twice, 0);
        if cmp_ge_u128(&twice, divisor) {
            quotient = quotient.checked_add(1)?;
        }
    }

    Some(quotient)
}

/// Exact magnitude of an `i128` as a `u128`.
///
/// `i128::MIN` has no positive counterpart within `i128`, so it returns `None`.
#[inline]
fn magnitude(value: i128) -> Option<u128> {
    if value >= 0 {
        Some(value as u128)
    } else {
        value.checked_neg().map(|positive| positive as u128)
    }
}

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Multiply two `u128` values and divide by a third using a full 256-bit
/// intermediate, returning `floor(value * mul / div)`.
///
/// This is the workhorse for proportional fee and share math. A naïve
/// `value * mul / div` overflows whenever `value * mul >= 2^128`, even when the
/// final quotient would fit; this helper never loses precision that way.
///
/// Returns `None` when `div == 0` or when the quotient does not fit in `u128`.
/// Division is *floor* division (rounding toward zero for non-negative inputs).
///
/// # Examples
///
/// ```
/// use tessera_no_std::checked_mul_div;
///
/// assert_eq!(checked_mul_div(10, 3, 4), Some(7));
/// assert_eq!(checked_mul_div(100, 25, 100), Some(25));
/// assert_eq!(checked_mul_div(7, 0, 3), Some(0));
/// assert_eq!(checked_mul_div(1, 1, 0), None);
/// // The intermediate (2^100 * 2^100) needs 201 bits, but the quotient fits.
/// assert_eq!(checked_mul_div(1u128 << 100, 1u128 << 100, 1u128 << 100), Some(1u128 << 100));
/// ```
#[must_use]
#[inline]
pub fn checked_mul_div(value: u128, mul: u128, div: u128) -> Option<u128> {
    if div == 0 {
        return None;
    }
    let product = mul_u128_u128(value, mul);
    div_round_256_by_u128(&product, div, false)
}

/// Apply basis points to a signed amount, rounding **half away from zero**.
///
/// One basis point is 1/10,000 (0.01%). The multiplication uses the same
/// 256-bit intermediate as [`checked_mul_div`], so even `i128::MAX` amounts and
/// the full `u16` basis-point range (0..=65_535, i.e. up to 655.35%) are exact.
///
/// Returns `None` when `amount == i128::MIN` (no positive magnitude in `i128`)
/// or when the rounded result does not fit in `i128`. A zero basis-point value
/// always yields `Some(0)`.
///
/// # Examples
///
/// ```
/// use tessera_no_std::bps;
///
/// assert_eq!(bps(10_000, 2_500), Some(2_500)); // 25%
/// assert_eq!(bps(1, 5_000), Some(1));          // 0.5 rounds away from zero
/// assert_eq!(bps(-1, 5_000), Some(-1));        // symmetric for negatives
/// assert_eq!(bps(999, 1), Some(0));            // 0.0999 rounds to zero
/// assert_eq!(bps(100, 0), Some(0));
/// ```
#[must_use]
#[inline]
pub fn bps(amount: i128, basis_points: u16) -> Option<i128> {
    if basis_points == 0 {
        return Some(0);
    }
    let negative = amount < 0;
    let magnitude = magnitude(amount)?;
    let product = mul_u128_u128(magnitude, u128::from(basis_points));
    let scaled = div_round_256_by_u128(&product, 10_000, true)?;
    if scaled > i128::MAX as u128 {
        return None;
    }
    let positive = scaled as i128;
    Some(if negative { -positive } else { positive })
}

/// Convert a `u128` to an `i128` without wrapping.
///
/// Returns `None` for values above `i128::MAX`. This is the inverse direction
/// of a safe cast and is useful at storage/host boundaries.
///
/// # Examples
///
/// ```
/// use tessera_no_std::u128_to_i128_checked;
///
/// assert_eq!(u128_to_i128_checked(42), Some(42));
/// assert_eq!(u128_to_i128_checked(i128::MAX as u128), Some(i128::MAX));
/// assert_eq!(u128_to_i128_checked(i128::MAX as u128 + 1), None);
/// ```
#[must_use]
#[inline]
pub const fn u128_to_i128_checked(value: u128) -> Option<i128> {
    if value <= i128::MAX as u128 {
        Some(value as i128)
    } else {
        None
    }
}

/// Ceiling division of `a` by `b` for `u64` values.
///
/// # Panics
///
/// Panics when `b == 0` (including in `const` contexts), mirroring the
/// behaviour of the built-in `/` operator.
///
/// # Examples
///
/// ```
/// use tessera_no_std::ceil_div_u64;
///
/// assert_eq!(ceil_div_u64(0, 5), 0);
/// assert_eq!(ceil_div_u64(1, 5), 1);
/// assert_eq!(ceil_div_u64(5, 5), 1);
/// assert_eq!(ceil_div_u64(6, 5), 2);
/// const PAGES: u64 = ceil_div_u64(9, 4);
/// assert_eq!(PAGES, 3);
/// ```
#[must_use]
#[inline]
// MSRV is 1.81 (contracts/rust-toolchain.toml); `is_multiple_of` is newer.
#[allow(clippy::manual_is_multiple_of)]
pub const fn ceil_div_u64(a: u64, b: u64) -> u64 {
    assert!(b != 0, "ceil_div_u64: division by zero");
    a / b + ((a % b != 0) as u64)
}

/// A signed fixed-point number with 64 fractional bits (Q63.64).
///
/// The raw `i128` value is scaled by `2^64`, giving roughly 19 integer digits
/// and 19 fractional digits. It is a `#[repr(transparent)]` newtype, so it has
/// no runtime overhead over `i128` and can be stored directly in Soroban state.
///
/// # Examples
///
/// ```
/// use tessera_no_std::Fixed64;
///
/// let half = Fixed64::from_fraction(1, 2).unwrap();
/// assert_eq!(half.as_i128(), 1i128 << 63);
/// assert_eq!(half.mul_floor(10), Some(5));
///
/// let third = Fixed64::from_fraction(1, 3).unwrap();
/// assert_eq!(third.mul_floor(3), Some(0)); // floor(0.999...)
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Fixed64(i128);

impl Fixed64 {
    /// Scaling factor: one whole unit equals `2^64` raw units.
    pub const SCALE: i128 = 1i128 << 64;

    /// Additive identity (zero).
    pub const ZERO: Self = Self(0);
    /// Multiplicative identity (one whole unit).
    pub const ONE: Self = Self(Self::SCALE);
    /// Smallest representable value.
    pub const MIN: Self = Self(i128::MIN);
    /// Largest representable value.
    pub const MAX: Self = Self(i128::MAX);

    /// Returns zero.
    #[must_use]
    #[inline]
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Returns one whole unit.
    #[must_use]
    #[inline]
    pub const fn one() -> Self {
        Self(Self::SCALE)
    }

    /// Wrap an already-scaled raw `i128` without validation.
    #[must_use]
    #[inline]
    pub const fn from_raw(raw: i128) -> Self {
        Self(raw)
    }

    /// Return the raw scaled `i128` value.
    #[must_use]
    #[inline]
    pub const fn as_i128(&self) -> i128 {
        self.0
    }

    /// Build a `Fixed64` from `numerator / denominator`, truncating toward zero.
    ///
    /// Uses a 256-bit intermediate, so any `i128` numerator/denominator pair is
    /// handled exactly before the final truncation to 64 fractional bits.
    ///
    /// Returns `None` for a zero denominator, for `i128::MIN` (which has no
    /// positive magnitude in `i128`), or when the scaled result overflows
    /// `i128`.
    #[must_use]
    #[inline]
    pub fn from_fraction(numerator: i128, denominator: i128) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let negative = (numerator < 0) ^ (denominator < 0);
        let numerator_magnitude = magnitude(numerator)?;
        let denominator_magnitude = magnitude(denominator)?;
        let product = mul_u128_u128(numerator_magnitude, Self::SCALE as u128);
        let scaled = div_round_256_by_u128(&product, denominator_magnitude, false)?;
        if scaled > i128::MAX as u128 {
            return None;
        }
        let raw = scaled as i128;
        Some(Self(if negative { -raw } else { raw }))
    }

    /// Multiply this fixed-point value by an integer and return `floor(result)`.
    ///
    /// The multiplication is exact (256-bit intermediate) and the fractional
    /// bits are truncated *downward*, including for negative results:
    /// `-1.5 * 1` floors to `-2`.
    ///
    /// Returns `None` on overflow of the final integer result, or when either
    /// operand is `i128::MIN` (which has no positive magnitude in `i128`).
    #[must_use]
    #[inline]
    pub fn mul_floor(self, rhs: i128) -> Option<i128> {
        if self.0 == 0 || rhs == 0 {
            return Some(0);
        }
        let negative = (self.0 < 0) ^ (rhs < 0);
        let left = magnitude(self.0)?;
        let right = magnitude(rhs)?;
        let product = mul_u128_u128(left, right);

        // `product >> 64` must fit in an i128 magnitude.
        if product[3] != 0 {
            return None;
        }
        let mut scaled = u128::from(product[1]) | (u128::from(product[2]) << 64);

        // floor(-x / 2^64) == -ceil(x / 2^64).
        if negative && product[0] != 0 {
            scaled = scaled.checked_add(1)?;
        }
        if scaled > i128::MAX as u128 {
            return None;
        }
        let result = scaled as i128;
        Some(if negative { -result } else { result })
    }
}

/// Pack two `u16` values into one `u32` (`a` in the high half, `b` in the low).
///
/// Useful for compact map keys, e.g. `(asset_id, tranche_id)`. Packing is
/// lossless — see [`unpack_u16_pair`].
///
/// # Examples
///
/// ```
/// use tessera_no_std::{pack_u16_pair, unpack_u16_pair};
///
/// let packed = pack_u16_pair(0x1234, 0xABCD);
/// assert_eq!(packed, 0x1234_ABCD);
/// assert_eq!(unpack_u16_pair(packed), (0x1234, 0xABCD));
/// ```
#[must_use]
#[inline]
pub const fn pack_u16_pair(a: u16, b: u16) -> u32 {
    ((a as u32) << 16) | (b as u32)
}

/// Unpack a `u32` produced by [`pack_u16_pair`] into `(high, low)` halves.
#[must_use]
#[inline]
pub const fn unpack_u16_pair(packed: u32) -> (u16, u16) {
    ((packed >> 16) as u16, packed as u16)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    // --- checked_mul_div ---------------------------------------------------

    #[test]
    fn checked_mul_div_basic() {
        assert_eq!(checked_mul_div(10, 3, 4), Some(7)); // floor(30/4)
        assert_eq!(checked_mul_div(100, 25, 100), Some(25));
        assert_eq!(checked_mul_div(7, 0, 3), Some(0));
        assert_eq!(checked_mul_div(0, 123, 7), Some(0));
        assert_eq!(checked_mul_div(3, 1, 10), Some(0)); // divisor > product
    }

    #[test]
    fn checked_mul_div_zero_divisor() {
        assert_eq!(checked_mul_div(1, 1, 0), None);
        assert_eq!(checked_mul_div(0, 0, 0), None);
    }

    #[test]
    fn checked_mul_div_full_precision_no_intermediate_overflow() {
        // 2^100 * 2^100 needs 201 bits; the quotient still fits in u128.
        assert_eq!(
            checked_mul_div(1u128 << 100, 1u128 << 100, 1u128 << 100),
            Some(1u128 << 100)
        );
        // (2^128-1) * 2 overflows u128 alone, but / 2 is representable.
        assert_eq!(checked_mul_div(u128::MAX, 2, 2), Some(u128::MAX));
        assert_eq!(
            checked_mul_div(u128::MAX, u128::MAX, u128::MAX),
            Some(u128::MAX)
        );
        assert_eq!(checked_mul_div(u128::MAX, 1, 1), Some(u128::MAX));
    }

    #[test]
    fn checked_mul_div_overflow_detection() {
        assert_eq!(checked_mul_div(u128::MAX, 2, 1), None);
        assert_eq!(
            checked_mul_div(1u128 << 127, 1u128 << 127, 1u128 << 126),
            None
        );
        // One below the overflow boundary still succeeds.
        assert_eq!(checked_mul_div(u128::MAX - 1, 2, 2), Some(u128::MAX - 1));
    }

    #[test]
    fn checked_mul_div_floors() {
        assert_eq!(checked_mul_div(10, 1, 3), Some(3));
        assert_eq!(checked_mul_div(u128::MAX, 1, 2), Some(u128::MAX / 2));
        assert_eq!(checked_mul_div(17, 17, 5), Some(57)); // 289 / 5 = 57
    }

    // --- bps ---------------------------------------------------------------

    #[test]
    fn bps_common_rates() {
        assert_eq!(bps(10_000, 2_500), Some(2_500));
        assert_eq!(bps(10_000, 1), Some(1));
        assert_eq!(bps(1_000_000, 1_000), Some(100_000));
        assert_eq!(bps(100, 0), Some(0));
        assert_eq!(bps(0, 65_535), Some(0));
    }

    #[test]
    fn bps_rounds_half_away_from_zero() {
        assert_eq!(bps(1, 5_000), Some(1));
        assert_eq!(bps(-1, 5_000), Some(-1));
        assert_eq!(bps(999, 1), Some(0)); // 0.0999 -> 0
        assert_eq!(bps(-999, 1), Some(0));
        assert_eq!(bps(1, 15_000), Some(2)); // 1.5 -> 2
        assert_eq!(bps(-1, 15_000), Some(-2));
    }

    #[test]
    fn bps_edge_cases() {
        assert_eq!(bps(i128::MIN, 5_000), None); // no positive magnitude
        assert_eq!(bps(i128::MAX, 65_535), None); // result overflows i128
        assert_eq!(bps(i128::MAX, 0), Some(0));
        assert_eq!(bps(-10_000, 2_500), Some(-2_500));
    }

    // --- conversions / ceil ------------------------------------------------

    #[test]
    fn u128_to_i128_checked_boundaries() {
        assert_eq!(u128_to_i128_checked(0), Some(0));
        assert_eq!(u128_to_i128_checked(123), Some(123));
        assert_eq!(u128_to_i128_checked(i128::MAX as u128), Some(i128::MAX));
        assert_eq!(u128_to_i128_checked(i128::MAX as u128 + 1), None);
        assert_eq!(u128_to_i128_checked(u128::MAX), None);
    }

    #[test]
    fn ceil_div_u64_cases() {
        assert_eq!(ceil_div_u64(0, 5), 0);
        assert_eq!(ceil_div_u64(1, 5), 1);
        assert_eq!(ceil_div_u64(4, 5), 1);
        assert_eq!(ceil_div_u64(5, 5), 1);
        assert_eq!(ceil_div_u64(6, 5), 2);
        assert_eq!(ceil_div_u64(u64::MAX, 1), u64::MAX);
        assert_eq!(ceil_div_u64(u64::MAX, 2), u64::MAX / 2 + 1);
    }

    #[test]
    #[should_panic(expected = "division by zero")]
    fn ceil_div_u64_panics_on_zero() {
        let _ = ceil_div_u64(1, 0);
    }

    // --- Fixed64 -----------------------------------------------------------

    #[test]
    fn fixed64_from_fraction_exact() {
        assert_eq!(Fixed64::from_fraction(1, 2).unwrap().as_i128(), 1i128 << 63);
        assert_eq!(Fixed64::from_fraction(3, 4).unwrap().as_i128(), 3i128 << 62);
        assert_eq!(Fixed64::from_fraction(0, 7).unwrap(), Fixed64::ZERO);
        assert_eq!(
            Fixed64::from_fraction(-1, 2).unwrap().as_i128(),
            -(1i128 << 63)
        );
        assert_eq!(
            Fixed64::from_fraction(-2, -4).unwrap(),
            Fixed64::from_fraction(1, 2).unwrap()
        );
    }

    #[test]
    fn fixed64_from_fraction_truncates_toward_zero() {
        let third = Fixed64::from_fraction(1, 3).unwrap();
        assert_eq!(third.as_i128(), (1i128 << 64) / 3);
        // 1/3 truncated down, applied to 3, floors to 0 (not 1).
        assert_eq!(third.mul_floor(3), Some(0));
        assert_eq!(Fixed64::from_fraction(2, 3).unwrap().mul_floor(3), Some(1));
    }

    #[test]
    fn fixed64_from_fraction_invalid() {
        assert_eq!(Fixed64::from_fraction(1, 0), None);
        assert_eq!(Fixed64::from_fraction(i128::MIN, 1), None);
        assert_eq!(Fixed64::from_fraction(i128::MAX, 1), None); // scaling overflows
    }

    #[test]
    fn fixed64_mul_floor() {
        assert_eq!(Fixed64::from_fraction(1, 2).unwrap().mul_floor(10), Some(5));
        assert_eq!(Fixed64::from_fraction(1, 2).unwrap().mul_floor(0), Some(0));
        assert_eq!(Fixed64::zero().mul_floor(123), Some(0));
        assert_eq!(Fixed64::ONE.mul_floor(42), Some(42));
        assert_eq!(Fixed64::from_fraction(3, 2).unwrap().mul_floor(4), Some(6));
    }

    #[test]
    fn fixed64_mul_floor_negative() {
        // floor(-0.5 * 3) == floor(-1.5) == -2
        assert_eq!(
            Fixed64::from_fraction(-1, 2).unwrap().mul_floor(3),
            Some(-2)
        );
        // floor(0.5 * -3) == floor(-1.5) == -2
        assert_eq!(
            Fixed64::from_fraction(1, 2).unwrap().mul_floor(-3),
            Some(-2)
        );
        assert_eq!(
            Fixed64::from_fraction(-1, 2).unwrap().mul_floor(-3),
            Some(1)
        );
    }

    #[test]
    fn fixed64_mul_floor_large_and_overflow() {
        // Raw i128::MAX scaled down by 2^64 is ~2^63, so doubling is fine.
        assert_eq!(
            Fixed64::from_raw(i128::MAX).mul_floor(2),
            Some((1i128 << 64) - 1)
        );
        // `i128::MIN` has no positive magnitude in `i128`, so it is rejected.
        assert_eq!(Fixed64::MIN.mul_floor(2), None);
        // Multiplying by 2^65 pushes the integer result past i128::MAX/MIN.
        assert_eq!(Fixed64::MAX.mul_floor(1i128 << 65), None);
        assert_eq!(u128_to_i128_checked(i128::MAX as u128 + 1), None);
    }

    #[test]
    fn fixed64_constants_and_raw() {
        assert_eq!(Fixed64::ZERO, Fixed64::zero());
        assert_eq!(Fixed64::ONE, Fixed64::one());
        assert_eq!(Fixed64::SCALE, 1i128 << 64);
        assert_eq!(Fixed64::from_raw(99).as_i128(), 99);
    }

    // --- packing -----------------------------------------------------------

    #[test]
    fn pack_unpack_roundtrip() {
        for (a, b) in [
            (0, 0),
            (1, 2),
            (u16::MAX, 0),
            (0, u16::MAX),
            (u16::MAX, u16::MAX),
            (0x1234, 0xABCD),
        ] {
            assert_eq!(unpack_u16_pair(pack_u16_pair(a, b)), (a, b));
        }
    }

    #[test]
    fn pack_layout() {
        assert_eq!(pack_u16_pair(0x1234, 0xABCD), 0x1234_ABCD);
        assert_eq!(unpack_u16_pair(0x0001_0002), (1, 2));
    }

    // --- private primitive cross-checks against u128 -----------------------

    #[test]
    fn mul_u128_u128_matches_u128_when_it_fits() {
        for (a, b) in [
            (0u128, 0u128),
            (1, u128::MAX),
            (u32::MAX as u128, u32::MAX as u128),
            (1u128 << 64, 1u128 << 63),
            (u64::MAX as u128, u64::MAX as u128),
        ] {
            let limbs = mul_u128_u128(a, b);
            if let Some(expected) = a.checked_mul(b) {
                assert_eq!(limbs[2], 0);
                assert_eq!(limbs[3], 0);
                assert_eq!(
                    u128::from(limbs[0]) | (u128::from(limbs[1]) << 64),
                    expected
                );
            }
        }
    }
}
