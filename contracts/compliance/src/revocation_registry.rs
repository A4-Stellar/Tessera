use soroban_sdk::{contracttype, Address, Env, Vec};

const BITS_PER_WORD: u32 = 64;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey { RevocationWord(Address, u32) }

pub struct RevocationRegistry;
impl RevocationRegistry {
    pub fn set(env: &Env, issuer: &Address, subject_index: u32, revoked: bool) {
        issuer.require_auth();
        let word = subject_index / BITS_PER_WORD;
        let bit = subject_index % BITS_PER_WORD;
        let key = DataKey::RevocationWord(issuer.clone(), word);
        let mut value: u64 = env.storage().persistent().get(&key).unwrap_or(0);
        let mask = 1u64 << bit;
        if revoked { value |= mask; } else { value &= !mask; }
        env.storage().persistent().set(&key, &value);
    }
    pub fn is_revoked(env: &Env, issuer: &Address, subject_index: u32) -> bool {
        let value: u64 = env.storage().persistent().get(&DataKey::RevocationWord(issuer.clone(), subject_index / BITS_PER_WORD)).unwrap_or(0);
        value & (1u64 << (subject_index % BITS_PER_WORD)) != 0
    }
}
