import {
  MAX_COMPARED_ASSETS,
  buildComparisonCsv,
  buildComparisonRows,
  concentrationBand,
  csvField,
  giniCoefficient,
  inferDividendSchedule,
  trailingAnnualYieldPercent,
  type CompareAsset,
} from "../lib/cap-table-compare";

const LEDGERS_PER_DAY = 17_280;

function asset(overrides: Partial<CompareAsset> & Pick<CompareAsset, "id" | "symbol">): CompareAsset {
  return {
    name: overrides.symbol,
    assetType: "real_estate",
    valuationUsd: 1_000_000,
    holderCount: 100,
    giniCoefficient: 0.5,
    topHolderSharePercent: 10,
    minInvestmentUsd: 1_000,
    annualYieldPercent: 5,
    dividendSchedule: { frequency: "quarterly", distributions: 4, medianIntervalDays: 91 },
    compliance: { jurisdictions: ["US"], accreditedOnly: true, lockupDays: 90, approvedPercent: 95 },
    ...overrides,
  };
}

describe("giniCoefficient", () => {
  it("is 0 for perfectly equal holdings", () => {
    expect(giniCoefficient([10, 10, 10, 10])).toBeCloseTo(0, 10);
  });

  it("approaches (n-1)/n when one holder owns almost everything", () => {
    expect(giniCoefficient([0, 0, 0, 100])).toBeCloseTo(0, 10); // zero balances are not holders
    // (2988 / 4000): close to the (n-1)/n = 0.75 ceiling for four holders.
    expect(giniCoefficient([1, 1, 1, 997])).toBeCloseTo(0.747, 4);
  });

  it("matches the textbook value for a known distribution", () => {
    // Mean absolute difference / (2 * mean) for [1, 2, 3, 4, 5] = 0.2667.
    expect(giniCoefficient([5, 3, 1, 4, 2])).toBeCloseTo(0.2667, 4);
  });

  it("is undefined without positive balances", () => {
    expect(giniCoefficient([])).toBeNull();
    expect(giniCoefficient([0, -5, Number.NaN])).toBeNull();
  });
});

describe("concentrationBand", () => {
  it("buckets Gini values", () => {
    expect(concentrationBand(0.2)).toBe("low");
    expect(concentrationBand(0.45)).toBe("moderate");
    expect(concentrationBand(0.8)).toBe("high");
    expect(concentrationBand(null)).toBe("unknown");
  });
});

describe("inferDividendSchedule", () => {
  const every = (days: number, count: number) => Array.from({ length: count }, (_, i) => 1_000 + i * days * LEDGERS_PER_DAY);

  it.each([
    [30, "monthly"],
    [91, "quarterly"],
    [182, "semi-annual"],
    [365, "annual"],
    [12, "irregular"],
  ] as const)("classifies a %i-day cadence as %s", (days, frequency) => {
    const schedule = inferDividendSchedule(every(days, 5));
    expect(schedule.frequency).toBe(frequency);
    expect(schedule.distributions).toBe(5);
    expect(schedule.medianIntervalDays).toBeCloseTo(days, 0);
  });

  it("handles zero and single distributions", () => {
    expect(inferDividendSchedule([])).toEqual({ frequency: "none", distributions: 0, medianIntervalDays: null });
    expect(inferDividendSchedule([42]).frequency).toBe("irregular");
  });

  it("uses the median so one late payment does not change the cadence", () => {
    const ledgers = [...every(91, 4), 1_000 + (3 * 91 + 140) * LEDGERS_PER_DAY];
    expect(inferDividendSchedule(ledgers).frequency).toBe("quarterly");
  });
});

describe("trailingAnnualYieldPercent", () => {
  const now = 400 * LEDGERS_PER_DAY;

  it("sums payouts in the last 365 days against the valuation", () => {
    const yieldPct = trailingAnnualYieldPercent(
      [
        { createdAtLedger: now - 10 * LEDGERS_PER_DAY, fiatEquivalentUsd: 25_000 },
        { createdAtLedger: now - 200 * LEDGERS_PER_DAY, fiatEquivalentUsd: 25_000 },
        { createdAtLedger: now - 380 * LEDGERS_PER_DAY, fiatEquivalentUsd: 99_000 },
      ],
      1_000_000,
      now
    );
    expect(yieldPct).toBeCloseTo(5, 10);
  });

  it("refuses to report a partial yield when a recent payout is unpriced", () => {
    expect(trailingAnnualYieldPercent([{ createdAtLedger: now - 1, fiatEquivalentUsd: null }], 1_000_000, now)).toBeNull();
    expect(trailingAnnualYieldPercent([], 0, now)).toBeNull();
  });
});

describe("buildComparisonRows", () => {
  const assets = [
    asset({ id: "a", symbol: "AAA", giniCoefficient: 0.4, minInvestmentUsd: 500 }),
    asset({ id: "b", symbol: "BBB", giniCoefficient: 0.7, minInvestmentUsd: 5_000, dividendSchedule: { frequency: "monthly", distributions: 12, medianIntervalDays: 30 } }),
    asset({ id: "c", symbol: "CCC", giniCoefficient: 0.4, minInvestmentUsd: null }),
  ];
  const rows = buildComparisonRows(assets);
  const row = (key: string) => rows.find((r) => r.metric.key === key)!;

  it("marks cells that differ from the baseline with a signed delta", () => {
    const gini = row("giniCoefficient");
    expect(gini.hasDifference).toBe(true);
    expect(gini.cells.map((c) => c.differs)).toEqual([false, true, false]);
    expect(gini.cells[1].delta).toBe("+0.300");
    expect(gini.metric.emphasised).toBe(true);
  });

  it("ranks best and worst by the metric's favourable direction", () => {
    const gini = row("giniCoefficient");
    expect(gini.cells.map((c) => c.rank)).toEqual(["best", "worst", "best"]);
  });

  it("does not rank when a value is unknown, and labels it Not published", () => {
    const min = row("minInvestmentUsd");
    expect(min.cells.map((c) => c.rank)).toEqual([null, null, null]);
    expect(min.cells[2].display).toBe("Not published");
    expect(min.cells[1].delta).toBe("+$4,500");
  });

  it("compares dividend schedules categorically", () => {
    const schedule = row("dividendFrequency");
    expect(schedule.cells.map((c) => c.display)).toEqual(["Quarterly", "Monthly", "Quarterly"]);
    expect(schedule.cells.map((c) => c.differs)).toEqual([false, true, false]);
    expect(schedule.cells[1].delta).toBeNull();
  });

  it("reports no differences or ranks for a single asset", () => {
    const single = buildComparisonRows([assets[0]]);
    expect(single.every((r) => !r.hasDifference && r.cells[0].rank === null)).toBe(true);
  });

  it("caps comparisons at four assets in the UI contract", () => {
    expect(MAX_COMPARED_ASSETS).toBe(4);
  });
});

describe("CSV export", () => {
  it("quotes fields and neutralises formula injection", () => {
    expect(csvField("plain")).toBe("plain");
    expect(csvField('He said "hi", twice')).toBe('"He said ""hi"", twice"');
    expect(csvField("=HYPERLINK(\"x\")")).toBe("\"'=HYPERLINK(\"\"x\"\")\"");
    expect(csvField("-1")).toBe("'-1");
    expect(csvField("@cmd")).toBe("'@cmd");
  });

  it("writes one metric per row and one asset per column", () => {
    const csv = buildComparisonCsv([
      asset({ id: "a", symbol: "AAA", name: "Alpha, Inc." }),
      asset({ id: "b", symbol: "BBB", giniCoefficient: 0.61 }),
    ]);
    const lines = csv.trimEnd().split("\r\n");
    expect(lines[0]).toBe('Metric,"Alpha, Inc. (AAA)",BBB (BBB)');
    expect(lines).toContain("Gini coefficient,0.500,0.610");
    expect(lines).toContain("Dividend schedule,Quarterly,Quarterly");
    expect(csv.endsWith("\r\n")).toBe(true);
  });
});
