//! Cap-table snapshot & Merkle proof verification contract (issue #13).
//!
//! This is new functionality, not a reconstruction of an already-deployed
//! contract — there is no `cap-table` entry among the four documented,
//! deployed contracts. It exists so distributing dividends (or otherwise
//! proving a holder's balance) to thousands of holders doesn't require
//! putting every holder's balance on-chain: an admin periodically publishes
//! a Merkle root committing to the full holder set for an asset token, and
//! any holder can then prove their balance against that root with an
//! `O(log n)`-sized proof instead of an on-chain enumeration.
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short,
    xdr::ToXdr, Address, Bytes, BytesN, Env, Vec,
};

#[contracttype]
#[derive(Clone)]
pub struct Snapshot {
    pub id: u64,
    pub asset_token: Address,
    pub merkle_root: BytesN<32>,
    pub ledger: u32,
    pub total_holders: u32,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    SnapshotNotFound = 4,
    UnauthorizedRegistrar = 5,
    InvalidAmount = 6,
}

#[derive(Clone)]
#[contracttype]
enum DataKey {
    Admin,
    NextId,
    Snapshot(u64),
    DripRegistrar,
    DripAcquisition(Address, Address),
    Acquisitions(Address, Address),
    Affiliate(Address, Address),
    DailyVolume(Address, u32),
}

/// Ledgers per day at the Stellar close time of ~5 s per ledger.
pub const LEDGERS_PER_DAY: u32 = 17_280;
/// Rule 144 minimum holding period for non-affiliates: 12 months (365 days).
pub const RULE144_HOLDING_LEDGERS: u32 = 365 * LEDGERS_PER_DAY;
/// Rolling window used for average daily trading volume (ADTV).
pub const ADTV_WINDOW_DAYS: u32 = 90;

/// Restriction regime under which a lot was acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[contracttype]
pub enum RestrictionType {
    Rule144,
    RegS,
    RegD,
}

/// One acquisition lot: `amount` units acquired at `acquisition_ledger`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct Acquisition {
    pub amount: i128,
    pub acquisition_ledger: u32,
    pub restriction: RestrictionType,
}

/// Signed-off Rule 144 status for one holder, for submission to the transfer agent.
#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct Rule144Attestation {
    pub asset_token: Address,
    pub holder: Address,
    pub ledger: u32,
    pub affiliate: bool,
    pub aged_amount: i128,
    pub adtv: i128,
    pub freely_tradeable: bool,
}

#[contract]
pub struct CapTableContract;

#[contractimpl]
impl CapTableContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Record a new cap-table snapshot for `asset_token`: `merkle_root`
    /// commits (via [`Self::verify_holder`]) to every holder's balance at
    /// the current ledger.
    pub fn submit_snapshot(
        env: Env,
        admin: Address,
        asset_token: Address,
        merkle_root: BytesN<32>,
        total_holders: u32,
    ) -> u64 {
        Self::require_admin(&env, &admin);

        let id: u64 = env.storage().instance().get(&DataKey::NextId).unwrap();
        env.storage().instance().set(&DataKey::NextId, &(id + 1));

        let snapshot = Snapshot {
            id,
            asset_token: asset_token.clone(),
            merkle_root,
            ledger: env.ledger().sequence(),
            total_holders,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Snapshot(id), &snapshot);

        env.events()
            .publish((symbol_short!("snapshot"), admin), (id, asset_token));
        id
    }

    pub fn get_snapshot(env: Env, id: u64) -> Snapshot {
        env.storage()
            .persistent()
            .get(&DataKey::Snapshot(id))
            .unwrap_or_else(|| panic_with_error!(env, Error::SnapshotNotFound))
    }

    /// Set the dividend contract allowed to record completed DRIP purchases.
    pub fn set_drip_registrar(env: Env, admin: Address, registrar: Address) {
        Self::require_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::DripRegistrar, &registrar);
        env.events()
            .publish((symbol_short!("dripreg"), admin), registrar);
    }

    /// Record asset tokens delivered to a holder by a DRIP swap. This is a
    /// cumulative audit ledger; token balances remain authoritative. The
    /// configured dividend contract is the only permitted caller.
    pub fn record_drip_acquisition(env: Env, asset_token: Address, holder: Address, amount: i128) {
        let registrar: Address = env
            .storage()
            .instance()
            .get(&DataKey::DripRegistrar)
            .unwrap_or_else(|| panic_with_error!(env, Error::UnauthorizedRegistrar));
        registrar.require_auth();
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let key = DataKey::DripAcquisition(asset_token.clone(), holder.clone());
        let old: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new = old
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount));
        env.storage().persistent().set(&key, &new);
        env.events().publish(
            (symbol_short!("dripbuy"), asset_token, holder),
            (amount, new),
        );
    }

    pub fn drip_acquisitions(env: Env, asset_token: Address, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::DripAcquisition(asset_token, holder))
            .unwrap_or(0)
    }

    /// Verify that `holder` held `balance` at `snapshot_id`'s recorded
    /// Merkle root.
    ///
    /// The leaf is `sha256(holder XDR bytes ++ balance big-endian bytes)`.
    /// The proof is then folded bottom-up to the root using SORTED-PAIR
    /// hashing at each level (`sha256(min(a, b) ++ max(a, b))`) rather than
    /// naive left/right concatenation, so a verifier does not need to also
    /// carry left/right-branch metadata, and the tree is not vulnerable to
    /// the classic ambiguous-ordering second-preimage trick. Returns
    /// `false` (never panics) for an unknown snapshot or a proof that
    /// doesn't fold to the stored root.
    pub fn verify_holder(
        env: Env,
        snapshot_id: u64,
        holder: Address,
        balance: i128,
        proof: Vec<BytesN<32>>,
    ) -> bool {
        let snapshot: Option<Snapshot> = env
            .storage()
            .persistent()
            .get(&DataKey::Snapshot(snapshot_id));
        let snapshot = match snapshot {
            Some(s) => s,
            None => return false,
        };

        let mut leaf_input = Bytes::new(&env);
        leaf_input.append(&holder.to_xdr(&env));
        leaf_input.append(&Bytes::from_array(&env, &balance.to_be_bytes()));
        let mut node: BytesN<32> = env.crypto().sha256(&leaf_input).into();

        for sibling in proof.iter() {
            node = Self::hash_pair(&env, &node, &sibling);
        }

        node == snapshot.merkle_root
    }

    /// Record an acquisition lot for `holder` (admin only).
    pub fn record_acquisition(
        env: Env,
        admin: Address,
        asset_token: Address,
        holder: Address,
        amount: i128,
        restriction: RestrictionType,
    ) {
        Self::require_admin(&env, &admin);
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let key = DataKey::Acquisitions(asset_token, holder);
        let mut lots: Vec<Acquisition> = env.storage().persistent().get(&key).unwrap_or(Vec::new(&env));
        lots.push_back(Acquisition {
            amount,
            acquisition_ledger: env.ledger().sequence(),
            restriction,
        });
        env.storage().persistent().set(&key, &lots);
    }

    /// Set affiliate status for `holder` (admin only). Affiliate status is
    /// read on every call to [`Self::is_freely_tradeable`], so a change takes
    /// effect immediately; there is no cached result to invalidate.
    pub fn set_affiliate(env: Env, admin: Address, asset_token: Address, holder: Address, is_affiliate: bool) {
        Self::require_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::Affiliate(asset_token.clone(), holder.clone()), &is_affiliate);
        env.events()
            .publish((symbol_short!("affil"), asset_token, holder), is_affiliate);
    }

    /// Add `amount` traded volume to the current day's bucket (admin only).
    pub fn record_volume(env: Env, admin: Address, asset_token: Address, amount: i128) {
        Self::require_admin(&env, &admin);
        if amount < 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let day = env.ledger().sequence() / LEDGERS_PER_DAY;
        let key = DataKey::DailyVolume(asset_token, day);
        let old: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new = old
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount));
        env.storage().persistent().set(&key, &new);
    }

    /// Average daily trading volume over the last [`ADTV_WINDOW_DAYS`] days.
    pub fn adtv(env: Env, asset_token: Address) -> i128 {
        let today = env.ledger().sequence() / LEDGERS_PER_DAY;
        let mut total: i128 = 0;
        for offset in 0..ADTV_WINDOW_DAYS {
            if offset > today {
                break;
            }
            let day = today - offset;
            let v: i128 = env
                .storage()
                .persistent()
                .get(&DataKey::DailyVolume(asset_token.clone(), day))
                .unwrap_or(0);
            total = total.saturating_add(v);
        }
        total / i128::from(ADTV_WINDOW_DAYS)
    }

    /// Whether `holder` may sell `amount` units of `asset_token` now.
    ///
    /// Requires: the holder is not an affiliate; lots held at least
    /// [`RULE144_HOLDING_LEDGERS`] cover `amount`; and `amount` does not exceed
    /// ADTV.
    pub fn is_freely_tradeable(env: Env, asset_token: Address, holder: Address, amount: i128) -> bool {
        if amount <= 0 {
            return false;
        }
        let affiliate: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Affiliate(asset_token.clone(), holder.clone()))
            .unwrap_or(false);
        if affiliate {
            return false;
        }
        let aged = Self::aged_amount(&env, &asset_token, &holder);
        let adtv = Self::adtv(env.clone(), asset_token);
        aged >= amount && amount <= adtv
    }

    /// Produce a Rule 144 attestation for `holder` (admin only). The returned
    /// struct is the payload brokers submit to the transfer agent; it is also
    /// published as an event so the attestation is verifiable from ledger history.
    pub fn generate_144_attestation(
        env: Env,
        admin: Address,
        asset_token: Address,
        holder: Address,
    ) -> Rule144Attestation {
        Self::require_admin(&env, &admin);
        let affiliate: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Affiliate(asset_token.clone(), holder.clone()))
            .unwrap_or(false);
        let aged_amount = Self::aged_amount(&env, &asset_token, &holder);
        let adtv = Self::adtv(env.clone(), asset_token.clone());
        let attestation = Rule144Attestation {
            asset_token: asset_token.clone(),
            holder: holder.clone(),
            ledger: env.ledger().sequence(),
            affiliate,
            aged_amount,
            adtv,
            freely_tradeable: !affiliate && aged_amount > 0 && aged_amount <= adtv,
        };
        env.events().publish(
            (symbol_short!("r144att"), asset_token, holder),
            attestation.clone(),
        );
        attestation
    }

    /// Sum of lots held for at least the Rule 144 holding period.
    fn aged_amount(env: &Env, asset_token: &Address, holder: &Address) -> i128 {
        let lots: Vec<Acquisition> = env
            .storage()
            .persistent()
            .get(&DataKey::Acquisitions(asset_token.clone(), holder.clone()))
            .unwrap_or(Vec::new(env));
        let now = env.ledger().sequence();
        let mut total: i128 = 0;
        for lot in lots.iter() {
            if now.saturating_sub(lot.acquisition_ledger) >= RULE144_HOLDING_LEDGERS {
                total = total.saturating_add(lot.amount);
            }
        }
        total
    }

    /// Current contract ABI version, polled by the off-chain indexer.
    pub fn version(_env: Env) -> u64 {
        1
    }

    // ---- internal ----

    fn require_admin(env: &Env, admin: &Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        if admin != &stored_admin {
            panic_with_error!(env, Error::Unauthorized);
        }
    }

    /// `sha256(min(a, b) ++ max(a, b))`, ordered by raw byte value so that
    /// the same pair of siblings always folds to the same parent regardless
    /// of which side of the tree each one came from.
    fn hash_pair(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
        let (lo, hi) = if a.to_array() <= b.to_array() {
            (a, b)
        } else {
            (b, a)
        };
        let mut combined = Bytes::new(env);
        combined.append(&Bytes::from(lo.clone()));
        combined.append(&Bytes::from(hi.clone()));
        env.crypto().sha256(&combined).into()
    }
}

#[cfg(test)]
mod rule144_tests {
    use super::*;
    use soroban_sdk::{testutils::{Address as _, Ledger as _}, Env};

    fn setup() -> (Env, CapTableContractClient<'static>, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(CapTableContract, ());
        let client = CapTableContractClient::new(&env, &id);
        let admin = Address::generate(&env);
        let asset = Address::generate(&env);
        let holder = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin, asset, holder)
    }

    #[test]
    fn purchase_then_wait_becomes_freely_tradeable() {
        let (env, client, admin, asset, holder) = setup();
        client.record_acquisition(&admin, &asset, &holder, &1_000, &RestrictionType::Rule144);
        assert!(!client.is_freely_tradeable(&asset, &holder, &1_000));

        env.ledger().with_mut(|l| l.sequence_number += RULE144_HOLDING_LEDGERS);
        client.record_volume(&admin, &asset, &(1_000 * i128::from(ADTV_WINDOW_DAYS)));
        assert!(client.is_freely_tradeable(&asset, &holder, &1_000));
        assert!(!client.is_freely_tradeable(&asset, &holder, &1_001));
    }

    #[test]
    fn affiliate_is_never_freely_tradeable_and_change_applies_immediately() {
        let (env, client, admin, asset, holder) = setup();
        client.record_acquisition(&admin, &asset, &holder, &500, &RestrictionType::RegD);
        env.ledger().with_mut(|l| l.sequence_number += RULE144_HOLDING_LEDGERS);
        client.record_volume(&admin, &asset, &(500 * i128::from(ADTV_WINDOW_DAYS)));
        assert!(client.is_freely_tradeable(&asset, &holder, &500));

        client.set_affiliate(&admin, &asset, &holder, &true);
        assert!(!client.is_freely_tradeable(&asset, &holder, &500));
    }

    #[test]
    fn attestation_reflects_current_state() {
        let (env, client, admin, asset, holder) = setup();
        client.record_acquisition(&admin, &asset, &holder, &200, &RestrictionType::Rule144);
        env.ledger().with_mut(|l| l.sequence_number += RULE144_HOLDING_LEDGERS);
        client.record_volume(&admin, &asset, &(200 * i128::from(ADTV_WINDOW_DAYS)));
        let att = client.generate_144_attestation(&admin, &asset, &holder);
        assert!(att.freely_tradeable);
        assert_eq!(att.aged_amount, 200);
        assert_eq!(att.adtv, 200);
        assert!(!att.affiliate);
    }
}
