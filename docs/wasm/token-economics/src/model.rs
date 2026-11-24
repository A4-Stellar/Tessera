//! Agent-based token-economics model.
//!
//! Three agent classes interact through an AMM whose oracle price follows a
//! geometric random walk:
//!
//! * **InstitutionalHolder** buys dips (AMM price below `oracle * (1 - dip)`)
//!   and trims positions above `oracle * (1 + dip)`.
//! * **RetailHolder** follows short-term momentum.
//! * **Arbitrageur** exploits discrepancies between AMM and oracle prices.
//!
//! All arithmetic is clamped to finite bounds and the PRNG is deterministic,
//! so a scenario always replays identically — the property the numerical
//! stability test in [`tests`] checks.

const PRICE_MIN: f64 = 1e-6;
const PRICE_MAX: f64 = 1e6;

/// Number of f64 parameters in the WABI payload.
pub const PARAM_COUNT: usize = 14;

/// Deterministic SplitMix64 PRNG.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn normal(&mut self) -> f64 {
        let u = self.uniform().max(f64::MIN_POSITIVE);
        let v = self.uniform();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    pub fn poisson_hit(&mut self, mean_interval: f64) -> bool {
        let lambda = (1.0 / mean_interval.max(1.0)).min(1.0);
        self.uniform() < lambda
    }
}

fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if !value.is_finite() {
        return low;
    }
    value.max(low).min(high)
}

/// UI-controlled simulation parameters. Every field is clamped on ingest.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub seed: u64,
    pub steps: u32,
    pub initial_price: f64,
    pub drift: f64,
    pub volatility: f64,
    pub institutional: u32,
    pub retail: u32,
    pub arbitrageur: u32,
    pub dip_threshold: f64,
    pub momentum_window: u32,
    pub arbitrage_tolerance: f64,
    pub liquidity_depth: f64,
    pub dividend_rate: f64,
    pub dividend_lambda: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            seed: 42,
            steps: 500,
            initial_price: 30.0,
            drift: 0.0002,
            volatility: 0.01,
            institutional: 40,
            retail: 160,
            arbitrageur: 12,
            dip_threshold: 0.04,
            momentum_window: 10,
            arbitrage_tolerance: 0.002,
            liquidity_depth: 1_000.0,
            dividend_rate: 0.06,
            dividend_lambda: 60.0,
        }
    }
}

impl Params {
    /// Decode the flat f64 payload used by the WASM ABI, clamping every value.
    pub fn from_f64_slice(values: &[f64]) -> Self {
        let get = |i: usize, default: f64| -> f64 {
            values
                .get(i)
                .copied()
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let as_u64 =
            |i: usize, default: u64| -> u64 { get(i, default as f64).round().max(0.0) as u64 };
        let as_u32 = |i: usize, default: u32, max: u32| -> u32 {
            (as_u64(i, default as u64) as u128).min(max as u128) as u32
        };
        Self {
            seed: as_u64(0, 42),
            steps: as_u32(1, 500, 10_000).max(1),
            initial_price: clamp(get(2, 30.0), PRICE_MIN, PRICE_MAX),
            drift: clamp(get(3, 0.0002), -1.0, 1.0),
            volatility: clamp(get(4, 0.01), 0.0, 2.0),
            institutional: as_u32(5, 40, 2_000),
            retail: as_u32(6, 160, 10_000),
            arbitrageur: as_u32(7, 12, 2_000),
            dip_threshold: clamp(get(8, 0.04), 0.0, 0.95),
            momentum_window: as_u32(9, 10, 500).max(1),
            arbitrage_tolerance: clamp(get(10, 0.002), 0.0, 0.5),
            liquidity_depth: clamp(get(11, 1_000.0), 1e-3, 1e12),
            dividend_rate: clamp(get(12, 0.06), 0.0, 2.0),
            dividend_lambda: clamp(get(13, 60.0), 1.0, 100_000.0),
        }
    }
}

#[derive(Debug, Clone)]
struct Agent {
    cash: f64,
    shares: f64,
}

impl Agent {
    fn wealth(&self, price: f64) -> f64 {
        self.cash + self.shares * price
    }
}

/// Complete result of a run, serialised to JSON for the docs worker.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationOutput {
    pub steps: u32,
    pub price_series: Vec<f64>,
    pub oracle_series: Vec<f64>,
    /// `holder_distribution[t][bin]` — bin counts at sampled steps.
    pub holder_distribution: Vec<Vec<u32>>,
    pub dividend_yield: Vec<f64>,
    /// `liquidity_depth[t][bucket]` — sampled order-book depth.
    pub liquidity_depth: Vec<Vec<f64>>,
    pub final_gini: f64,
    pub max_drawdown: f64,
}

/// A running simulation. `step()` advances exactly one time step, which lets
/// the Web Worker report progress and stay responsive.
#[derive(Debug, Clone)]
pub struct Simulation {
    pub params: Params,
    rng: Rng,
    t: u32,
    oracle: f64,
    amm: f64,
    institutional: Vec<Agent>,
    retail: Vec<Agent>,
    arbitrageurs: Vec<Agent>,
    last_dividend_per_share: f64,
    output: SimulationOutput,
    sample_every: u32,
}

impl Simulation {
    pub fn new(params: Params) -> Self {
        let total_agents = (params.institutional as u64)
            .saturating_add(params.retail as u64)
            .saturating_add(params.arbitrageur as u64)
            .max(1) as f64;
        let cash_per_agent = params.liquidity_depth * 2.0 / total_agents;
        let mk_agents = |count: u32| {
            (0..count)
                .map(|_| Agent {
                    cash: cash_per_agent,
                    shares: 0.0,
                })
                .collect()
        };
        let sample_every = (params.steps / 40).max(1);
        Self {
            params: params.clone(),
            rng: Rng::new(params.seed),
            t: 0,
            oracle: params.initial_price,
            amm: params.initial_price,
            institutional: mk_agents(params.institutional),
            retail: mk_agents(params.retail),
            arbitrageurs: mk_agents(params.arbitrageur),
            last_dividend_per_share: 0.0,
            output: SimulationOutput {
                steps: params.steps,
                price_series: Vec::with_capacity(params.steps as usize),
                oracle_series: Vec::with_capacity(params.steps as usize),
                holder_distribution: Vec::new(),
                dividend_yield: Vec::with_capacity(params.steps as usize),
                liquidity_depth: Vec::new(),
                final_gini: 0.0,
                max_drawdown: 0.0,
            },
            sample_every,
        }
    }

    pub fn progress(&self) -> f64 {
        (self.t as f64 / self.params.steps as f64).min(1.0)
    }

    pub fn is_done(&self) -> bool {
        self.t >= self.params.steps
    }

    pub fn step(&mut self) {
        if self.is_done() {
            return;
        }
        let dt: f64 = 1.0 / 252.0;
        // Oracle: geometric random walk with drift and volatility.
        let shock = self.params.volatility * self.rng.normal() * dt.sqrt();
        self.oracle = clamp(
            self.oracle * (1.0 + self.params.drift * dt + shock),
            PRICE_MIN,
            PRICE_MAX,
        );

        // Institutional investors buy dips and trim rallies; retail investors
        // chase momentum. Both trade through `apply_order` below so their flow
        // moves the AMM price exactly once.
        let window = self.params.momentum_window as usize;
        let past = self
            .output
            .price_series
            .get(self.output.price_series.len().saturating_sub(window))
            .copied()
            .unwrap_or(self.amm);
        let momentum = (self.amm - past) / past.max(PRICE_MIN);

        // Arbitrageurs close the AMM/oracle gap and capture the spread.
        let mut depth = vec![0.0f64; 8];
        let mut net_flow = 0.0f64;
        // Copy scalars out of `self` so the closure does not keep a borrow of
        // the whole struct while we mutate individual agent vectors.
        let oracle = self.oracle;
        let amm = self.amm;
        let liquidity = self.params.liquidity_depth;
        let apply_order = |agent: &mut Agent,
                           amount: f64,
                           buy: bool,
                           price: f64,
                           depth: &mut Vec<f64>,
                           net_flow: &mut f64| {
            let amount = amount.max(0.0).min(if buy {
                agent.cash
            } else {
                agent.shares * price
            });
            if amount <= 0.0 {
                return;
            }
            if buy {
                agent.cash -= amount;
                agent.shares += amount / price;
            } else {
                agent.shares -= amount / price;
                agent.cash += amount;
            }
            *net_flow += if buy { amount } else { -amount };
            let bucket = (((price / oracle.max(PRICE_MIN) - 0.9) / 0.2) * 8.0)
                .round()
                .max(0.0)
                .min(7.0) as usize;
            depth[bucket] += amount;
        };

        let dip = self.params.dip_threshold;
        let tolerance = self.params.arbitrage_tolerance;
        for agent in &mut self.institutional {
            let ratio = amm / oracle.max(PRICE_MIN);
            if ratio < 1.0 - dip {
                apply_order(
                    agent,
                    agent.cash * 0.05,
                    true,
                    amm,
                    &mut depth,
                    &mut net_flow,
                );
            } else if ratio > 1.0 + dip {
                apply_order(
                    agent,
                    agent.shares * amm * 0.05,
                    false,
                    amm,
                    &mut depth,
                    &mut net_flow,
                );
            }
        }
        for agent in &mut self.retail {
            if momentum > 0.0 {
                apply_order(
                    agent,
                    agent.cash * 0.02,
                    true,
                    amm,
                    &mut depth,
                    &mut net_flow,
                );
            } else if momentum < 0.0 {
                apply_order(
                    agent,
                    agent.shares * amm * 0.02,
                    false,
                    amm,
                    &mut depth,
                    &mut net_flow,
                );
            }
        }
        for agent in &mut self.arbitrageurs {
            let gap = (oracle - amm) / oracle.max(PRICE_MIN);
            if gap.abs() > tolerance {
                let size = gap.abs() * liquidity * 0.1;
                apply_order(agent, size, gap > 0.0, amm, &mut depth, &mut net_flow);
            }
        }

        // AMM price moves with net flow, then mean-reverts toward the oracle.
        let impact = net_flow / self.params.liquidity_depth;
        self.amm = clamp(
            self.amm * (1.0 + impact.clamp(-0.5, 0.5)),
            PRICE_MIN,
            PRICE_MAX,
        );
        self.amm += (self.oracle - self.amm) * 0.05;
        self.amm = clamp(self.amm, PRICE_MIN, PRICE_MAX);

        // Dividends arrive as a Poisson process; yield is last dividend / price.
        if self.rng.poisson_hit(self.params.dividend_lambda) {
            self.last_dividend_per_share = self.oracle * self.params.dividend_rate;
            for agent in self
                .institutional
                .iter_mut()
                .chain(self.retail.iter_mut())
                .chain(self.arbitrageurs.iter_mut())
            {
                agent.cash += agent.shares * self.last_dividend_per_share;
            }
        }
        let yield_now = self.last_dividend_per_share / self.amm.max(PRICE_MIN);

        self.output.price_series.push(self.amm);
        self.output.oracle_series.push(self.oracle);
        self.output.dividend_yield.push(clamp(yield_now, 0.0, 10.0));

        if self.t % self.sample_every == 0 {
            self.output.holder_distribution.push(self.holder_bins());
            self.output.liquidity_depth.push(depth);
        }
        self.t += 1;
        if self.t == self.params.steps {
            self.output.final_gini = self.gini();
            self.output.max_drawdown = self.max_drawdown();
        }
    }

    pub fn run(&mut self) {
        while !self.is_done() {
            self.step();
        }
    }

    pub fn output(&self) -> &SimulationOutput {
        &self.output
    }

    fn all_agents(&self) -> impl Iterator<Item = &Agent> {
        self.institutional
            .iter()
            .chain(self.retail.iter())
            .chain(self.arbitrageurs.iter())
    }

    fn holder_bins(&self) -> Vec<u32> {
        let mut bins = vec![0u32; 8];
        let mut wealth: Vec<f64> = self.all_agents().map(|a| a.wealth(self.amm)).collect();
        wealth.sort_by(f64::total_cmp);
        let median = wealth
            .get(wealth.len() / 2)
            .copied()
            .unwrap_or(1.0)
            .max(PRICE_MIN);
        for value in wealth {
            let bin = (value / median).log2().max(-4.0).min(4.0) + 4.0;
            bins[(bin as usize).min(7)] += 1;
        }
        bins
    }

    fn gini(&self) -> f64 {
        let mut wealth: Vec<f64> = self.all_agents().map(|a| a.wealth(self.amm)).collect();
        wealth.sort_by(f64::total_cmp);
        let n = wealth.len();
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = wealth.iter().sum();
        if sum <= 0.0 {
            return 0.0;
        }
        let weighted: f64 = wealth
            .iter()
            .enumerate()
            .map(|(i, value)| (i as f64 + 1.0) * value)
            .sum();
        clamp(
            2.0 * weighted / (n as f64 * sum) - (n as f64 + 1.0) / n as f64,
            0.0,
            1.0,
        )
    }

    fn max_drawdown(&self) -> f64 {
        let mut peak: f64 = PRICE_MIN;
        let mut worst: f64 = 0.0;
        for price in &self.output.price_series {
            peak = peak.max(*price);
            worst = worst.max((peak - price) / peak.max(PRICE_MIN));
        }
        clamp(worst, 0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_run(params: Params) -> Simulation {
        let mut sim = Simulation::new(params);
        sim.run();
        sim
    }

    #[test]
    fn output_is_deterministic_for_a_seed() {
        let a = full_run(Params::default());
        let b = full_run(Params::default());
        assert_eq!(a.output(), b.output());
    }

    #[test]
    fn numerical_stability_no_nan_or_infinity_across_extreme_inputs() {
        let extremes = [
            Params {
                seed: 1,
                steps: 2_000,
                volatility: 2.0,
                drift: -1.0,
                ..Params::default()
            },
            Params {
                seed: 2,
                steps: 2_000,
                volatility: 0.0,
                drift: 1.0,
                liquidity_depth: 1e-3,
                ..Params::default()
            },
            Params {
                seed: 3,
                steps: 2_000,
                initial_price: 1e-6,
                arbitrage_tolerance: 0.0,
                dividend_rate: 2.0,
                ..Params::default()
            },
            Params {
                seed: 4,
                steps: 2_000,
                retail: 0,
                institutional: 0,
                arbitrageur: 0,
                ..Params::default()
            },
        ];
        for params in extremes {
            let sim = full_run(params);
            let out = sim.output();
            assert_eq!(out.price_series.len(), out.steps as usize);
            for value in out
                .price_series
                .iter()
                .chain(out.oracle_series.iter())
                .chain(out.dividend_yield.iter())
                .chain(std::iter::once(&out.final_gini))
                .chain(std::iter::once(&out.max_drawdown))
            {
                assert!(value.is_finite(), "non-finite output: {value}");
                assert!(*value >= 0.0, "negative output: {value}");
            }
            assert!(out.final_gini >= 0.0 && out.final_gini <= 1.0);
            assert!(out.max_drawdown >= 0.0 && out.max_drawdown <= 1.0);
        }
    }

    #[test]
    fn progress_is_monotonic_and_reaches_one() {
        let mut sim = Simulation::new(Params {
            steps: 10,
            ..Params::default()
        });
        let mut last = 0.0;
        while !sim.is_done() {
            sim.step();
            let progress = sim.progress();
            assert!(progress >= last);
            last = progress;
        }
        assert_eq!(last, 1.0);
    }

    #[test]
    fn arbitrageurs_reduce_the_amm_oracle_gap() {
        let params = Params {
            seed: 77,
            arbitrageur: 200,
            arbitrage_tolerance: 0.0,
            liquidity_depth: 100_000.0,
            ..Params::default()
        };
        let sim = full_run(params);
        let last_price = *sim.output().price_series.last().unwrap();
        let last_oracle = *sim.output().oracle_series.last().unwrap();
        assert!(
            (last_price - last_oracle).abs() / last_oracle < 0.1,
            "arbitrage did not close the gap"
        );
    }
}
