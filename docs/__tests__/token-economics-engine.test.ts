import {
  DEFAULT_PARAMS,
  parseScenario,
  TokenEconomicsEngine,
  type Scenario,
} from "@/lib/token-economics-engine";

const scenario = (overrides: Partial<Scenario["params"]> = {}): Scenario => ({
  version: 1,
  name: "test",
  params: { ...DEFAULT_PARAMS, steps: 120, ...overrides },
});

describe("token-economics reference engine", () => {
  it("is deterministic for a seed", () => {
    const a = new TokenEconomicsEngine(scenario()).run();
    const b = new TokenEconomicsEngine(scenario()).run();
    expect(a).toEqual(b);
  });

  it("reports monotonic progress and reaches one", () => {
    const engine = new TokenEconomicsEngine(scenario({ steps: 10 }));
    let last = 0;
    while (!engine.isDone()) {
      engine.step();
      expect(engine.progress()).toBeGreaterThanOrEqual(last);
      last = engine.progress();
    }
    expect(last).toBe(1);
  });

  it("stays finite under extreme parameter values", () => {
    const extremes = [
      { volatility: 2, drift: -1, steps: 400 },
      { volatility: 0, liquidityDepth: 1e-3, steps: 400 },
      { initialPrice: 1e-6, dividendRate: 2, steps: 400 },
      { institutional: 0, retail: 0, arbitrageur: 0, steps: 400 },
    ];
    for (const overrides of extremes) {
      const output = new TokenEconomicsEngine(scenario(overrides)).run();
      const values = [
        ...output.price_series,
        ...output.oracle_series,
        ...output.dividend_yield,
        output.final_gini,
        output.max_drawdown,
      ];
      for (const value of values) {
        expect(Number.isFinite(value)).toBe(true);
        expect(value).toBeGreaterThanOrEqual(0);
      }
      expect(output.final_gini).toBeLessThanOrEqual(1);
      expect(output.max_drawdown).toBeLessThanOrEqual(1);
    }
  });

  it("clamps unsafe imported scenario values", () => {
    const parsed = parseScenario({
      name: "hostile",
      params: {
        steps: 1e9,
        initialPrice: -5,
        volatility: Number.NaN,
        liquidityDepth: 0,
        arbitrageur: 100000,
      },
    });
    expect(parsed.params.steps).toBe(10_000);
    expect(parsed.params.initialPrice).toBeGreaterThan(0);
    expect(parsed.params.volatility).toBe(0);
    expect(parsed.params.liquidityDepth).toBeGreaterThan(0);
    expect(parsed.params.arbitrageur).toBe(2_000);
  });
});
