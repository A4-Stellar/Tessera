//! Shared fuzzing harness for the Tessera Soroban contract suite.
//!
//! Two things live here, and both are deliberately dependency-free (no
//! `arbitrary`, no `libfuzzer-sys`) so that the exact same code drives
//!
//! * the `cargo-fuzz` / libFuzzer targets in [`contracts/fuzz/fuzz_targets`],
//!   and
//! * the `proptest` property targets in [`contracts/fuzz/proptests`].
//!
//! # [`Input`]
//!
//! A deterministic, total cursor over the fuzzer-supplied byte slice. "Total" is
//! the important part: when the input is exhausted the cursor keeps producing
//! values from a SplitMix64 stream seeded off the input itself, so a 2-byte
//! input still drives a full fuzz iteration instead of bailing out early. Early
//! bail-outs are the single biggest reason a libFuzzer target stops making
//! progress.
//!
//! [`Input::i128_interesting`] deliberately over-samples the boundaries that
//! actually break `i128` money math — `0`, `±1`, `i128::MAX`/`MIN`,
//! `i128::MAX/10_000`, and the `u64`/`u32` ceilings — because a uniform random
//! `i128` essentially never lands on an overflow trigger.
//!
//! # [`check_no_trap`]
//!
//! The oracle. In a native Soroban test environment the host wraps every
//! contract call in a panic catcher in order to emulate WASM trap semantics.
//! The three outcomes of a `try_*` call are therefore fully distinguishable:
//!
//! | what happened in the contract        | what the host reports                    |
//! |--------------------------------------|------------------------------------------|
//! | returned normally                    | `Ok(Ok(value))`                           |
//! | `panic_with_error!(env, MyError::Foo)`| `Err(Ok(MyError::Foo))` — *declared*      |
//! | a bare `panic!()`                    | `Err(Err(InvokeError::Abort))` — a *trap* |
//!
//! The third row is the bug class this component exists to find. A bare panic
//! in Rust is how an arithmetic overflow, a `.unwrap()` on `None`, an ignored
//! `checked_*` failure, and an out-of-bounds index all surface, and the host maps
//! every one of them onto the same `InvokeError::Abort` that a WASM trap would
//! produce. So `Err(Err(InvokeError::Abort))` is never benign: it means the
//! contract trapped where it should have returned a declared error.
//!
//! [`check_no_trap`] turns that into a `panic!` so libFuzzer records and
//! minimises a reproducer, and so `proptest` shrinks it to a readable
//! counterexample.
//!
//! # Why every contract here declares `#[contracterror]`
//!
//! There is deliberately no "raw panic" variant of this oracle, because it
//! cannot be written soundly. A contract that signals failure with
//! `panic!("unauthorized")` produces exactly the same `InvokeError::Abort` as
//! `i128::MAX + 1`, so any oracle for such a contract either flags every
//! deliberate rejection as a finding (useless) or swallows every real overflow
//! (worse). Giving these contracts a `#[contracterror]` enum is therefore not
//! cosmetic — it is what makes them fuzzable at all, and it was the first thing
//! the fuzzing work changed.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use core::fmt::Debug;

use soroban_sdk::{ConversionError, InvokeError};

/// Longest stateful call sequence a single fuzz iteration will drive.
///
/// Bounded so that one iteration stays well inside a single invocation's
/// resource budget (a fresh [`soroban_sdk::Env`] is created per iteration, and
/// budget exhaustion is reported as `InvokeError::Abort`, which this harness
/// deliberately treats as a finding — so exceeding it would be a false
/// positive).
pub const MAX_OPS: usize = 24;

/// Longest `soroban_sdk::Vec` the targets will build for proof/signature
/// arguments, kept small for the same budget reason.
pub const MAX_SEQ_LEN: usize = 8;

// ---------------------------------------------------------------------------
// Input cursor
// ---------------------------------------------------------------------------

/// Deterministic, total cursor over the fuzzer's byte slice.
#[derive(Debug)]
pub struct Input<'a> {
    data: &'a [u8],
    pos: usize,
    state: u64,
}

impl<'a> Input<'a> {
    /// Wrap the fuzzer-supplied bytes.
    pub fn new(data: &'a [u8]) -> Self {
        // Seed the fallback stream from the input itself (FNV-1a) so that two
        // different inputs never produce the same continuation.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in data {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            data,
            pos: 0,
            state: hash ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    /// True once every input byte has been consumed.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Number of bytes still unread.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Next raw byte, from the input if any remain, else from the fallback
    /// stream.
    pub fn u8(&mut self) -> u8 {
        if let Some(b) = self.data.get(self.pos) {
            self.pos += 1;
            *b
        } else {
            (self.next_u64() >> 24) as u8
        }
    }

    /// Next `u8`, masked into `0..n` (returns `0` when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.u8() as usize) % n
    }

    /// Next `bool`, unbiased across the whole byte (any non-zero byte is
    /// `true`).
    pub fn bool(&mut self) -> bool {
        self.u8() & 1 == 1
    }

    /// Next uniform `u16`.
    #[must_use]
    pub fn u16(&mut self) -> u16 {
        let mut buf = [0u8; 2];
        self.fill(&mut buf);
        u16::from_le_bytes(buf)
    }

    /// Next uniform `u32`.
    #[must_use]
    pub fn u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        self.fill(&mut buf);
        u32::from_le_bytes(buf)
    }

    /// Next uniform `u64`.
    #[must_use]
    pub fn u64(&mut self) -> u64 {
        self.next_u64()
    }

    /// Next uniform `i128`, assembled from two `u64` halves.
    #[must_use]
    pub fn i128(&mut self) -> i128 {
        let lo = self.next_u64();
        let hi = self.next_u64();
        ((hi as u128) << 64 | lo as u128) as i128
    }

    /// Next `u32` biased towards the interesting boundaries: `0`, `1`, the
    /// `u32::MAX` neighbourhood, and small ledger-style values.
    #[must_use]
    pub fn u32_interesting(&mut self) -> u32 {
        const INTERESTING: [u32; 12] = [
            0,
            1,
            2,
            3,
            10,
            100,
            1_000,
            10_000,
            100_000,
            u32::MAX,
            u32::MAX - 1,
            0x8000_0000,
        ];
        self.pick_u32(&INTERESTING)
    }

    /// Next `u64` biased towards the interesting boundaries: `0`, `1`,
    /// `u64::MAX`, and the `i128`/`u32` ceilings that show up when a `u64`
    /// timestamp or counter meets an `i128` amount.
    #[must_use]
    pub fn u64_interesting(&mut self) -> u64 {
        const INTERESTING: [u64; 14] = [
            0,
            1,
            2,
            10,
            10_000,
            1_000_000,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            i64::MAX as u64,
            i64::MAX as u64 + 1,
            i128::MAX as u64,
            u64::MAX - 1,
            u64::MAX,
            0x8000_0000_0000_0000,
        ];
        if self.below(2) == 0 {
            self.pick_u64(&INTERESTING)
        } else {
            self.u64()
        }
    }

    /// Next `i128` biased towards the values that break money math: `0`, `±1`,
    /// the `i128` extremes, the `u64`/`u32` ceilings, and `i128::MAX / 10_000`
    /// (the dividend basis-point divisor, and the exact point where
    /// `amount * rate_bps` stops fitting in an `i128` for any realistic
    /// `rate_bps`).
    #[must_use]
    pub fn i128_interesting(&mut self) -> i128 {
        const INTERESTING: [i128; 22] = [
            0,
            1,
            -1,
            2,
            -2,
            10,
            -10,
            100,
            -100,
            10_000,
            -10_000,
            1_000_000,
            1_000_000_000_000,
            u32::MAX as i128,
            u32::MAX as i128 + 1,
            u64::MAX as i128,
            i64::MAX as i128,
            i64::MIN as i128,
            i128::MAX,
            i128::MAX - 1,
            i128::MIN,
            i128::MIN + 1,
        ];
        // One slot in eight is left to the pure-random path so the target is
        // not restricted to the hand-picked set; the rest is drawn from the
        // boundaries, which uniform sampling would essentially never reach.
        if self.below(8) == 0 {
            self.i128()
        } else {
            self.pick_i128(&INTERESTING)
        }
    }

    /// Next `i128` constrained to a caller-chosen set of extreme magnitudes
    /// (for "transfer `n` units of a `SUPPLY`-sized token" style targets).
    #[must_use]
    pub fn i128_amount(&mut self, supply: i128) -> i128 {
        if supply <= 0 {
            return self.i128_interesting();
        }
        const SCALE: [i128; 8] = [
            0,
            1,
            -1,
            9_999_999,
            1_000_000,
            1_000_000_000,
            340_282_366_920_938_463_463_374_607_431,
            1_000_000_000_000_000_000,
        ];
        let magnitude = self.pick_i128(&SCALE).unsigned_abs() as i128;
        match self.below(4) {
            0 => magnitude,
            1 => -magnitude,
            2 => supply,
            _ => supply.saturating_sub(magnitude),
        }
    }

    /// Next ASCII string of length `0..=max`, drawn from a small alphabet that
    /// includes non-ASCII bytes so `String` length/encoding edges get hit.
    #[must_use]
    pub fn ascii(&mut self, max: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_/ \x00\x7f\xff";
        let len = self.below(max + 1);
        let mut out = String::with_capacity(len);
        for _ in 0..len {
            out.push(ALPHABET[self.below(ALPHABET.len())] as char);
        }
        out
    }

    /// Next `[u8; 32]`.
    #[must_use]
    pub fn bytes32(&mut self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        self.fill(&mut buf);
        buf
    }

    /// Next `[u8; 64]`.
    #[must_use]
    pub fn bytes64(&mut self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        self.fill(&mut buf);
        buf
    }

    /// Next byte slice of exactly `n` bytes.
    #[must_use]
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        self.fill(&mut out);
        out
    }

    /// Length of the next sequence/vector argument, always in
    /// `0..=MAX_SEQ_LEN`.
    #[must_use]
    pub fn seq_len(&mut self) -> u32 {
        self.below(MAX_SEQ_LEN + 1) as u32
    }

    // -- internals ---------------------------------------------------------

    fn fill(&mut self, buf: &mut [u8]) {
        let mut i = 0;
        while i < buf.len() {
            // Take only what is actually left in the input; never read past the
            // end (that is the whole point of `get`).
            let want = core::cmp::min(8, buf.len() - i);
            match self.data.get(self.pos..self.pos + want) {
                Some(chunk) => {
                    buf[i..i + chunk.len()].copy_from_slice(chunk);
                    self.pos += chunk.len();
                }
                None => {
                    let word = self.next_u64().to_le_bytes();
                    buf[i..i + want].copy_from_slice(&word[..want]);
                }
            }
            i += want;
        }
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64: cheap, well-distributed, and needs no state beyond `u64`.
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn pick_u32(&mut self, pool: &[u32]) -> u32 {
        pool[self.below(pool.len())]
    }

    fn pick_u64(&mut self, pool: &[u64]) -> u64 {
        pool[self.below(pool.len())]
    }

    fn pick_i128(&mut self, pool: &[i128]) -> i128 {
        pool[self.below(pool.len())]
    }
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// Outcome of a `try_*` client call, as generated by `#[contractimpl]`:
///
/// ```text
/// Result<Result<T, ConversionError>, Result<ContractError, InvokeError>>
/// ```
pub type TryResult<T, CErr> = Result<Result<T, ConversionError>, Result<CErr, InvokeError>>;

/// Assert that a `try_*` call to a contract with a `#[contracterror]` enum
/// either succeeded or failed with one of the contract's *declared* errors.
///
/// Panics (so libFuzzer records a minimised reproducer) on:
///
/// * `InvokeError::Abort` — the contract trapped. In a native test env the host
///   turns any bare `panic!` into a trap, and that is exactly what an
///   overflowing `+`/`-`/`*`, a `.unwrap()` on `None`, or an out-of-bounds index
///   looks like.
/// * `InvokeError::Contract(code)` — a host error whose code is not one of the
///   contract's declared `#[contracterror]` variants. Because the generated
///   `try_*` client maps every declared variant onto `Err(Ok(_))`, reaching
///   this arm at all means something signalled failure by a channel the
///   contract's error enum does not cover.
///
/// The two accepted arms are deliberately distinguished from the rejected ones
/// by [`succeeded`], so a target can additionally assert that an operation which
/// *should* have succeeded actually did.
pub fn check_no_trap<T, CErr>(res: &TryResult<T, CErr>, ctx: &str)
where
    CErr: Debug,
{
    match res {
        Ok(_) => {}
        Err(Ok(_)) => {}
        Err(Err(InvokeError::Abort)) => finding(
            ctx,
            "contract trapped (arithmetic overflow, unwrap on None, or out-of-bounds \
             access) instead of returning a declared error",
        ),
        Err(Err(InvokeError::Contract(code))) => {
            finding(ctx, &format!("undeclared contract error code {code}"))
        }
    }
}

/// `true` when the call returned a value rather than any form of failure.
///
/// Note that this is *not* `res.is_ok()`: an `Ok(Err(_))` inner result is a
/// host `ConversionError` (an argument that could not be encoded for the
/// contract's ABI), which is a failure of the harness, not of the contract.
#[must_use]
pub fn succeeded<T, CErr>(res: &TryResult<T, CErr>) -> bool {
    matches!(res, Ok(Ok(_)))
}

/// `true` when the call failed with one of the contract's declared
/// `#[contracterror]` variants.
#[must_use]
pub fn declared_error<T, CErr>(res: &TryResult<T, CErr>) -> bool {
    matches!(res, Err(Ok(_)))
}

/// `true` when the call failed with exactly `expected`.
///
/// This is stronger than `!succeeded(&res)` and is what a negative test should
/// use whenever the *reason* for rejection is the thing under test. Checking
/// only that a call failed lets a test pass for the wrong reason: a message
/// already marked processed, an expired TTL, or a paused contract will all
/// refuse a call, so a signature or length check that never ran still looks
/// like a pass. Comparing the declared error pins down which check fired.
#[must_use]
pub fn failed_with<T, CErr>(res: &TryResult<T, CErr>, expected: CErr) -> bool
where
    CErr: Debug + PartialEq,
{
    matches!(res, Err(Ok(e)) if *e == expected)
}

/// Report an invariant violation. Routed through `panic!` so libFuzzer treats
/// it as a crash and minimises the input, and so `proptest` shrinks it to a
/// readable counterexample.
pub fn finding(ctx: &str, detail: &str) -> ! {
    panic!("[tessera-fuzz] {ctx}: {detail}");
}

/// Assert a conservation law that must hold across an operation.
pub fn assert_conserved(ctx: &str, what: &str, before: i128, after: i128) {
    if before != after {
        let delta = after.saturating_sub(before);
        finding(
            ctx,
            &format!("{what} not conserved: {before} -> {after} (delta {delta})"),
        );
    }
}

// ---------------------------------------------------------------------------
// Ledger control
// ---------------------------------------------------------------------------

use soroban_sdk::{testutils::Ledger as _, Env};

/// Current ledger sequence number.
#[must_use]
pub fn sequence(env: &Env) -> u32 {
    env.ledger().sequence()
}

/// Current ledger timestamp.
#[must_use]
pub fn timestamp(env: &Env) -> u64 {
    env.ledger().timestamp()
}

/// Set the ledger to an explicit position.
///
/// Absolute extremes are only safe *before* any state exists, which is why the
/// stateful targets use [`step_ledger`] instead.
pub fn set_ledger(env: &Env, sequence_number: u32, ts: u64) {
    env.ledger().set_sequence_number(sequence_number);
    env.ledger().set_timestamp(ts);
}

/// Largest single-step sequence advance used by [`step_ledger`].
///
/// Chosen so that `MAX_OPS * MAX_STEP` stays far below
/// `min_persistent_entry_ttl` (4096), which is what the test ledger uses. This
/// matters more than it looks: a persistent entry written at sequence `S` is
/// readable only while the sequence is below `S + min_persistent_entry_ttl`.
/// Once it lapses, the host returns `EntryExpired`, which reaches the client as
/// `InvokeError::Abort` — the *same* channel as a genuine arithmetic trap. So an
/// over-large step does not merely lose coverage, it makes `check_no_trap` report
/// a finding that is an artifact of the harness. With `MAX_OPS = 24` and a step
/// of up to 8, the worst case is a sequence of 192.
pub const MAX_STEP: u32 = 8;

/// Assert at compile time that the worst-case sequence advance cannot outrun the
/// shortest-lived persistent entry.
///
/// If this ever fires, raising `MAX_OPS` or `MAX_STEP` past the TTL bound would
/// silently turn every stateful target into a false-positive generator, which is
/// exactly the failure mode that is hardest to notice and most likely to be
/// mistaken for a real bug in the contracts.
const _: () = assert!(
    (MAX_OPS as u64) * (MAX_STEP as u64) < 4096,
    "MAX_OPS * MAX_STEP must stay under min_persistent_entry_ttl (4096), or \
     persistent entries will expire mid-run and surface as InvokeError::Abort"
);

/// Advance the ledger by a small, bounded amount.
///
/// Deliberately *not* an absolute jump. Persistent entries carry a TTL relative
/// to the sequence number at which they were written, and the test ledger is
/// configured with `min_persistent_entry_ttl: 4096` / `max_entry_ttl: 6_312_000`.
/// Teleporting the sequence number past `max_entry_ttl` expires every entry, and
/// the resulting `EntryExpired` host error surfaces as `InvokeError::Abort` —
/// indistinguishable from a real trap, i.e. a false positive. The step is also
/// kept small enough that the *cumulative* advance over a whole run stays under
/// `min_persistent_entry_ttl`; see [`MAX_STEP`].
///
/// Bounded steps keep the ledger-sensitive code paths (`lockup`, KYC
/// `expires_at`, bridge `ttl`) genuinely exercised without ever stranding the
/// storage. The timestamp is advanced on a much wider scale, because nothing
/// stores a TTL against it.
pub fn step_ledger(env: &Env, input: &mut Input<'_>) {
    let seq = env
        .ledger()
        .sequence()
        .saturating_add(1 + input.below(MAX_STEP as usize) as u32);
    env.ledger().set_sequence_number(seq);
    let ts = env
        .ledger()
        .timestamp()
        .saturating_add(input.u64() % 86_400);
    env.ledger().set_timestamp(ts);
}
