/* eslint-env worker */
/**
 * Web Worker for the TokenEconomicsSimulator docs component.
 *
 * Drives the Rust WASM module step-by-step (posting progress after each batch)
 * and falls back to an in-worker reference engine when the binary has not been
 * built. Keep the agent rules in sync with docs/lib/token-economics-engine.ts
 * and docs/wasm/token-economics/src/model.rs.
 */

const FIELDS = [
  "seed", "steps", "initialPrice", "drift", "volatility",
  "institutional", "retail", "arbitrageur", "dipThreshold",
  "momentumWindow", "arbitrageTolerance", "liquidityDepth",
  "dividendRate", "dividendLambda",
];
const DEFAULTS = [42, 500, 30, 0.0002, 0.01, 40, 160, 12, 0.04, 10, 0.002, 1000, 0.06, 60];
const PMAX = 1e6;
const PMIN = 1e-6;
const clamp = (v, lo, hi) => (Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : lo);

async function runWasm(scenario, onProgress) {
  const response = await fetch("/wasm/token_economics.wasm");
  if (!response.ok) throw new Error(`wasm unavailable (${response.status})`);
  const bytes = await response.arrayBuffer();
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const api = instance.exports;
  const params = new Float64Array(14);
  FIELDS.forEach((key, i) => {
    params[i] = clamp(Number(scenario.params?.[key] ?? DEFAULTS[i]), -PMAX, PMAX);
  });
  const ptr = api.tessera_alloc(params.byteLength);
  new Float64Array(api.memory.buffer, ptr, 14).set(params);
  const handle = api.tessera_sim_new(ptr, params.byteLength);
  if (!handle) throw new Error("wasm rejected the scenario");
  let lastReport = 0;
  for (;;) {
    if (!api.tessera_sim_step(handle)) break;
    const progress = api.tessera_sim_progress(handle);
    if (progress - lastReport > 0.02 || progress === 1) {
      lastReport = progress;
      onProgress(progress);
    }
  }
  api.tessera_sim_write_output(handle);
  const outPtr = api.tessera_sim_output_ptr();
  const len = api.tessera_sim_output_len();
  const json = new TextDecoder().decode(new Uint8Array(api.memory.buffer, outPtr, len));
  api.tessera_sim_free(handle);
  return JSON.parse(json);
}

const U64_MASK = 0xffffffffffffffffn;

class Rng {
  constructor(seed) { this.state = BigInt(seed) & U64_MASK; }
  next() {
    this.state = (this.state + 0x9e3779b97f4a7c15n) & U64_MASK;
    let z = this.state;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & U64_MASK;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & U64_MASK;
    return z ^ (z >> 31n);
  }
  uniform() { return Number(this.next() >> 11n) / 2 ** 53; }
  normal() {
    const u = Math.max(this.uniform(), Number.MIN_VALUE);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * this.uniform());
  }
  poissonHit(mean) { return this.uniform() < Math.min(1, 1 / Math.max(1, mean)); }
}

function runReference(scenario, onProgress) {
  const p = {};
  FIELDS.forEach((key, i) => { p[key] = Number(scenario.params?.[key] ?? DEFAULTS[i]); });
  p.steps = Math.round(clamp(p.steps, 1, 10000));
  p.initialPrice = clamp(p.initialPrice, PMIN, PMAX);
  p.volatility = clamp(p.volatility, 0, 2);
  p.liquidityDepth = clamp(p.liquidityDepth, 1e-3, 1e12);

  const rng = new Rng(p.seed);
  const total = Math.max(1, p.institutional + p.retail + p.arbitrageur);
  const cash = (p.liquidityDepth * 2) / total;
  const make = (n) => Array.from({ length: n }, () => ({ cash, shares: 0 }));
  const inst = make(p.institutional);
  const retail = make(p.retail);
  const arb = make(p.arbitrageur);
  let oracle = p.initialPrice;
  let amm = p.initialPrice;
  let lastDividend = 0;
  const out = {
    steps: p.steps, price_series: [], oracle_series: [], holder_distribution: [],
    dividend_yield: [], liquidity_depth: [], final_gini: 0, max_drawdown: 0,
  };
  const sampleEvery = Math.max(1, Math.floor(p.steps / 40));

  for (let t = 0; t < p.steps; t += 1) {
    const dt = 1 / 252;
    oracle = clamp(oracle * (1 + p.drift * dt + p.volatility * rng.normal() * Math.sqrt(dt)), PMIN, PMAX);
    const past = out.price_series[Math.max(0, out.price_series.length - p.momentumWindow)] ?? amm;
    const momentum = (amm - past) / Math.max(PMIN, past);
    const depth = new Array(8).fill(0);
    let flow = 0;
    const order = (agent, amount, buy) => {
      const bounded = Math.max(0, Math.min(amount, buy ? agent.cash : agent.shares * amm));
      if (bounded <= 0) return;
      if (buy) { agent.cash -= bounded; agent.shares += bounded / amm; }
      else { agent.shares -= bounded / amm; agent.cash += bounded; }
      flow += buy ? bounded : -bounded;
      const bucket = Math.round(clamp(((amm / Math.max(PMIN, oracle) - 0.9) / 0.2) * 8, 0, 7));
      depth[bucket] += bounded;
    };
    const ratio = amm / Math.max(PMIN, oracle);
    for (const a of inst) {
      if (ratio < 1 - p.dipThreshold) order(a, a.cash * 0.05, true);
      else if (ratio > 1 + p.dipThreshold) order(a, a.shares * amm * 0.05, false);
    }
    for (const a of retail) {
      if (momentum > 0) order(a, a.cash * 0.02, true);
      else if (momentum < 0) order(a, a.shares * amm * 0.02, false);
    }
    for (const a of arb) {
      const gap = (oracle - amm) / Math.max(PMIN, oracle);
      if (Math.abs(gap) > p.arbitrageTolerance) order(a, Math.abs(gap) * p.liquidityDepth * 0.1, gap > 0);
    }
    amm = clamp(amm * (1 + clamp(flow / p.liquidityDepth, -0.5, 0.5)), PMIN, PMAX);
    amm += (oracle - amm) * 0.05;
    amm = clamp(amm, PMIN, PMAX);
    if (rng.poissonHit(p.dividendLambda)) {
      lastDividend = oracle * p.dividendRate;
      for (const a of [...inst, ...retail, ...arb]) a.cash += a.shares * lastDividend;
    }
    out.price_series.push(amm);
    out.oracle_series.push(oracle);
    out.dividend_yield.push(clamp(lastDividend / Math.max(PMIN, amm), 0, 10));
    if (t % sampleEvery === 0) {
      const agents = [...inst, ...retail, ...arb];
      const wealth = agents.map((a) => a.cash + a.shares * amm).sort((a, b) => a - b);
      const median = Math.max(PMIN, wealth[Math.floor(wealth.length / 2)] ?? 1);
      const bins = new Array(8).fill(0);
      for (const value of wealth) {
        bins[Math.min(7, Math.round(Math.log2(clamp(value / median, 1e-4, 16)) + 4))] += 1;
      }
      out.holder_distribution.push(bins);
      out.liquidity_depth.push(depth);
    }
    if ((t + 1) % Math.max(1, Math.floor(p.steps / 50)) === 0) {
      onProgress((t + 1) / p.steps);
    }
  }
  const agents = [...inst, ...retail, ...arb];
  const wealth = agents.map((a) => a.cash + a.shares * amm).sort((a, b) => a - b);
  const n = wealth.length;
  const sum = wealth.reduce((a, b) => a + b, 0);
  if (n && sum > 0) {
    const weighted = wealth.reduce((acc, v, i) => acc + (i + 1) * v, 0);
    out.final_gini = clamp((2 * weighted) / (n * sum) - (n + 1) / n, 0, 1);
  }
  let peak = PMIN;
  for (const price of out.price_series) {
    peak = Math.max(peak, price);
    out.max_drawdown = Math.max(out.max_drawdown, (peak - price) / Math.max(PMIN, peak));
  }
  return out;
}

self.onmessage = async (event) => {
  const { scenario } = event.data || {};
  const progress = (value) => self.postMessage({ type: "progress", value });
  try {
    let output;
    let engine;
    try {
      output = await runWasm(scenario, progress);
      engine = "wasm";
    } catch (error) {
      self.postMessage({ type: "notice", message: `WASM unavailable, using reference engine (${error.message})` });
      output = runReference(scenario, progress);
      engine = "typescript";
    }
    progress(1);
    self.postMessage({ type: "result", output, engine });
  } catch (error) {
    self.postMessage({ type: "error", message: String(error) });
  }
};
