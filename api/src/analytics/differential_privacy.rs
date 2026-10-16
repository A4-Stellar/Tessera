//! Differential privacy for aggregate analytics endpoints (issue #297).
//!
//! `AssetAnalytics` (see [`super::velocity`]) reports holder-concentration
//! metrics (Gini, HHI) and transfer velocity computed directly from the
//! holder snapshot. Reported exactly, a sequence of narrow, repeated queries
//! (e.g. "recompute HHI after excluding holder X" for every X) can recover
//! individual balances by difference. This module adds calibrated noise so
//! that no single query's output reveals whether any one holder's data was
//! included.
//!
//! # Mechanism
//!
//! Implements the classic Laplace and Gaussian mechanisms for a numeric
//! query of bounded sensitivity `Δ` (the maximum amount one holder's
//! presence/absence can change the output):
//!
//! * **Laplace**: adds noise drawn from `Laplace(0, Δ/ε)`. Gives
//!   `ε`-differential privacy exactly, for any `ε > 0`.
//! * **Gaussian**: adds noise drawn from `Normal(0, σ)` with
//!   `σ = Δ·√(2·ln(1.25/δ))/ε`. Gives `(ε, δ)`-differential privacy for
//!   `0 < ε, 0 < δ < 1` (Dwork & Roth, *The Algorithmic Foundations of
//!   Differential Privacy*, Theorem 3.22).
//!
//! Composing `k` independent queries against the same privacy budget costs
//! `k·ε` under basic composition; this module tracks that bound via
//! [`PrivacyBudget::charge`] so a caller cannot silently exceed the budget
//! it configured by issuing many queries.

use rand::Rng;

/// Configurable privacy parameters for one query or one budgeted session.
///
/// `epsilon` is required and must be strictly positive: it is the privacy
/// loss per query, smaller is more private and noisier. `delta` is only
/// meaningful for the Gaussian mechanism (the per-query failure probability
/// of the `(ε, δ)` guarantee) and must be `None`, or in `(0, 1)`, for
/// Laplace, which gives a pure `ε`-guarantee with no `δ` term.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrivacyParams {
    pub epsilon: f64,
    pub delta: Option<f64>,
}

impl PrivacyParams {
    pub fn new(epsilon: f64) -> Result<Self, PrivacyError> {
        Self::validate_epsilon(epsilon)?;
        Ok(Self {
            epsilon,
            delta: None,
        })
    }

    pub fn with_delta(epsilon: f64, delta: f64) -> Result<Self, PrivacyError> {
        Self::validate_epsilon(epsilon)?;
        if !(delta > 0.0 && delta < 1.0) {
            return Err(PrivacyError::InvalidDelta(delta));
        }
        Ok(Self {
            epsilon,
            delta: Some(delta),
        })
    }

    fn validate_epsilon(epsilon: f64) -> Result<(), PrivacyError> {
        if !(epsilon > 0.0) || !epsilon.is_finite() {
            return Err(PrivacyError::InvalidEpsilon(epsilon));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PrivacyError {
    InvalidEpsilon(f64),
    InvalidDelta(f64),
    MissingDelta,
    BudgetExceeded { spent: f64, limit: f64 },
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyError::InvalidEpsilon(e) => {
                write!(f, "epsilon must be finite and > 0, got {e}")
            }
            PrivacyError::InvalidDelta(d) => write!(f, "delta must be in (0, 1), got {d}"),
            PrivacyError::MissingDelta => {
                write!(f, "the Gaussian mechanism requires delta to be set")
            }
            PrivacyError::BudgetExceeded { spent, limit } => write!(
                f,
                "privacy budget exceeded: {spent} epsilon spent against a {limit} limit"
            ),
        }
    }
}

impl std::error::Error for PrivacyError {}

/// Draws one sample from `Laplace(0, scale)` using inverse-CDF sampling:
/// `-scale · sign(u) · ln(1 − 2|u|)` for `u ~ Uniform(-1/2, 1/2)`.
fn sample_laplace(scale: f64, rng: &mut impl Rng) -> f64 {
    // `gen_range` is exclusive of 0.5, so `u` never reaches the ln(0)
    // singularity at |u| = 0.5.
    let u: f64 = rng.random_range(-0.5..0.5);
    -scale * u.signum() * (1.0 - 2.0 * u.abs()).ln()
}

/// Draws one sample from `Normal(0, std_dev)` via the Box–Muller transform.
fn sample_gaussian(std_dev: f64, rng: &mut impl Rng) -> f64 {
    // `gen_range` on `(0.0, 1.0)` to keep both away from the ln(0)/atan2
    // singularities at exactly 0.
    let u1: f64 = rng.random_range(f64::EPSILON..1.0);
    let u2: f64 = rng.random_range(0.0..1.0);
    let radius = (-2.0 * u1.ln()).sqrt();
    std_dev * radius * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Adds Laplace-mechanism noise to `value`, calibrated to `sensitivity` and
/// `params.epsilon`. `params.delta` is ignored -- Laplace needs none.
pub fn add_laplace_noise(
    value: f64,
    sensitivity: f64,
    params: &PrivacyParams,
    rng: &mut impl Rng,
) -> Result<f64, PrivacyError> {
    if !(sensitivity > 0.0) || !sensitivity.is_finite() {
        return Err(PrivacyError::InvalidEpsilon(sensitivity));
    }
    let scale = sensitivity / params.epsilon;
    Ok(value + sample_laplace(scale, rng))
}

/// Adds Gaussian-mechanism noise to `value`, calibrated to `sensitivity`,
/// `params.epsilon` and `params.delta`. Returns [`PrivacyError::MissingDelta`]
/// if `params.delta` is `None`.
pub fn add_gaussian_noise(
    value: f64,
    sensitivity: f64,
    params: &PrivacyParams,
    rng: &mut impl Rng,
) -> Result<f64, PrivacyError> {
    if !(sensitivity > 0.0) || !sensitivity.is_finite() {
        return Err(PrivacyError::InvalidEpsilon(sensitivity));
    }
    let delta = params.delta.ok_or(PrivacyError::MissingDelta)?;
    // σ = Δ·√(2·ln(1.25/δ)) / ε  (Dwork & Roth, Theorem 3.22).
    let std_dev = sensitivity * (2.0 * (1.25_f64 / delta).ln()).sqrt() / params.epsilon;
    Ok(value + sample_gaussian(std_dev, rng))
}

/// Tracks cumulative privacy loss across repeated queries against one asset
/// or session, using basic composition (`k` queries at `ε` each cost `k·ε`).
/// A caller issuing many aggregate queries (e.g. once per page load of an
/// analytics dashboard) should hold one of these for the session rather than
/// a fresh [`PrivacyParams`] per call, or the per-query guarantee says
/// nothing about what the queries reveal together.
#[derive(Debug, Clone)]
pub struct PrivacyBudget {
    limit: f64,
    spent: f64,
}

impl PrivacyBudget {
    pub fn new(limit: f64) -> Result<Self, PrivacyError> {
        if !(limit > 0.0) || !limit.is_finite() {
            return Err(PrivacyError::InvalidEpsilon(limit));
        }
        Ok(Self { limit, spent: 0.0 })
    }

    pub fn remaining(&self) -> f64 {
        self.limit - self.spent
    }

    /// Reserves `epsilon` against the budget. Fails, without mutating state,
    /// if that would exceed the limit.
    pub fn charge(&mut self, epsilon: f64) -> Result<(), PrivacyError> {
        let next = self.spent + epsilon;
        if next > self.limit {
            return Err(PrivacyError::BudgetExceeded {
                spent: next,
                limit: self.limit,
            });
        }
        self.spent = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    #[test]
    fn rejects_non_positive_epsilon() {
        assert!(PrivacyParams::new(0.0).is_err());
        assert!(PrivacyParams::new(-1.0).is_err());
        assert!(PrivacyParams::new(f64::NAN).is_err());
    }

    #[test]
    fn rejects_delta_outside_open_unit_interval() {
        assert!(PrivacyParams::with_delta(1.0, 0.0).is_err());
        assert!(PrivacyParams::with_delta(1.0, 1.0).is_err());
        assert!(PrivacyParams::with_delta(1.0, 1.5).is_err());
        assert!(PrivacyParams::with_delta(1.0, 0.5).is_ok());
    }

    #[test]
    fn gaussian_mechanism_requires_delta() {
        let params = PrivacyParams::new(1.0).unwrap();
        let mut r = rng();
        assert_eq!(
            add_gaussian_noise(100.0, 1.0, &params, &mut r),
            Err(PrivacyError::MissingDelta)
        );
    }

    #[test]
    fn laplace_noise_has_zero_mean_and_scale_matches_theory() {
        // Mean of Laplace(0, b) is 0 and variance is 2b^2; check both hold
        // over a large sample rather than asserting on any single draw,
        // since that's what the mechanism actually guarantees.
        let params = PrivacyParams::new(0.5).unwrap();
        let sensitivity = 1.0;
        let scale = sensitivity / params.epsilon;
        let mut r = rng();

        let n = 200_000;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for _ in 0..n {
            let noisy = add_laplace_noise(0.0, sensitivity, &params, &mut r).unwrap();
            sum += noisy;
            sum_sq += noisy * noisy;
        }
        let mean = sum / n as f64;
        let variance = sum_sq / n as f64 - mean * mean;
        let expected_variance = 2.0 * scale * scale;

        assert!(mean.abs() < 0.05, "mean {mean} too far from 0");
        assert!(
            (variance - expected_variance).abs() / expected_variance < 0.1,
            "variance {variance} too far from expected {expected_variance}"
        );
    }

    #[test]
    fn gaussian_noise_std_dev_matches_theory() {
        let params = PrivacyParams::with_delta(0.5, 1e-5).unwrap();
        let sensitivity = 1.0;
        let expected_std_dev =
            sensitivity * (2.0 * (1.25_f64 / params.delta.unwrap()).ln()).sqrt() / params.epsilon;
        let mut r = rng();

        let n = 200_000;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for _ in 0..n {
            let noisy = add_gaussian_noise(0.0, sensitivity, &params, &mut r).unwrap();
            sum += noisy;
            sum_sq += noisy * noisy;
        }
        let mean = sum / n as f64;
        let variance = sum_sq / n as f64 - mean * mean;
        let std_dev = variance.sqrt();

        assert!(mean.abs() < 0.1, "mean {mean} too far from 0");
        assert!(
            (std_dev - expected_std_dev).abs() / expected_std_dev < 0.1,
            "std dev {std_dev} too far from expected {expected_std_dev}"
        );
    }

    #[test]
    fn smaller_epsilon_means_more_noise() {
        // Tighter privacy (smaller epsilon) must widen the noise
        // distribution -- otherwise the "privacy knob" does nothing.
        let tight = PrivacyParams::new(0.1).unwrap();
        let loose = PrivacyParams::new(10.0).unwrap();
        let sensitivity = 1.0;
        let mut r = rng();

        let sample_abs_mean = |params: &PrivacyParams, r: &mut StdRng| {
            let n = 50_000;
            let mut sum_abs = 0.0;
            for _ in 0..n {
                sum_abs += add_laplace_noise(0.0, sensitivity, params, r)
                    .unwrap()
                    .abs();
            }
            sum_abs / n as f64
        };

        let tight_noise = sample_abs_mean(&tight, &mut r);
        let loose_noise = sample_abs_mean(&loose, &mut r);

        assert!(
            tight_noise > loose_noise,
            "tight epsilon ({tight_noise}) should be noisier than loose epsilon ({loose_noise})"
        );
    }

    #[test]
    fn two_queries_never_return_the_exact_raw_value() {
        // Not a formal leakage proof, but a cheap sanity check that the
        // mechanism is actually being applied: for a reasonably small
        // epsilon, the noisy output should essentially never land exactly
        // on the raw value it is protecting.
        let params = PrivacyParams::new(1.0).unwrap();
        let mut r = rng();
        let raw_value = 12_345.678;

        for _ in 0..1_000 {
            let noisy = add_laplace_noise(raw_value, 1.0, &params, &mut r).unwrap();
            assert_ne!(noisy, raw_value);
        }
    }

    #[test]
    fn privacy_budget_tracks_composition_and_rejects_overspend() {
        let mut budget = PrivacyBudget::new(1.0).unwrap();
        budget.charge(0.4).unwrap();
        budget.charge(0.4).unwrap();
        assert!((budget.remaining() - 0.2).abs() < 1e-9);

        // A third charge of 0.4 would bring total spend to 1.2, over the
        // 1.0 limit -- it must be rejected, and must not partially apply.
        let err = budget.charge(0.4).unwrap_err();
        assert!(matches!(err, PrivacyError::BudgetExceeded { .. }));
        assert!((budget.remaining() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn rejects_non_positive_sensitivity() {
        let params = PrivacyParams::new(1.0).unwrap();
        let mut r = rng();
        assert!(add_laplace_noise(10.0, 0.0, &params, &mut r).is_err());
        assert!(add_laplace_noise(10.0, -1.0, &params, &mut r).is_err());
    }
}
