//! Structure-Aware XDR Mutator for Soroban ScVal Byte Sequences (Issue #370).
//!
//! Generates valid-but-adversarial ScVal XDR encodings:
//! - Mutates field lengths (0, boundary lengths, bad 4-byte padding, off-by-one).
//! - Injects boundary integers (0, ±1, i32/u32/i64/u64/i128 bounds).
//! - Swaps ScVal variant discriminants.

#[derive(Debug, Clone)]
pub struct StructuredXdrMutator {
    prng_state: u64,
}

impl Default for StructuredXdrMutator {
    fn default() -> Self {
        Self::new(0x853c49e6748fea9b)
    }
}

impl StructuredXdrMutator {
    pub fn new(seed: u64) -> Self {
        Self {
            prng_state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.prng_state = self.prng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.prng_state >> 32) as u32
    }

    fn next_u64(&mut self) -> u64 {
        let hi = self.next_u32() as u64;
        let lo = self.next_u32() as u64;
        (hi << 32) | lo
    }

    /// Mutates an arbitrary input byte slice into an adversarial XDR byte buffer.
    pub fn mutate(&mut self, data: &[u8]) -> std::vec::Vec<u8> {
        if data.is_empty() {
            return self.generate_adversarial_scval();
        }

        let mut out = std::vec::Vec::from(data);
        let mutation_choice = self.next_u32() % 4;

        match mutation_choice {
            0 => {
                // Mutate length prefix: modify first 4 bytes if present
                if out.len() >= 4 {
                    let boundary_len = match self.next_u32() % 5 {
                        0 => 0u32,
                        1 => 1u32,
                        2 => 32u32,
                        3 => u32::MAX - 3,
                        _ => (out.len() as u32).wrapping_add(4),
                    };
                    out[0..4].copy_from_slice(&boundary_len.to_be_bytes());
                }
            }
            1 => {
                // Inject boundary integer at random 4-byte aligned offset
                let boundary_val = match self.next_u32() % 6 {
                    0 => 0i32,
                    1 => 1i32,
                    2 => -1i32,
                    3 => i32::MAX,
                    4 => i32::MIN,
                    _ => 0x7fff_ffff,
                };
                let aligned_len = (out.len() / 4) * 4;
                if aligned_len >= 4 {
                    let slot = ((self.next_u32() as usize) % (aligned_len / 4)) * 4;
                    out[slot..slot + 4].copy_from_slice(&boundary_val.to_be_bytes());
                }
            }
            2 => {
                // Swap variant tag in first 4 bytes
                let tag = match self.next_u32() % 10 {
                    0 => 0u32,  // Bool
                    1 => 1u32,  // Void
                    2 => 4u32,  // U32
                    3 => 5u32,  // I32
                    4 => 6u32,  // U64
                    5 => 7u32,  // I64
                    6 => 10u32, // Bytes
                    7 => 11u32, // String
                    8 => 12u32, // Symbol
                    _ => 13u32, // Vec
                };
                if out.len() >= 4 {
                    out[0..4].copy_from_slice(&tag.to_be_bytes());
                }
            }
            _ => {
                // Corrupt XDR 4-byte padding bytes at end
                let len = out.len();
                if len % 4 != 0 {
                    let pad = 4 - (len % 4);
                    for _ in 0..pad {
                        out.push(1); // non-zero padding corruption
                    }
                }
            }
        }

        out
    }

    /// Synthesizes a valid-but-adversarial ScVal XDR byte payload from scratch.
    pub fn generate_adversarial_scval(&mut self) -> std::vec::Vec<u8> {
        let mut buf = std::vec::Vec::new();
        let variant = self.next_u32() % 7;

        match variant {
            0 => {
                // ScVal::Bool
                buf.extend_from_slice(&0u32.to_be_bytes());
                let b = if self.next_u32() % 2 == 0 { 1u32 } else { 0u32 };
                buf.extend_from_slice(&b.to_be_bytes());
            }
            1 => {
                // ScVal::U32 (tag 4) with boundary integer
                buf.extend_from_slice(&4u32.to_be_bytes());
                let val = match self.next_u32() % 4 {
                    0 => 0u32,
                    1 => 1u32,
                    2 => u32::MAX,
                    _ => u32::MAX / 2,
                };
                buf.extend_from_slice(&val.to_be_bytes());
            }
            2 => {
                // ScVal::I64 (tag 7) with boundary integer
                buf.extend_from_slice(&7u32.to_be_bytes());
                let val = match self.next_u32() % 4 {
                    0 => 0i64,
                    1 => -1i64,
                    2 => i64::MAX,
                    _ => i64::MIN,
                };
                buf.extend_from_slice(&val.to_be_bytes());
            }
            3 => {
                // ScVal::Bytes (tag 10) with mutated length and 4-byte alignment
                buf.extend_from_slice(&10u32.to_be_bytes());
                let payload_len = (self.next_u32() % 16) as usize;
                buf.extend_from_slice(&(payload_len as u32).to_be_bytes());
                buf.resize(buf.len() + payload_len, 0xAA);
                // XDR zero-padding
                let pad = (4 - (payload_len % 4)) % 4;
                buf.resize(buf.len() + pad, 0);
            }
            4 => {
                // ScVal::Symbol (tag 12)
                buf.extend_from_slice(&12u32.to_be_bytes());
                let symbol_str = b"TRANSFER";
                buf.extend_from_slice(&(symbol_str.len() as u32).to_be_bytes());
                buf.extend_from_slice(symbol_str);
            }
            5 => {
                // ScVal::Vec (tag 13) - empty or nested
                buf.extend_from_slice(&13u32.to_be_bytes());
                buf.extend_from_slice(&0u32.to_be_bytes()); // length 0
            }
            _ => {
                // ScVal::I128 (tag 9)
                buf.extend_from_slice(&9u32.to_be_bytes());
                let val = match self.next_u32() % 3 {
                    0 => 0i128,
                    1 => i128::MAX,
                    _ => i128::MIN,
                };
                buf.extend_from_slice(&val.to_be_bytes());
            }
        }

        buf
    }
}
