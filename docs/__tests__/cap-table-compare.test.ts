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

// ── Issue #476: on-chain cap table vs EDGAR Form 4 ──────────────────────────

import { readFileSync } from "fs";
import { join } from "path";
import {
  CsvFormatError,
  highlightFor,
  parseCsv,
  parseFilingCsv,
  reconcile,
} from "../lib/cap-table-reconcile";
import { buildSignedPdf, canonicalJson } from "../lib/signed-pdf";

/** Synthetic EDGAR golden dataset; see api/tests/fixtures/edgar/README.md. */
const FORM4_CSV = readFileSync(join(__dirname, "fixtures", "edgar-form4-holders.csv"), "utf8");
const DOE = "GAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAI";
const ROE = "GAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQ";
const SMITH = "GAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAY";
const NEWCOMER = "GAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBA";

describe("EDGAR Form 4 CSV", () => {
  it("parses the golden fixture, including quoted thousands", () => {
    const filing = parseFilingCsv(FORM4_CSV);
    expect(filing).toEqual([
      { address: DOE, ownerCik: "0009990101", ownerName: "DOE JANE", shares: 115_000 },
      { address: ROE, ownerCik: "0009990102", ownerName: "ROE RICHARD", shares: 40_000 },
      { address: SMITH, ownerCik: "0009990103", ownerName: "SMITH JOHN", shares: 60_000 },
    ]);
  });

  it("handles quotes, CRLF and blank lines", () => {
    expect(parseCsv('a,"b ""c""",d\r\n\r\n1,2,3\n')).toEqual([["a", 'b "c"', "d"], ["1", "2", "3"]]);
  });

  it("rejects files without the required columns or with bad numbers", () => {
    expect(() => parseFilingCsv("name,shares\nx,1")).toThrow(CsvFormatError);
    expect(() => parseFilingCsv("address,shares\nGA,-5")).toThrow("Row 2 has an invalid share count.");
    expect(() => parseFilingCsv("address,shares\nGA,")).toThrow(CsvFormatError);
  });

  it("sums several rows for one address", () => {
    expect(parseFilingCsv("address,shares\nGA,10\nGA,5")[0].shares).toBe(15);
  });
});

describe("reconcile on-chain holders against EDGAR", () => {
  // Seven decimals on chain, 1 000 000 tokens of supply.
  const units = (tokens: number) => String(BigInt(tokens) * 10_000_000n);
  const onChain = [
    { address: DOE, balance: units(115_000) }, // unchanged
    { address: ROE, balance: units(44_100) }, // +10.25 %: red
    { address: NEWCOMER, balance: units(12_000) }, // not in the filing
  ];
  const report = reconcile(onChain, parseFilingCsv(FORM4_CSV), {
    assetId: "1",
    decimals: 7,
    totalSupply: units(1_000_000),
    now: new Date("2026-11-24T00:00:00Z"),
  });
  const row = (a: string) => report.rows.find((r) => r.address === a)!;

  it("detects new, departed and changed holders", () => {
    expect(row(NEWCOMER).kind).toBe("new");
    expect(row(SMITH).kind).toBe("departed");
    expect(row(SMITH).absoluteChange).toBe(-60_000);
    expect(row(ROE).kind).toBe("changed");
    expect(row(ROE).absoluteChange).toBeCloseTo(4_100, 6);
    expect(row(ROE).percentChange).toBeCloseTo(10.25, 6);
    expect(row(DOE).kind).toBe("unchanged");
    expect(report.counts).toEqual({ new: 1, departed: 1, changed: 1, unchanged: 1 });
    expect(report.rows.map((r) => r.kind)).toEqual(["new", "departed", "changed", "unchanged"]);
  });

  it("highlights over 5 % yellow and over 10 % red", () => {
    expect(highlightFor(5)).toBe("none");
    expect(highlightFor(5.5)).toBe("yellow");
    expect(highlightFor(10)).toBe("yellow");
    expect(highlightFor(-10.01)).toBe("red");
    expect(row(ROE).highlight).toBe("red");
    expect(row(DOE).highlight).toBe("none");
  });

  it("flags discrepancies over 1 % of total supply for compliance", () => {
    expect(row(SMITH).complianceAlert).toBe(true); // 6 %
    expect(row(NEWCOMER).complianceAlert).toBe(true); // 1.2 %
    expect(row(ROE).complianceAlert).toBe(false); // 0.41 %
    expect(report.complianceAlerts).toBe(2);
  });

  it("applies the token-per-share ratio", () => {
    const r = reconcile([{ address: DOE, balance: "230000" }], [{ address: DOE, ownerCik: null, ownerName: null, shares: 115_000 }], {
      assetId: "1",
      decimals: 0,
      totalSupply: "1000000",
      tokensPerShare: 2,
    });
    expect(r.rows[0].kind).toBe("unchanged");
  });
});

describe("signed PDF export", () => {
  const report = { b: 1, a: { d: [1, "x"], c: null } };
  const signature = {
    algorithm: "ed25519-sha256",
    digest: "ab".repeat(32),
    signature: "cd".repeat(64),
    publicKey: "ef".repeat(32),
    signedAt: "2026-11-24T00:00:00Z",
  };

  it("canonicalizes like the API: sorted keys, no whitespace", () => {
    expect(canonicalJson(report)).toBe('{"a":{"c":null,"d":[1,"x"]},"b":1}');
  });

  it("writes a PDF with the signature block and the signed JSON attached", () => {
    const pdf = buildSignedPdf({ title: "Reconciliation (asset 1)", lines: ["Row one"], signature, signedJson: canonicalJson(report) });
    const text = Buffer.from(pdf).toString("latin1");
    expect(text.startsWith("%PDF-1.4")).toBe(true);
    expect(text.trimEnd().endsWith("%%EOF")).toBe(true);
    expect(text).toContain(signature.digest);
    expect(text).toContain("(report.json)");
    expect(text).toContain('{"a":{"c":null,"d":[1,"x"]},"b":1}');
    expect(text).toContain("Reconciliation \\(asset 1\\)");

    // Every xref offset points at its object header.
    const xrefAt = Number(text.slice(text.lastIndexOf("startxref") + 10).trim().split("\n")[0]);
    const entries = text.slice(xrefAt).split("\n").slice(3).filter((l) => / n $/.test(l));
    entries.forEach((entry, i) => {
      expect(text.slice(Number(entry.slice(0, 10)), Number(entry.slice(0, 10)) + 12)).toMatch(new RegExp(`^${i + 1} 0 obj`));
    });
  });

  it("paginates long reports", () => {
    const pdf = buildSignedPdf({ title: "t", lines: Array.from({ length: 200 }, (_, i) => `line ${i}`), signature, signedJson: "{}" });
    expect(Buffer.from(pdf).toString("latin1")).toMatch(/\/Count [3-9]/);
  });
});
