//! Bonded M-of-N compliance oracle verification.
//!
//! Providers sign the SHA-256 digest of a domain-separated XDR payload. A
//! provider may cast a vote in a separate transaction, but its vote is only
//! effective once the configured threshold agrees on the same payload nonce.
//! Fresh, cryptographically valid attestations with invalid time bounds and
//! provable per-oracle equivocations are slashable. A bad Ed25519 signature
//! itself traps in the Soroban host, so it cannot be caught and slashed in the
//! same transaction; it is rejected without changing contract state.

use soroban_sdk::{
    contracttype, panic_with_error, token::TokenClient, xdr::ToXdr, Address, Bytes, BytesN, Env,
    String, Vec,
};

use crate::{ComplianceStatus, DataKey, Error, KycRecord};

const SIGNING_DOMAIN: &[u8] = b"TESSERA_COMPLIANCE_ORACLE_V1";
const BASIS_POINTS: i128 = 10_000;
const MAX_ORACLES: u32 = 32;
const MIN_INSTANCE_TTL: u32 = 100_000;
const TARGET_INSTANCE_TTL: u32 = 120_000;
const MIN_PERSISTENT_TTL: u32 = 100_000;
const TARGET_PERSISTENT_TTL: u32 = 120_000;

/// Canonical KYC/AML claim shared by the oracle signatures in one quorum.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleAttestation {
    pub investor: Address,
    pub status: ComplianceStatus,
    pub jurisdiction: String,
    pub verified_at: u32,
    /// Expiry ledger; zero means no time-based expiry.
    pub expires_at: u32,
    /// Must increase independently for each oracle and investor pair.
    pub nonce: u64,
}

/// One registered provider's Ed25519 signature over an OracleAttestation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleSignature {
    pub oracle: Address,
    pub signature: BytesN<64>,
}

/// On-chain quorum, bond, and slashing configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleConfig {
    pub staking_token: Address,
    pub slash_recipient: Address,
    pub threshold: u32,
    pub oracle_count: u32,
    pub minimum_stake: i128,
    pub slash_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
struct OracleVoteRecord {
    status: ComplianceStatus,
    jurisdiction: String,
    verified_at: u32,
    expires_at: u32,
    nonce: u64,
}

pub(crate) fn configure(
    env: &Env,
    staking_token: Address,
    slash_recipient: Address,
    threshold: u32,
    minimum_stake: i128,
    slash_bps: u32,
) {
    if env.storage().instance().has(&DataKey::OracleMode) {
        panic_with_error!(env, Error::OracleAlreadyConfigured);
    }
    if threshold == 0
        || minimum_stake <= 0
        || slash_bps == 0
        || slash_bps > BASIS_POINTS as u32
        || slash_recipient == env.current_contract_address()
    {
        panic_with_error!(env, Error::InvalidOracleConfig);
    }

    env.storage().instance().set(&DataKey::OracleToken, &staking_token);
    env.storage()
        .instance()
        .set(&DataKey::OracleSlashRecipient, &slash_recipient);
    env.storage()
        .instance()
        .set(&DataKey::OracleThreshold, &threshold);
    env.storage()
        .instance()
        .set(&DataKey::OracleMinStake, &minimum_stake);
    env.storage()
        .instance()
        .set(&DataKey::OracleSlashBps, &slash_bps);
    env.storage()
        .instance()
        .set(&DataKey::OracleList, &Vec::<Address>::new(env));
    env.storage().instance().set(&DataKey::OracleMode, &true);
    extend_instance_ttl(env);
}

pub(crate) fn set_threshold(env: &Env, threshold: u32) {
    let oracles = oracle_list(env);
    if threshold == 0 || threshold > oracles.len() {
        panic_with_error!(env, Error::InvalidOracleThreshold);
    }
    env.storage()
        .instance()
        .set(&DataKey::OracleThreshold, &threshold);
    extend_instance_ttl(env);
}

pub(crate) fn register(env: &Env, oracle: Address, public_key: BytesN<32>, stake_amount: i128) {
    let minimum_stake: i128 = required_config(env, &DataKey::OracleMinStake);
    if stake_amount < minimum_stake {
        panic_with_error!(env, Error::InvalidOracleStake);
    }
    let mut oracles = oracle_list(env);
    if oracles.len() >= MAX_ORACLES {
        panic_with_error!(env, Error::InvalidOracleThreshold);
    }
    if env
        .storage()
        .instance()
        .has(&DataKey::OracleKey(oracle.clone()))
    {
        panic_with_error!(env, Error::OracleAlreadyRegistered);
    }
    for registered in oracles.iter() {
        let registered_key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::OracleKey(registered))
            .unwrap();
        if registered_key == public_key {
            panic_with_error!(env, Error::OracleAlreadyRegistered);
        }
    }

    oracle.require_auth();
    let token: Address = required_config(env, &DataKey::OracleToken);
    TokenClient::new(env, &token).transfer(
        &oracle,
        &env.current_contract_address(),
        &stake_amount,
    );

    oracles.push_back(oracle.clone());
    env.storage().instance().set(&DataKey::OracleList, &oracles);
    env.storage()
        .instance()
        .set(&DataKey::OracleKey(oracle.clone()), &public_key);
    env.storage()
        .instance()
        .set(&DataKey::OracleStake(oracle), &stake_amount);
    extend_instance_ttl(env);
}

pub(crate) fn restake(env: &Env, oracle: Address, stake_amount: i128) {
    if stake_amount <= 0
        || !env
            .storage()
            .instance()
            .has(&DataKey::OracleKey(oracle.clone()))
    {
        panic_with_error!(env, Error::InvalidOracleStake);
    }
    oracle.require_auth();
    let token: Address = required_config(env, &DataKey::OracleToken);
    TokenClient::new(env, &token).transfer(
        &oracle,
        &env.current_contract_address(),
        &stake_amount,
    );

    let old_stake: i128 = env
        .storage()
        .instance()
        .get(&DataKey::OracleStake(oracle.clone()))
        .unwrap_or(0);
    let new_stake = old_stake
        .checked_add(stake_amount)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidOracleStake));
    env.storage()
        .instance()
        .set(&DataKey::OracleStake(oracle), &new_stake);
    extend_instance_ttl(env);
}

pub(crate) fn remove(env: &Env, oracle: Address) {
    let mut oracles = oracle_list(env);
    let index = oracles
        .iter()
        .position(|registered| registered == oracle)
        .unwrap_or_else(|| panic_with_error!(env, Error::OracleNotRegistered));
    let threshold: u32 = required_config(env, &DataKey::OracleThreshold);
    if oracles.len().saturating_sub(1) < threshold {
        panic_with_error!(env, Error::InvalidOracleThreshold);
    }

    let stake: i128 = env
        .storage()
        .instance()
        .get(&DataKey::OracleStake(oracle.clone()))
        .unwrap_or(0);
    if stake > 0 {
        let token: Address = required_config(env, &DataKey::OracleToken);
        TokenClient::new(env, &token).transfer(
            &env.current_contract_address(),
            &oracle,
            &stake,
        );
    }
    oracles.remove(index as u32);
    env.storage().instance().set(&DataKey::OracleList, &oracles);
    env.storage()
        .instance()
        .remove(&DataKey::OracleKey(oracle.clone()));
    env.storage()
        .instance()
        .remove(&DataKey::OracleStake(oracle));
    extend_instance_ttl(env);
}

/// Verify each fresh signature, persist its vote, and publish the claim only
/// when the same M-of-N set agrees on the claim and nonce.
pub(crate) fn submit(
    env: &Env,
    attestation: OracleAttestation,
    signatures: Vec<OracleSignature>,
) -> bool {
    let oracles = oracle_list(env);
    let threshold: u32 = required_config(env, &DataKey::OracleThreshold);
    let minimum_stake: i128 = required_config(env, &DataKey::OracleMinStake);
    if threshold == 0 || threshold > oracles.len() || signatures.is_empty() {
        return false;
    }

    let message = signing_message(env, &attestation);
    let mut verified = Vec::<Address>::new(env);
    for submitted in signatures.iter() {
        if verified.iter().any(|seen| seen == submitted.oracle) {
            return false;
        }
        if !oracles.iter().any(|oracle| oracle == submitted.oracle) {
            continue;
        }
        let stake: i128 = env
            .storage()
            .instance()
            .get(&DataKey::OracleStake(submitted.oracle.clone()))
            .unwrap_or(0);
        if stake < minimum_stake {
            continue;
        }
        let public_key: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::OracleKey(submitted.oracle.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::OracleNotRegistered));

        // The Soroban host traps on an invalid Ed25519 signature. That makes
        // the entire invocation atomic and prevents slashing unproven signers.
        env.crypto()
            .ed25519_verify(&public_key, &message, &submitted.signature);
        verified.push_back(submitted.oracle);
    }

    if verified.is_empty() {
        return false;
    }

    let now = env.ledger().sequence();
    let invalid_claim = attestation.nonce == 0
        || attestation.verified_at > now
        || (attestation.expires_at != 0 && attestation.expires_at <= now)
        || (attestation.status == ComplianceStatus::Approved
            && crate::ComplianceContract::is_jurisdiction_blocked(
                env.clone(),
                attestation.jurisdiction.clone(),
            ));
    for oracle in verified.iter() {
        let nonce_key = DataKey::OracleNonce(attestation.investor.clone(), oracle.clone());
        let last_nonce: Option<u64> = env.storage().persistent().get(&nonce_key);
        if let Some(last) = last_nonce {
            if attestation.nonce < last {
                continue;
            }
            if attestation.nonce == last {
                let vote_key = DataKey::OracleVote(attestation.investor.clone(), oracle.clone());
                let previous: Option<OracleVoteRecord> =
                    env.storage().persistent().get(&vote_key);
                if let Some(previous) = previous {
                    if previous.status != attestation.status
                        || previous.jurisdiction != attestation.jurisdiction
                        || previous.verified_at != attestation.verified_at
                        || previous.expires_at != attestation.expires_at
                    {
                        slash(env, &oracle);
                    }
                }
                continue;
            }
        }
        env.storage().persistent().set(&nonce_key, &attestation.nonce);
        env.storage().persistent().extend_ttl(
            &nonce_key,
            MIN_PERSISTENT_TTL,
            TARGET_PERSISTENT_TTL,
        );

        if invalid_claim {
            slash(env, &oracle);
            continue;
        }

        let vote_key = DataKey::OracleVote(attestation.investor.clone(), oracle.clone());

        let vote = OracleVoteRecord {
            status: attestation.status.clone(),
            jurisdiction: attestation.jurisdiction.clone(),
            verified_at: attestation.verified_at,
            expires_at: attestation.expires_at,
            nonce: attestation.nonce,
        };
        env.storage().persistent().set(&vote_key, &vote);
        env.storage().persistent().extend_ttl(
            &vote_key,
            MIN_PERSISTENT_TTL,
            TARGET_PERSISTENT_TTL,
        );
    }

    if invalid_claim {
        return false;
    }

    let mut agreeing = 0u32;
    for oracle in oracles.iter() {
        let stake: i128 = env
            .storage()
            .instance()
            .get(&DataKey::OracleStake(oracle.clone()))
            .unwrap_or(0);
        if stake < minimum_stake {
            continue;
        }
        let vote: Option<OracleVoteRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::OracleVote(attestation.investor.clone(), oracle));
        if let Some(vote) = vote {
            if vote.nonce == attestation.nonce
                && vote.status == attestation.status
                && vote.jurisdiction == attestation.jurisdiction
                && vote.verified_at == attestation.verified_at
                && vote.expires_at == attestation.expires_at
            {
                agreeing += 1;
            }
        }
    }
    if agreeing < threshold {
        return false;
    }

    let record = KycRecord {
        address: attestation.investor.clone(),
        status: attestation.status,
        jurisdiction: attestation.jurisdiction,
        verified_at: attestation.verified_at,
        expires_at: attestation.expires_at,
    };
    let record_key = DataKey::OracleRecord(attestation.investor);
    env.storage().persistent().set(&record_key, &record);
    env.storage().persistent().extend_ttl(
        &record_key,
        MIN_PERSISTENT_TTL,
        TARGET_PERSISTENT_TTL,
    );
    extend_instance_ttl(env);
    true
}

pub(crate) fn verify(env: &Env, investor: &Address) -> bool {
    let record = match get_record(env, investor) {
        Some(record) => record,
        None => return false,
    };
    record.status == ComplianceStatus::Approved
        && (record.expires_at == 0 || env.ledger().sequence() < record.expires_at)
        && !crate::ComplianceContract::is_jurisdiction_blocked(
            env.clone(),
            record.jurisdiction,
        )
}

pub(crate) fn oracle_mode_enabled(env: &Env) -> bool {
    env.storage().instance().get(&DataKey::OracleMode).unwrap_or(false)
}

pub(crate) fn get_record(env: &Env, investor: &Address) -> Option<KycRecord> {
    let key = DataKey::OracleRecord(investor.clone());
    let record = env.storage().persistent().get(&key);
    if record.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, MIN_PERSISTENT_TTL, TARGET_PERSISTENT_TTL);
    }
    record
}

pub(crate) fn get_config(env: &Env) -> Option<OracleConfig> {
    if !oracle_mode_enabled(env) {
        return None;
    }
    Some(OracleConfig {
        staking_token: required_config(env, &DataKey::OracleToken),
        slash_recipient: required_config(env, &DataKey::OracleSlashRecipient),
        threshold: required_config(env, &DataKey::OracleThreshold),
        oracle_count: oracle_list(env).len(),
        minimum_stake: required_config(env, &DataKey::OracleMinStake),
        slash_bps: required_config(env, &DataKey::OracleSlashBps),
    })
}

fn slash(env: &Env, oracle: &Address) {
    let stake_key = DataKey::OracleStake(oracle.clone());
    let stake: i128 = env.storage().instance().get(&stake_key).unwrap_or(0);
    if stake <= 0 {
        return;
    }
    let slash_bps: u32 = required_config(env, &DataKey::OracleSlashBps);
    let mut penalty = (stake / BASIS_POINTS) * slash_bps as i128
        + ((stake % BASIS_POINTS) * slash_bps as i128) / BASIS_POINTS;
    if penalty == 0 && slash_bps > 0 {
        penalty = 1;
    }
    penalty = penalty.min(stake);
    let token: Address = required_config(env, &DataKey::OracleToken);
    let recipient: Address = required_config(env, &DataKey::OracleSlashRecipient);
    TokenClient::new(env, &token).transfer(
        &env.current_contract_address(),
        &recipient,
        &penalty,
    );
    env.storage()
        .instance()
        .set(&stake_key, &(stake - penalty));
}

fn signing_message(env: &Env, attestation: &OracleAttestation) -> Bytes {
    let mut payload = Bytes::from_slice(env, SIGNING_DOMAIN);
    payload.append(&attestation.investor.clone().to_xdr(env));
    payload.append(&attestation.status.clone().to_xdr(env));
    payload.append(&attestation.jurisdiction.clone().to_xdr(env));
    payload.append(&Bytes::from_array(env, &attestation.verified_at.to_be_bytes()));
    payload.append(&Bytes::from_array(env, &attestation.expires_at.to_be_bytes()));
    payload.append(&Bytes::from_array(env, &attestation.nonce.to_be_bytes()));
    env.crypto().sha256(&payload).into()
}

fn oracle_list(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::OracleList)
        .unwrap_or(Vec::new(env))
}

fn required_config<T>(env: &Env, key: &DataKey) -> T
where
    T: soroban_sdk::TryFromVal<Env, soroban_sdk::Val>,
{
    env.storage()
        .instance()
        .get(key)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidOracleConfig))
}

fn extend_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(MIN_INSTANCE_TTL, TARGET_INSTANCE_TTL);
}