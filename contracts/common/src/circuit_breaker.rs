//! Automated Emergency Liquidation Circuit Breaker & Safe-Mode Vault (Issue #94).
//!
//! Implements an anomaly-detecting circuit breaker that automatically transitions
//! into `SafeMode` when statistical parameters (e.g. transfer volume, asset price,
//! supply changes) deviate beyond 3 standard deviations (3σ) from the running mean.
//!
//! When `SafeMode` is triggered:
//! 1. High-risk actions (e.g. large transfers, liquidations, new mints) are halted.
//! 2. Detailed audit logs are permanently recorded with deviation telemetry.
//! 3. Resetting the circuit breaker requires multi-signature admin authorization
//!    satisfying an M-of-N signature threshold.
//!
//! # Algorithmic & Statistical Design
//! - Uses Welford's online algorithm for computing exact rolling sample mean and variance
//!   in O(1) time and O(1) space per observation.
//! - Checks the 3σ anomaly condition without floating-point arithmetic or lossy square roots:
//!   `|x - μ| > 3σ  <=>  (x - μ)² > 9 * σ²  <=>  N * (x - μ)² > 9 * M2`.
//!
//! # Time & Space Complexity
//! - Observation Update & 3σ Check: O(1) time, O(1) space.
//! - Multi-sig Reset Verification: O(M * K) where M <= K <= 10 (effective O(1)), O(1) space.
//! - Storage: Instance/Persistent key-value slots.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Symbol, Vec,
};

/// Error codes for Circuit Breaker operations.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum CircuitBreakerError {
    AlreadyInitialized = 101,
    NotInitialized = 102,
    CircuitBreakerTripped = 103,
    InSafeMode = 104,
    UnauthorizedAdmin = 105,
    InsufficientSignatures = 106,
    DuplicateSignatures = 107,
    InvalidThreshold = 108,
    NoAdminsProvided = 109,
    ArithmeticOverflow = 110,
}

/// Operational state of the circuit breaker.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CircuitBreakerState {
    /// Normal operations permitted.
    Normal = 0,
    /// Safe mode active: high-risk operations frozen.
    SafeMode = 1,
}

/// Statistical accumulator tracking rolling mean and variance using Welford's algorithm.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetricTracker {
    /// Number of observations recorded.
    pub count: u64,
    /// Running mean (scaled in base units / fixed point).
    pub mean: i128,
    /// Sum of squared differences from the mean (M2 = Σ(x_i - μ)²).
    pub m2: i128,
    /// Minimum samples required before applying 3σ anomaly checks.
    pub min_samples: u32,
}

impl MetricTracker {
    pub fn new(min_samples: u32) -> Self {
        Self {
            count: 0,
            mean: 0,
            m2: 0,
            min_samples,
        }
    }

    /// Tests if a new observation deviates beyond 3 standard deviations (3σ).
    /// Condition: (x - μ)² > 9 * σ²  <=>  count * (x - μ)² > 9 * M2.
    pub fn is_3sigma_anomaly(&self, value: i128) -> bool {
        if self.count < self.min_samples as u64 || self.count == 0 {
            return false;
        }

        let diff = value.saturating_sub(self.mean);
        let diff_squared = diff.saturating_mul(diff);
        let n = self.count as i128;

        // 9 * σ² = 9 * (m2 / n) => n * diff² > 9 * m2
        let lhs = n.saturating_mul(diff_squared);
        let rhs = 9i128.saturating_mul(self.m2);

        if self.m2 == 0 {
            diff != 0
        } else {
            lhs > rhs
        }
    }

    /// Updates the rolling accumulator with a new observation.
    /// Time Complexity: O(1), Space Complexity: O(1).
    pub fn update(&mut self, value: i128) {
        self.count = self.count.saturating_add(1);
        let delta1 = value.saturating_sub(self.mean);
        let step = delta1 / (self.count as i128);
        self.mean = self.mean.saturating_add(step);
        let delta2 = value.saturating_sub(self.mean);
        let product = delta1.saturating_mul(delta2);
        self.m2 = self.m2.saturating_add(product);
    }
}

/// Audit log record created whenever SafeMode is triggered or reset.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLogEntry {
    /// Ledger sequence when the event occurred.
    pub ledger_sequence: u32,
    /// Ledger timestamp.
    pub timestamp: u64,
    /// Reason or trigger type (e.g. "vol_3sig", "price_3sig", "reset").
    pub action: Symbol,
    /// Value that caused the anomaly or context data.
    pub observed_value: i128,
    /// Mean at time of observation.
    pub expected_mean: i128,
}

#[contractevent]
pub struct SafeModeTriggered {
    #[topic]
    pub action: Symbol,
    pub observed_value: i128,
    pub expected_mean: i128,
}

#[contractevent]
pub struct CircuitBreakerReset {
    pub signers_count: u32,
    pub threshold: u32,
}

/// Comprehensive status of the circuit breaker.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CircuitBreakerStatus {
    pub state: CircuitBreakerState,
    pub volume_tracker: MetricTracker,
    pub price_tracker: MetricTracker,
    pub supply_tracker: MetricTracker,
    pub admin_count: u32,
    pub required_threshold: u32,
    pub total_trips: u32,
}

// ── Storage Keys ────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
enum DataKey {
    State,
    Admins,
    Threshold,
    VolTracker,
    PriceTracker,
    SupplyTracker,
    AuditLogs,
    TripCount,
}

#[contract]
pub struct CircuitBreakerContract;

#[contractimpl]
impl CircuitBreakerContract {
    /// Initialize the circuit breaker with multi-signature admin rules and sample requirements.
    pub fn initialize(
        env: Env,
        admins: Vec<Address>,
        threshold: u32,
        min_samples: u32,
    ) {
        if env.storage().instance().has(&DataKey::State) {
            panic_with_error!(env, CircuitBreakerError::AlreadyInitialized);
        }
        if admins.is_empty() {
            panic_with_error!(env, CircuitBreakerError::NoAdminsProvided);
        }
        if threshold == 0 || threshold > admins.len() {
            panic_with_error!(env, CircuitBreakerError::InvalidThreshold);
        }

        env.storage()
            .instance()
            .set(&DataKey::State, &CircuitBreakerState::Normal);
        env.storage().instance().set(&DataKey::Admins, &admins);
        env.storage().instance().set(&DataKey::Threshold, &threshold);
        env.storage()
            .instance()
            .set(&DataKey::VolTracker, &MetricTracker::new(min_samples));
        env.storage()
            .instance()
            .set(&DataKey::PriceTracker, &MetricTracker::new(min_samples));
        env.storage()
            .instance()
            .set(&DataKey::SupplyTracker, &MetricTracker::new(min_samples));
        env.storage()
            .instance()
            .set(&DataKey::AuditLogs, &Vec::<AuditLogEntry>::new(&env));
        env.storage().instance().set(&DataKey::TripCount, &0u32);
    }

    /// Checks whether the circuit breaker is currently in `SafeMode`.
    pub fn is_in_safe_mode(env: Env) -> bool {
        let state: CircuitBreakerState = env
            .storage()
            .instance()
            .get(&DataKey::State)
            .unwrap_or(CircuitBreakerState::Normal);
        state == CircuitBreakerState::SafeMode
    }

    /// Asserts that the circuit breaker is NOT in safe mode.
    pub fn assert_not_in_safe_mode(env: Env) {
        if Self::is_in_safe_mode(env.clone()) {
            panic_with_error!(env, CircuitBreakerError::InSafeMode);
        }
    }

    /// Records a transfer volume observation and triggers SafeMode if anomaly > 3σ is detected.
    pub fn observe_volume(env: Env, volume: i128) -> bool {
        let mut tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::VolTracker)
            .unwrap_or(MetricTracker::new(5));

        if tracker.is_3sigma_anomaly(volume) {
            Self::trigger_safe_mode(&env, symbol_short!("vol_3sig"), volume, tracker.mean);
            return false;
        }

        tracker.update(volume);
        env.storage().instance().set(&DataKey::VolTracker, &tracker);
        true
    }

    /// Records a price observation and triggers SafeMode if flash-crash or anomaly > 3σ is detected.
    pub fn observe_price(env: Env, price: i128) -> bool {
        let mut tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::PriceTracker)
            .unwrap_or(MetricTracker::new(5));

        if tracker.is_3sigma_anomaly(price) {
            Self::trigger_safe_mode(&env, symbol_short!("prc_3sig"), price, tracker.mean);
            return false;
        }

        tracker.update(price);
        env.storage().instance().set(&DataKey::PriceTracker, &tracker);
        true
    }

    /// Records a supply delta observation and triggers SafeMode if anomaly > 3σ is detected.
    pub fn observe_supply_delta(env: Env, delta: i128) -> bool {
        let mut tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::SupplyTracker)
            .unwrap_or(MetricTracker::new(5));

        if tracker.is_3sigma_anomaly(delta) {
            Self::trigger_safe_mode(&env, symbol_short!("sup_3sig"), delta, tracker.mean);
            return false;
        }

        tracker.update(delta);
        env.storage().instance().set(&DataKey::SupplyTracker, &tracker);
        true
    }

    /// Resets the circuit breaker from `SafeMode` back to `Normal`.
    /// Requires M-of-N multi-signature admin authorization.
    pub fn reset_circuit_breaker(
        env: Env,
        signers: Vec<Address>,
    ) {
        let admins: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Admins)
            .unwrap_or_else(|| panic_with_error!(env, CircuitBreakerError::NotInitialized));
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);

        if signers.len() < threshold {
            panic_with_error!(env, CircuitBreakerError::InsufficientSignatures);
        }

        let mut verified_count = 0u32;
        let mut seen_signers = Vec::<Address>::new(&env);

        for signer in signers.iter() {
            signer.require_auth();

            if seen_signers.iter().any(|s| s == signer) {
                panic_with_error!(env, CircuitBreakerError::DuplicateSignatures);
            }
            seen_signers.push_back(signer.clone());

            if !admins.iter().any(|a| a == signer) {
                panic_with_error!(env, CircuitBreakerError::UnauthorizedAdmin);
            }

            verified_count = verified_count.saturating_add(1);
        }

        if verified_count < threshold {
            panic_with_error!(env, CircuitBreakerError::InsufficientSignatures);
        }

        env.storage()
            .instance()
            .set(&DataKey::State, &CircuitBreakerState::Normal);

        let entry = AuditLogEntry {
            ledger_sequence: env.ledger().sequence(),
            timestamp: env.ledger().timestamp(),
            action: symbol_short!("cb_reset"),
            observed_value: verified_count as i128,
            expected_mean: threshold as i128,
        };

        let mut logs: Vec<AuditLogEntry> = env
            .storage()
            .instance()
            .get(&DataKey::AuditLogs)
            .unwrap_or(Vec::new(&env));
        logs.push_back(entry);
        env.storage().instance().set(&DataKey::AuditLogs, &logs);

        CircuitBreakerReset {
            signers_count: verified_count,
            threshold,
        }
        .publish(&env);
    }

    /// Retrieve all recorded audit logs.
    pub fn get_audit_logs(env: Env) -> Vec<AuditLogEntry> {
        env.storage()
            .instance()
            .get(&DataKey::AuditLogs)
            .unwrap_or(Vec::new(&env))
    }

    /// Retrieve the full status of the circuit breaker and all metric trackers.
    pub fn get_circuit_breaker_status(env: Env) -> CircuitBreakerStatus {
        let state: CircuitBreakerState = env
            .storage()
            .instance()
            .get(&DataKey::State)
            .unwrap_or(CircuitBreakerState::Normal);
        let vol_tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::VolTracker)
            .unwrap_or(MetricTracker::new(5));
        let price_tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::PriceTracker)
            .unwrap_or(MetricTracker::new(5));
        let supply_tracker: MetricTracker = env
            .storage()
            .instance()
            .get(&DataKey::SupplyTracker)
            .unwrap_or(MetricTracker::new(5));
        let admins: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Admins)
            .unwrap_or(Vec::new(&env));
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(0);
        let total_trips: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TripCount)
            .unwrap_or(0);

        CircuitBreakerStatus {
            state,
            volume_tracker: vol_tracker,
            price_tracker,
            supply_tracker,
            admin_count: admins.len(),
            required_threshold: threshold,
            total_trips,
        }
    }

    fn trigger_safe_mode(env: &Env, action: Symbol, observed_value: i128, expected_mean: i128) {
        env.storage()
            .instance()
            .set(&DataKey::State, &CircuitBreakerState::SafeMode);

        let trip_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TripCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TripCount, &trip_count.saturating_add(1));

        let entry = AuditLogEntry {
            ledger_sequence: env.ledger().sequence(),
            timestamp: env.ledger().timestamp(),
            action: action.clone(),
            observed_value,
            expected_mean,
        };

        let mut logs: Vec<AuditLogEntry> = env
            .storage()
            .instance()
            .get(&DataKey::AuditLogs)
            .unwrap_or(Vec::new(env));
        logs.push_back(entry);
        env.storage().instance().set(&DataKey::AuditLogs, &logs);

        SafeModeTriggered {
            action,
            observed_value,
            expected_mean,
        }
        .publish(env);
    }
}

// ── Standalone Helper Functions for In-Contract Callers ────────────────────

pub fn is_in_safe_mode(env: &Env) -> bool {
    CircuitBreakerContract::is_in_safe_mode(env.clone())
}

pub fn assert_not_in_safe_mode(env: &Env) {
    CircuitBreakerContract::assert_not_in_safe_mode(env.clone());
}

pub fn observe_volume(env: &Env, volume: i128) -> bool {
    CircuitBreakerContract::observe_volume(env.clone(), volume)
}

pub fn observe_price(env: &Env, price: i128) -> bool {
    CircuitBreakerContract::observe_price(env.clone(), price)
}

pub fn observe_supply_delta(env: &Env, delta: i128) -> bool {
    CircuitBreakerContract::observe_supply_delta(env.clone(), delta)
}

pub fn get_audit_logs(env: &Env) -> Vec<AuditLogEntry> {
    CircuitBreakerContract::get_audit_logs(env.clone())
}

pub fn get_circuit_breaker_status(env: &Env) -> CircuitBreakerStatus {
    CircuitBreakerContract::get_circuit_breaker_status(env.clone())
}

// ── Unit Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn test_welford_accumulator_convergence() {
        let mut tracker = MetricTracker::new(5);
        let samples = [100i128, 102, 98, 101, 99];
        for s in samples.iter() {
            tracker.update(*s);
        }

        assert_eq!(tracker.count, 5);
        assert_eq!(tracker.mean, 100);
        assert!(tracker.is_3sigma_anomaly(1000));
        assert!(!tracker.is_3sigma_anomaly(101));
    }

    #[test]
    fn test_exact_integer_3sigma_boundary() {
        let mut tracker = MetricTracker::new(5);
        tracker.count = 10;
        tracker.mean = 100;
        tracker.m2 = 100;

        assert!(!tracker.is_3sigma_anomaly(108));
        assert!(tracker.is_3sigma_anomaly(115));
    }

    #[test]
    fn test_circuit_breaker_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(CircuitBreakerContract, ());
        let client = CircuitBreakerContractClient::new(&env, &contract_id);

        let admin1 = Address::generate(&env);
        let admin2 = Address::generate(&env);
        let admin3 = Address::generate(&env);

        let mut admins = Vec::new(&env);
        admins.push_back(admin1.clone());
        admins.push_back(admin2.clone());
        admins.push_back(admin3.clone());

        // Init with 2-of-3 threshold
        client.initialize(&admins, &2, &5);

        assert!(!client.is_in_safe_mode());
        client.assert_not_in_safe_mode();

        // Feed normal observations
        for v in [1000, 1020, 980, 1010, 990] {
            assert!(client.observe_volume(&v));
        }
        assert!(!client.is_in_safe_mode());

        // Trigger anomaly spike (> 3σ)
        let normal = client.observe_volume(&50_000);
        assert!(!normal);
        assert!(client.is_in_safe_mode());

        // Check audit logs
        let logs = client.get_audit_logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs.get(0).unwrap().action, symbol_short!("vol_3sig"));

        // Reset with 2 valid signers should succeed
        let mut valid_signers = Vec::new(&env);
        valid_signers.push_back(admin1);
        valid_signers.push_back(admin2);
        client.reset_circuit_breaker(&valid_signers);

        assert!(!client.is_in_safe_mode());
        client.assert_not_in_safe_mode();

        // Verify status
        let status = client.get_circuit_breaker_status();
        assert_eq!(status.state, CircuitBreakerState::Normal);
        assert_eq!(status.total_trips, 1);
    }
}
