import {
  MAX_COMPARED_ASSETS,
  SEC_EDGAR_GOLDEN_FIXTURES,
  buildComparisonCsv,
  buildComparisonRows,
  buildSignedEdgarComparisonPdf,
  classifyChangeHighlight,
  concentrationBand,
  csvField,
  giniCoefficient,
  inferDividendSchedule,
  parseEdgarForm4Csv,
  reconcileCapTableWithEdgar,
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

describe("SEC EDGAR Form 4 Cap-Table Reconciliation (Issue #476)", () => {
  it("parses SEC EDGAR Form 4 CSV golden dataset records", () => {
    const records = parseEdgarForm4Csv(SEC_EDGAR_GOLDEN_FIXTURES.csv);
    expect(records).toHaveLength(4);
    expect(records[0]).toMatchObject({
      cik: "0001849821",
      accessionNumber: "0001849821-26-000041",
      holderName: "Apex Institutional Partners LP",
      shares: 400_000,
    });
  });

  it("classifies balance changes into none (<=5%), yellow (>5%), and red (>10%)", () => {
    expect(classifyChangeHighlight(0)).toBe("none");
    expect(classifyChangeHighlight(5.0)).toBe("none");
    expect(classifyChangeHighlight(5.01)).toBe("yellow");
    expect(classifyChangeHighlight(-7.5)).toBe("yellow");
    expect(classifyChangeHighlight(10.0)).toBe("yellow");
    expect(classifyChangeHighlight(10.01)).toBe("red");
    expect(classifyChangeHighlight(-15.0)).toBe("red");
  });

  it("detects new holders, departed holders, changed balances, yellow/red alerts, and triggers >1% supply compliance webhook", () => {
    const edgarRecords = parseEdgarForm4Csv(SEC_EDGAR_GOLDEN_FIXTURES.csv);
    const report = reconcileCapTableWithEdgar(
      SEC_EDGAR_GOLDEN_FIXTURES.onChainHolders,
      edgarRecords,
      {
        assetId: SEC_EDGAR_GOLDEN_FIXTURES.assetId,
        cik: SEC_EDGAR_GOLDEN_FIXTURES.cik,
        totalSupply: SEC_EDGAR_GOLDEN_FIXTURES.totalSupply,
        generatedAt: "2026-11-23T12:00:00.000Z",
      }
    );

    expect(report.newHoldersCount).toBe(1);
    expect(report.departedHoldersCount).toBe(1);
    expect(report.changedBalancesCount).toBe(2);
    expect(report.unchangedCount).toBe(1);

    // Apex (+7.5%) -> yellow; Beacon (-15%), Delta (-100% departed), Emerald (+100% new) -> 3 red
    expect(report.yellowAlertCount).toBe(1);
    expect(report.redAlertCount).toBe(3);

    const apex = report.rows.find((r) => r.holderName.includes("Apex"))!;
    expect(apex.status).toBe("changed_balance");
    expect(apex.absoluteChange).toBe(30_000);
    expect(apex.percentChange).toBeCloseTo(7.5, 2);
    expect(apex.highlight).toBe("yellow");

    const beacon = report.rows.find((r) => r.holderName.includes("Beacon"))!;
    expect(beacon.status).toBe("changed_balance");
    expect(beacon.absoluteChange).toBe(-37_500);
    expect(beacon.percentChange).toBeCloseTo(-15.0, 2);
    expect(beacon.highlight).toBe("red");

    const delta = report.rows.find((r) => r.holderName.includes("Delta"))!;
    expect(delta.status).toBe("departed_holder");
    expect(delta.onChainShares).toBe(0);
    expect(delta.highlight).toBe("red");

    const emerald = report.rows.find((r) => r.holderName.includes("Emerald"))!;
    expect(emerald.status).toBe("new_holder");
    expect(emerald.edgarShares).toBe(0);
    expect(emerald.onChainShares).toBe(45_000);
    expect(emerald.highlight).toBe("red");

    // Total discrepancy = 30k + 37.5k + 50k + 45k = 162,500 shares = 16.25% of 1M supply (> 1%)
    expect(report.totalDiscrepancyShares).toBe(162_500);
    expect(report.totalDiscrepancyPercentOfSupply).toBeCloseTo(16.25, 2);
    expect(report.complianceWebhookTriggered).toBe(true);
    expect(report.webhookPayload).not.toBeNull();
    expect(report.webhookPayload?.event).toBe("cap_table.edgar_discrepancy");
    expect(report.webhookPayload?.flaggedHolders).toHaveLength(4);
    expect(report.signature).toHaveLength(64);
  });

  it("does not trigger compliance webhook when discrepancy is <= 1% of total supply", () => {
    const edgar = parseEdgarForm4Csv(
      [
        "cik,accession_number,filing_date,reporting_owner,stellar_address,shares_owned_following_transaction",
        "0001849821,0001849821-26-000001,2026-10-01,Minor Holder,GMINORHOLDER11111111111111111111111111111111111111111111,100000",
      ].join("\n")
    );
    const report = reconcileCapTableWithEdgar(
      [{ address: "GMINORHOLDER11111111111111111111111111111111111111111111", balance: "100500" }],
      edgar,
      { totalSupply: 1_000_000 }
    );
    // 500 / 1,000,000 = 0.05% of total supply (<= 1.0%)
    expect(report.totalDiscrepancyPercentOfSupply).toBeCloseTo(0.05, 4);
    expect(report.complianceWebhookTriggered).toBe(false);
    expect(report.webhookPayload).toBeNull();
  });

  it("exports a cryptographically signed %PDF-1.4 binary report", () => {
    const edgarRecords = parseEdgarForm4Csv(SEC_EDGAR_GOLDEN_FIXTURES.csv);
    const report = reconcileCapTableWithEdgar(
      SEC_EDGAR_GOLDEN_FIXTURES.onChainHolders,
      edgarRecords,
      { totalSupply: SEC_EDGAR_GOLDEN_FIXTURES.totalSupply }
    );

    const pdfBytes = buildSignedEdgarComparisonPdf(report);
    const pdfText = new TextDecoder().decode(pdfBytes);

    expect(pdfText.startsWith("%PDF-1.4\n")).toBe(true);
    expect(pdfText).toContain("TESSERA INSTITUTIONAL CAP TABLE RECONCILIATION REPORT");
    expect(pdfText).toContain("DIGITAL AUDIT SIGNATURE");
    expect(pdfText).toContain(report.signature);
    expect(pdfText).toContain("Apex Institutional Partners LP");
    expect(pdfText.trimEnd().endsWith("%%EOF")).toBe(true);
  });
});
