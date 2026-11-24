/**
 * TypeScript reference implementation of the Rust `token-economics` model.
 *
 * It is used by the Web Worker when the compiled `.wasm` has not been built
 * (fresh checkouts, jsdom tests). The agent rules and RNG match the Rust
 * crate exactly so a scenario produces the same series in either engine.
 */

export interface SimParams {
  seed: number;
  steps: number;
  initialPrice: number;
  drift: number;
  volatility: number;
  institutional: number;
  retail: number;
  arbitrageur: number;
  dipThreshold: number;
  momentumWindow: number;
  arbitrageTolerance: number;
  liquidityDepth: number;
  dividendRate: number;
  dividendLambda: number;
}

export interface Scenario {
  version: 1;
  name: string;
  params: SimParams;
}

export interface SimOutput {
  steps: number;
  price_series: number[];
  oracle_series: number[];
  holder_distribution: number[][];
  dividend_yield: number[];
  liquidity_depth: number[][];
  final_gini: number;
  max_drawdown: number;
}

const PRICE_MIN = 1e-6;
const PRICE_MAX = 1e6;

export const DEFAULT_PARAMS: SimParams = {
  seed: 42,
  steps: 500,
  initialPrice: 30,
  drift: 0.0002,
  volatility: 0.01,
  institutional: 40,
  retail: 160,
  arbitrageur: 12,
  dipThreshold: 0.04,
  momentumWindow: 10,
  arbitrageTolerance: 0.002,
  liquidityDepth: 1000,
  dividendRate: 0.06,
  dividendLambda: 60,
};

const clamp = (v: number, lo: number, hi: number): number =>
  Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : lo;

const U64_MASK = 0xffffffffffffffffn;

class Rng {
  private state: bigint;
  constructor(seed: number) {
    this.state = BigInt(seed) & U64_MASK;
  }
  nextU64(): bigint {
    // Full 64-bit SplitMix64 — identical to the Rust engine's RNG.
    this.state = (this.state + 0x9e3779b97f4a7c15n) & U64_MASK;
    let z = this.state;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & U64_MASK;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & U64_MASK;
    return z ^ (z >> 31n);
  }
  uniform(): number {
    return Number(this.nextU64() >> 11n) / 2 ** 53;
  }
  normal(): number {
    const u = Math.max(this.uniform(), Number.MIN_VALUE);
    const v = this.uniform();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }
  poissonHit(meanInterval: number): boolean {
    return this.uniform() < Math.min(1, 1 / Math.max(1, meanInterval));
  }
}

interface Agent {
  cash: number;
  shares: number;
}

/** Validate and normalise an imported scenario. Never trust imported numbers. */
export function parseScenario(input: unknown): Scenario {
  if (!input || typeof input !== "object") throw new Error("scenario must be an object");
  const raw = input as Partial<Scenario>;
  const supplied = (raw.params ?? {}) as Partial<SimParams>;
  const p: SimParams = {
    seed: clamp(Number(supplied.seed ?? DEFAULT_PARAMS.seed), 0, 2 ** 53 - 1),
    steps: Math.round(clamp(Number(supplied.steps ?? DEFAULT_PARAMS.steps), 1, 10_000)),
    initialPrice: clamp(Number(supplied.initialPrice ?? DEFAULT_PARAMS.initialPrice), PRICE_MIN, PRICE_MAX),
    drift: clamp(Number(supplied.drift ?? DEFAULT_PARAMS.drift), -1, 1),
    volatility: clamp(Number(supplied.volatility ?? DEFAULT_PARAMS.volatility), 0, 2),
    institutional: Math.round(clamp(Number(supplied.institutional ?? DEFAULT_PARAMS.institutional), 0, 2_000)),
    retail: Math.round(clamp(Number(supplied.retail ?? DEFAULT_PARAMS.retail), 0, 10_000)),
    arbitrageur: Math.round(clamp(Number(supplied.arbitrageur ?? DEFAULT_PARAMS.arbitrageur), 0, 2_000)),
    dipThreshold: clamp(Number(supplied.dipThreshold ?? DEFAULT_PARAMS.dipThreshold), 0, 0.95),
    momentumWindow: Math.round(clamp(Number(supplied.momentumWindow ?? DEFAULT_PARAMS.momentumWindow), 1, 500)),
    arbitrageTolerance: clamp(Number(supplied.arbitrageTolerance ?? DEFAULT_PARAMS.arbitrageTolerance), 0, 0.5),
    liquidityDepth: clamp(Number(supplied.liquidityDepth ?? DEFAULT_PARAMS.liquidityDepth), 1e-3, 1e12),
    dividendRate: clamp(Number(supplied.dividendRate ?? DEFAULT_PARAMS.dividendRate), 0, 2),
    dividendLambda: clamp(Number(supplied.dividendLambda ?? DEFAULT_PARAMS.dividendLambda), 1, 100_000),
  };
  return {
    version: 1,
    name: typeof raw.name === "string" ? raw.name.slice(0, 120) : "Untitled scenario",
    params: p,
  };
}

export class TokenEconomicsEngine {
  readonly scenario: Scenario;
  private rng: Rng;
  private t = 0;
  private oracle: number;
  private amm: number;
  private institutional: Agent[];
  private retail: Agent[];
  private arbitrageurs: Agent[];
  private lastDividendPerShare = 0;
  private readonly sampleEvery: number;
  private result: SimOutput;

  constructor(scenario: Scenario) {
    this.scenario = parseScenario(scenario);
    const p = this.scenario.params;
    this.rng = new Rng(p.seed);
    this.oracle = p.initialPrice;
    this.amm = p.initialPrice;
    const total = Math.max(1, p.institutional + p.retail + p.arbitrageur);
    const cash = (p.liquidityDepth * 2) / total;
    const make = (count: number): Agent[] =>
      Array.from({ length: count }, () => ({ cash, shares: 0 }));
    this.institutional = make(p.institutional);
    this.retail = make(p.retail);
    this.arbitrageurs = make(p.arbitrageur);
    this.sampleEvery = Math.max(1, Math.floor(p.steps / 40));
    this.result = {
      steps: p.steps,
      price_series: [],
      oracle_series: [],
      holder_distribution: [],
      dividend_yield: [],
      liquidity_depth: [],
      final_gini: 0,
      max_drawdown: 0,
    };
  }

  progress(): number {
    return Math.min(1, this.t / this.scenario.params.steps);
  }

  isDone(): boolean {
    return this.t >= this.scenario.params.steps;
  }

  output(): SimOutput {
    return this.result;
  }

  run(): SimOutput {
    while (!this.isDone()) this.step();
    return this.result;
  }

  step(): void {
    if (this.isDone()) return;
    const p = this.scenario.params;
    const dt = 1 / 252;
    const shock = p.volatility * this.rng.normal() * Math.sqrt(dt);
    this.oracle = clamp(this.oracle * (1 + p.drift * dt + shock), PRICE_MIN, PRICE_MAX);

    const past = this.result.price_series[
      Math.max(0, this.result.price_series.length - p.momentumWindow)
    ] ?? this.amm;
    const momentum = (this.amm - past) / Math.max(PRICE_MIN, past);

    const depth = new Array<number>(8).fill(0);
    let netFlow = 0;
    const applyOrder = (agent: Agent, amount: number, buy: boolean, price: number): void => {
      const bounded = Math.max(
        0,
        Math.min(amount, buy ? agent.cash : agent.shares * price),
      );
      if (bounded <= 0) return;
      if (buy) {
        agent.cash -= bounded;
        agent.shares += bounded / price;
      } else {
        agent.shares -= bounded / price;
        agent.cash += bounded;
      }
      netFlow += buy ? bounded : -bounded;
      const bucket = Math.round(
        clamp(((price / Math.max(PRICE_MIN, this.oracle) - 0.9) / 0.2) * 8, 0, 7),
      );
      depth[bucket] += bounded;
    };

    for (const agent of this.institutional) {
      const ratio = this.amm / Math.max(PRICE_MIN, this.oracle);
      if (ratio < 1 - p.dipThreshold) applyOrder(agent, agent.cash * 0.05, true, this.amm);
      else if (ratio > 1 + p.dipThreshold)
        applyOrder(agent, agent.shares * this.amm * 0.05, false, this.amm);
    }
    for (const agent of this.retail) {
      if (momentum > 0) applyOrder(agent, agent.cash * 0.02, true, this.amm);
      else if (momentum < 0)
        applyOrder(agent, agent.shares * this.amm * 0.02, false, this.amm);
    }
    for (const agent of this.arbitrageurs) {
      const gap = (this.oracle - this.amm) / Math.max(PRICE_MIN, this.oracle);
      if (Math.abs(gap) > p.arbitrageTolerance) {
        applyOrder(agent, Math.abs(gap) * p.liquidityDepth * 0.1, gap > 0, this.amm);
      }
    }

    const impact = netFlow / p.liquidityDepth;
    this.amm = clamp(this.amm * (1 + clamp(impact, -0.5, 0.5)), PRICE_MIN, PRICE_MAX);
    this.amm += (this.oracle - this.amm) * 0.05;
    this.amm = clamp(this.amm, PRICE_MIN, PRICE_MAX);

    if (this.rng.poissonHit(p.dividendLambda)) {
      this.lastDividendPerShare = this.oracle * p.dividendRate;
      for (const agent of [...this.institutional, ...this.retail, ...this.arbitrageurs]) {
        agent.cash += agent.shares * this.lastDividendPerShare;
      }
    }
    this.result.price_series.push(this.amm);
    this.result.oracle_series.push(this.oracle);
    this.result.dividend_yield.push(
      clamp(this.lastDividendPerShare / Math.max(PRICE_MIN, this.amm), 0, 10),
    );
    if (this.t % this.sampleEvery === 0) {
      this.result.holder_distribution.push(this.holderBins());
      this.result.liquidity_depth.push(depth);
    }
    this.t += 1;
    if (this.t === p.steps) {
      this.result.final_gini = this.gini();
      this.result.max_drawdown = this.maxDrawdown();
    }
  }

  private allAgents(): Agent[] {
    return [...this.institutional, ...this.retail, ...this.arbitrageurs];
  }

  private holderBins(): number[] {
    const bins = new Array<number>(8).fill(0);
    const wealth = this.allAgents()
      .map((a) => a.cash + a.shares * this.amm)
      .sort((a, b) => a - b);
    const median = Math.max(PRICE_MIN, wealth[Math.floor(wealth.length / 2)] ?? 1);
    for (const value of wealth) {
      const bin = Math.log2(Math.max(1e-4, Math.min(16, value / median))) + 4;
      bins[Math.min(7, Math.round(bin))] += 1;
    }
    return bins;
  }

  private gini(): number {
    const wealth = this.allAgents()
      .map((a) => a.cash + a.shares * this.amm)
      .sort((a, b) => a - b);
    const n = wealth.length;
    const sum = wealth.reduce((a, b) => a + b, 0);
    if (n === 0 || sum <= 0) return 0;
    const weighted = wealth.reduce((acc, v, i) => acc + (i + 1) * v, 0);
    return clamp((2 * weighted) / (n * sum) - (n + 1) / n, 0, 1);
  }

  private maxDrawdown(): number {
    let peak = PRICE_MIN;
    let worst = 0;
    for (const price of this.result.price_series) {
      peak = Math.max(peak, price);
      worst = Math.max(worst, (peak - price) / Math.max(PRICE_MIN, peak));
    }
    return clamp(worst, 0, 1);
  }
}
