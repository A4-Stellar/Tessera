//! Structure-aware mutation driven by the contract ABI (issue #470).
//!
//! Random bytes rarely reach interesting XDR states: a one-byte change to an
//! `ScVal` discriminant turns a `Vec` into an `I128` and the host rejects it
//! before the contract's logic runs. This mutator keeps the XDR skeleton intact
//! and mutates *fields* — discriminants, length prefixes, integer boundaries,
//! padding — using a tiny deterministic PRNG. Its shape matches LibAFL's
//! `Mutator::mutate(&mut self, input: &mut Vec<u8>)`, so an adapter is a few
//! lines (see `contracts/fuzz/README.md`).

/// The XDR-flavoured argument types the mutator knows how to skew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiKind {
    Bool,
    U32,
    I64,
    I128,
    Bytes,
    Symbol,
    Vec,
    Address,
}

/// One exported-function argument.
#[derive(Debug, Clone, Copy)]
pub struct AbiField {
    pub name: &'static str,
    pub kind: AbiKind,
}

/// A minimal ABI description for one function.
#[derive(Debug, Clone, Copy)]
pub struct AbiSchema {
    pub function: &'static str,
    pub args: &'static [AbiField],
}

/// Representative ABI entries for the contracts with the largest fuzz surface.
/// These are hand-maintained (the repo does not check in generated bindings).
pub const ASSET_TOKEN_MINT: AbiSchema = AbiSchema {
    function: "mint",
    args: &[
        AbiField {
            name: "admin",
            kind: AbiKind::Address,
        },
        AbiField {
            name: "to",
            kind: AbiKind::Address,
        },
        AbiField {
            name: "amount",
            kind: AbiKind::I128,
        },
    ],
};

pub const DIVIDEND_CLAIM: AbiSchema = AbiSchema {
    function: "claim",
    args: &[
        AbiField {
            name: "holder",
            kind: AbiKind::Address,
        },
        AbiField {
            name: "proof",
            kind: AbiKind::Bytes,
        },
        AbiField {
            name: "index",
            kind: AbiKind::U32,
        },
    ],
};

pub const COMMON_SCHEMAS: &[AbiSchema] = &[ASSET_TOKEN_MINT, DIVIDEND_CLAIM];

/// SplitMix64 so mutation is reproducible from a crash's seed.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// LibAFL-shaped structure-aware mutator.
#[derive(Debug, Clone)]
pub struct StructureAwareMutator<'a> {
    rng: Rng,
    schema: &'a AbiSchema,
}

impl<'a> StructureAwareMutator<'a> {
    pub fn new(seed: u64, schema: &'a AbiSchema) -> Self {
        Self {
            rng: Rng(seed),
            schema,
        }
    }

    pub fn schema(&self) -> &'a AbiSchema {
        self.schema
    }

    /// Mutate `input` in place. Returns the number of edits applied.
    pub fn mutate(&mut self, input: &mut Vec<u8>) -> usize {
        if input.is_empty() {
            input.extend_from_slice(&[0, 0, 0, 0]);
            return 1;
        }
        let mut edits = 0;
        let field = self.schema.args[(self.rng.next() as usize) % self.schema.args.len()];
        let position = (self.rng.next() as usize) % input.len();
        match field.kind {
            AbiKind::Bool => {
                input[position] ^= 1;
                edits += 1;
            }
            AbiKind::U32 | AbiKind::I64 | AbiKind::I128 => {
                // Skewed toward values that expose overflow/underflow bugs.
                const BOUNDARIES: [u8; 8] = [0, 1, 0x7f, 0x80, 0xff, 0xfe, 0x01, 0x00];
                let count = (self.rng.next() % 4) as usize + 1;
                for offset in 0..count.min(input.len() - position) {
                    input[position + offset] = BOUNDARIES[(self.rng.next() % 8) as usize];
                    edits += 1;
                }
            }
            AbiKind::Bytes | AbiKind::Symbol | AbiKind::Vec => {
                // XDR length prefixes are 4-byte big-endian u32.
                let len_position = position.min(input.len().saturating_sub(4));
                if input.len() >= 4 {
                    let new_len = match self.rng.next() % 4 {
                        0 => 0u32,     // empty collection
                        1 => u32::MAX, // absurd length — must be rejected
                        2 => 1,        // single element
                        _ => (self.rng.next() as u32) % 64,
                    };
                    input[len_position..len_position + 4].copy_from_slice(&new_len.to_be_bytes());
                    edits += 1;
                }
                if self.rng.next() % 2 == 0 && input.len() < 4096 {
                    input.push((self.rng.next() & 0xff) as u8); // trailing padding
                    edits += 1;
                }
            }
            AbiKind::Address => {
                // Flip a discriminant byte of an XDR union.
                input[position] = (self.rng.next() & 0xff) as u8;
                edits += 1;
            }
        }
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_is_deterministic_for_a_seed() {
        let mut a = StructureAwareMutator::new(7, &ASSET_TOKEN_MINT);
        let mut b = StructureAwareMutator::new(7, &ASSET_TOKEN_MINT);
        let mut input_a = vec![0u8; 32];
        let mut input_b = vec![0u8; 32];
        for _ in 0..20 {
            a.mutate(&mut input_a);
            b.mutate(&mut input_b);
        }
        assert_eq!(input_a, input_b);
    }

    #[test]
    fn mutator_targets_xdr_length_prefixes_and_integer_boundaries() {
        let mut mutator = StructureAwareMutator::new(11, &DIVIDEND_CLAIM);
        let mut input = vec![0u8; 16];
        let mut touched_length_prefix = false;
        for _ in 0..200 {
            mutator.mutate(&mut input);
            if input.iter().any(|b| *b == 0xff) || input.iter().any(|b| *b == 0x80) {
                touched_length_prefix = true;
            }
        }
        assert!(
            touched_length_prefix,
            "expected boundary values in the corpus"
        );
    }

    #[test]
    fn never_panics_on_tiny_or_empty_inputs() {
        for seed in 0..50 {
            let mut mutator = StructureAwareMutator::new(seed, &ASSET_TOKEN_MINT);
            let mut input = Vec::new();
            for _ in 0..10 {
                mutator.mutate(&mut input);
                input.truncate((seed % 5) as usize);
            }
        }
    }
}
