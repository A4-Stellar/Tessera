//! Deterministic samplers for the sandbox's synthetic data.
//!
//! The sandbox must be reproducible: two tenants reset with the same seed and
//! the same parameters get byte-identical CSV files. To keep the binary buildable
//! in an air-gapped environment this module uses a small SplitMix64 PRNG instead
//! of pulling in `rand`. SplitMix64 has a full 64-bit period and passes the
//! statistical checks a seed dataset needs (BigCrush is overkill here).

/// SplitMix64 — deterministic, fast, no external dependencies.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next raw `u64`.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform sample in `[0, 1)`.
    pub fn uniform(&mut self) -> f64 {
        // 53 bits of mantissa, matching f64 precision exactly.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal via Box–Muller (one sample per call, using the cosine
    /// form so we do not need to cache a second value).
    pub fn normal(&mut self) -> f64 {
        let u = self.uniform().max(f64::MIN_POSITIVE);
        let v = self.uniform();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    /// Pareto type I: `x >= x_min`, with tail index `alpha`. Holder balances
    /// are heavy-tailed, so `alpha` is typically 1.1–1.6.
    pub fn pareto(&mut self, x_min: f64, alpha: f64) -> f64 {
        assert!(x_min > 0.0 && alpha > 0.0, "invalid Pareto parameters");
        let u = self.uniform().max(f64::MIN_POSITIVE);
        x_min * u.powf(-1.0 / alpha)
    }

    /// Log-normal: `exp(mu + sigma * Z)`. Asset prices are strictly positive
    /// and right-skewed, which is exactly what log-normal captures.
    pub fn log_normal(&mut self, mu: f64, sigma: f64) -> f64 {
        assert!(sigma >= 0.0, "invalid log-normal sigma");
        (mu + sigma * self.normal()).exp()
    }

    /// Poisson draw. Knuth's product method for small `lambda`; a normal
    /// approximation (clamped at zero) for large `lambda` where the product
    /// would underflow. Dividend intervals use the small-lambda branch.
    pub fn poisson(&mut self, lambda: f64) -> u64 {
        assert!(lambda > 0.0 && lambda.is_finite(), "invalid Poisson lambda");
        if lambda < 30.0 {
            let limit = (-lambda).exp();
            let mut product = 1.0;
            let mut k = 0u64;
            loop {
                product *= self.uniform();
                if product <= limit {
                    return k;
                }
                k += 1;
            }
        }
        let value = (lambda + lambda.sqrt() * self.normal()).round();
        value.max(0.0) as u64
    }

    /// Pick an element from a slice using a uniform index.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        assert!(!items.is_empty(), "cannot choose from an empty slice");
        let index = (self.uniform() * items.len() as f64) as usize;
        &items[index.min(items.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_for_a_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        assert_eq!(
            (0..64).map(|_| a.next_u64()).collect::<Vec<_>>(),
            (0..64).map(|_| b.next_u64()).collect::<Vec<_>>()
        );
        assert_ne!(Rng::new(42).next_u64(), Rng::new(43).next_u64());
    }

    #[test]
    fn uniform_stays_in_unit_interval() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let u = rng.uniform();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn pareto_respects_minimum_and_is_heavy_tailed() {
        let mut rng = Rng::new(11);
        let mut max = 0.0f64;
        let mut sum = 0.0f64;
        const N: usize = 20_000;
        for _ in 0..N {
            let x = rng.pareto(100.0, 1.5);
            assert!(x >= 100.0);
            max = max.max(x);
            sum += x;
        }
        // With alpha=1.5 the theoretical mean is x_min * 3 = 300; the sample
        // fluctuates but must not collapse to the minimum.
        assert!(sum / N as f64 > 150.0, "pareto sample lost its heavy tail");
        assert!(max > 1_000.0, "pareto sample produced no outliers");
    }

    #[test]
    fn log_normal_is_positive_and_matches_its_median() {
        let mut rng = Rng::new(13);
        let mut values = Vec::with_capacity(20_000);
        for _ in 0..20_000 {
            let price = rng.log_normal(0.0, 0.5);
            assert!(price.is_finite() && price > 0.0);
            values.push(price);
        }
        values.sort_by(f64::total_cmp);
        let median = values[values.len() / 2];
        assert!((median - 1.0).abs() < 0.05, "median should be exp(mu) = 1");
    }

    #[test]
    fn poisson_mean_is_close_to_lambda() {
        let mut rng = Rng::new(17);
        let lambda = 6.0;
        let sum: u64 = (0..50_000).map(|_| rng.poisson(lambda)).sum();
        let mean = sum as f64 / 50_000.0;
        assert!((mean - lambda).abs() < 0.1, "poisson mean drifted: {mean}");
    }
}
