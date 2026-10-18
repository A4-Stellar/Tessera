/**
 * Pure helpers behind `CapTableCompare` (issue #241): concentration metrics,
 * dividend-schedule inference, cross-asset diffing and CSV export. Kept free of
 * React so they can be unit tested and reused by other views.
 */

/** Maximum number of assets compared side by side. */
export const MAX_COMPARED_ASSETS = 4;

/** Stellar closes a ledger roughly every five seconds. */
export const SECONDS_PER_LEDGER = 5;
const LEDGERS_PER_DAY = (24 * 60 * 60) / SECONDS_PER_LEDGER;

export type DividendFrequency =
  | "monthly"
  | "quarterly"
  | "semi-annual"
  | "annual"
  | "irregular"
  | "none";

export interface DividendSchedule {
  frequency: DividendFrequency;
  /** Number of distributions observed. */
  distributions: number;
  /** Median days between distributions, when at least two exist. */
  medianIntervalDays: number | null;
}

/** Compliance rules that gate who can hold the asset. */
export interface ComplianceRules {
  /** Jurisdictions with approved holders (ISO codes or region labels). */
  jurisdictions: string[];
  /** Whether only accredited / professional investors may hold the token. */
  accreditedOnly: boolean | null;
  /** Transfer lock-up after issuance, in days (0 = none, null = not published). */
  lockupDays: number | null;
  /** Share of allowlist records that are approved, 0–100. */
  approvedPercent: number | null;
}

/** One asset as shown in the comparison grid. Unknown values are `null`, never guessed. */
export interface CompareAsset {
  id: string;
  name: string;
  symbol: string;
  assetType: string;
  valuationUsd: number | null;
  holderCount: number;
  /** Gini coefficient of holder balances, 0 (equal) – 1 (one holder owns everything). */
  giniCoefficient: number | null;
  /** Largest single holder's share of supply, 0–100. */
  topHolderSharePercent: number | null;
  /** Smallest ticket an investor can buy, in USD. */
  minInvestmentUsd: number | null;
  /** Where `minInvestmentUsd` comes from; shown next to the value. */
  minInvestmentBasis?: "issuer" | "one-token";
  /** Trailing twelve-month dividend yield, 0–100. */
  annualYieldPercent: number | null;
  dividendSchedule: DividendSchedule;
  compliance: ComplianceRules;
}

/**
 * Gini coefficient of non-negative balances, using the sorted-rank formula
 * G = Σ(2i − n − 1)·xᵢ / (n·Σx). Returns `null` when it is undefined
 * (no holders or zero supply). O(n log n).
 */
export function giniCoefficient(balances: readonly number[]): number | null {
  const values = balances.filter((v) => Number.isFinite(v) && v > 0).sort((a, b) => a - b);
  const n = values.length;
  if (n === 0) return null;
  const total = values.reduce((sum, v) => sum + v, 0);
  if (total <= 0) return null;
  let weighted = 0;
  values.forEach((value, index) => {
    weighted += (2 * (index + 1) - n - 1) * value;
  });
  return Math.min(1, Math.max(0, weighted / (n * total)));
}

/** Qualitative band for a Gini coefficient. */
export function concentrationBand(gini: number | null): "low" | "moderate" | "high" | "unknown" {
  if (gini === null) return "unknown";
  if (gini < 0.4) return "low";
  if (gini < 0.6) return "moderate";
  return "high";
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid];
}

/**
 * Infers a dividend cadence from the ledgers at which distributions were
 * created. Intervals within ±25% of a calendar period map to that period;
 * anything else — or a single payment — is "irregular".
 */
export function inferDividendSchedule(createdAtLedgers: readonly number[]): DividendSchedule {
  const ledgers = [...createdAtLedgers].filter((l) => Number.isFinite(l)).sort((a, b) => a - b);
  if (ledgers.length === 0) return { frequency: "none", distributions: 0, medianIntervalDays: null };
  if (ledgers.length === 1) return { frequency: "irregular", distributions: 1, medianIntervalDays: null };

  const gaps = ledgers.slice(1).map((ledger, i) => (ledger - ledgers[i]) / LEDGERS_PER_DAY);
  const days = median(gaps);
  const periods: Array<[DividendFrequency, number]> = [
    ["monthly", 30.44],
    ["quarterly", 91.31],
    ["semi-annual", 182.62],
    ["annual", 365.25],
  ];
  const match = periods.find(([, period]) => Math.abs(days - period) <= period * 0.25);
  return {
    frequency: match ? match[0] : "irregular",
    distributions: ledgers.length,
    medianIntervalDays: Math.round(days * 10) / 10,
  };
}

export interface DistributionSample {
  createdAtLedger: number;
  /** USD value of the distribution, when the API could price it. */
  fiatEquivalentUsd: number | null;
}

/**
 * Trailing twelve-month yield: USD paid in the last 365 days of ledgers divided
 * by the current valuation. `null` when the valuation or any recent payout is
 * unpriced, so a partial sum is never presented as the yield.
 */
export function trailingAnnualYieldPercent(
  distributions: readonly DistributionSample[],
  valuationUsd: number | null,
  currentLedger: number
): number | null {
  if (!valuationUsd || valuationUsd <= 0) return null;
  const windowStart = currentLedger - 365 * LEDGERS_PER_DAY;
  const recent = distributions.filter((d) => d.createdAtLedger >= windowStart && d.createdAtLedger <= currentLedger);
  if (recent.some((d) => d.fiatEquivalentUsd === null)) return null;
  const paid = recent.reduce((sum, d) => sum + (d.fiatEquivalentUsd ?? 0), 0);
  return (paid / valuationUsd) * 100;
}

// ── Comparison rows ─────────────────────────────────────────────────────────

export type MetricKey =
  | "assetType"
  | "valuationUsd"
  | "holderCount"
  | "giniCoefficient"
  | "topHolderSharePercent"
  | "minInvestmentUsd"
  | "annualYieldPercent"
  | "dividendFrequency"
  | "dividendCount"
  | "jurisdictions"
  | "accreditedOnly"
  | "lockupDays"
  | "approvedPercent";

export interface MetricDefinition {
  key: MetricKey;
  label: string;
  group: "Profile" | "Concentration" | "Investment & yield" | "Dividends" | "Compliance";
  /** Which direction is favourable for an investor, for numeric metrics. */
  better?: "higher" | "lower";
  /** Metrics the issue calls out are emphasised when they differ. */
  emphasised?: boolean;
  value: (asset: CompareAsset) => number | string | boolean | null;
  format: (value: number | string | boolean | null) => string;
}

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
const usdCents = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 2 });
const NOT_PUBLISHED = "Not published";

function fmtNumber(digits: number, suffix = "") {
  return (v: number | string | boolean | null) =>
    typeof v === "number" ? `${v.toFixed(digits)}${suffix}` : NOT_PUBLISHED;
}

const FREQUENCY_LABELS: Record<DividendFrequency, string> = {
  monthly: "Monthly",
  quarterly: "Quarterly",
  "semi-annual": "Semi-annual",
  annual: "Annual",
  irregular: "Irregular",
  none: "No distributions",
};

export const METRICS: MetricDefinition[] = [
  { key: "assetType", label: "Asset type", group: "Profile", value: (a) => a.assetType, format: (v) => String(v ?? NOT_PUBLISHED).replace(/_/g, " ") },
  { key: "valuationUsd", label: "Valuation", group: "Profile", value: (a) => a.valuationUsd, format: (v) => (typeof v === "number" ? usd.format(v) : NOT_PUBLISHED) },
  { key: "holderCount", label: "Holders", group: "Concentration", better: "higher", value: (a) => a.holderCount, format: (v) => (typeof v === "number" ? v.toLocaleString("en-US") : NOT_PUBLISHED) },
  { key: "giniCoefficient", label: "Gini coefficient", group: "Concentration", better: "lower", emphasised: true, value: (a) => a.giniCoefficient, format: fmtNumber(3) },
  { key: "topHolderSharePercent", label: "Largest holder share", group: "Concentration", better: "lower", value: (a) => a.topHolderSharePercent, format: fmtNumber(1, "%") },
  {
    key: "minInvestmentUsd",
    label: "Minimum investment",
    group: "Investment & yield",
    better: "lower",
    emphasised: true,
    value: (a) => a.minInvestmentUsd,
    format: (v) => (typeof v === "number" ? (v < 100 ? usdCents.format(v) : usd.format(v)) : NOT_PUBLISHED),
  },
  { key: "annualYieldPercent", label: "Dividend yield (TTM)", group: "Investment & yield", better: "higher", value: (a) => a.annualYieldPercent, format: fmtNumber(2, "%") },
  {
    key: "dividendFrequency",
    label: "Dividend schedule",
    group: "Dividends",
    emphasised: true,
    value: (a) => a.dividendSchedule.frequency,
    format: (v) => FREQUENCY_LABELS[(v as DividendFrequency) ?? "none"] ?? String(v),
  },
  { key: "dividendCount", label: "Distributions paid", group: "Dividends", value: (a) => a.dividendSchedule.distributions, format: (v) => String(v ?? 0) },
  {
    key: "jurisdictions",
    label: "Jurisdictions",
    group: "Compliance",
    value: (a) => [...a.compliance.jurisdictions].sort().join(", ") || null,
    format: (v) => (typeof v === "string" && v ? v : NOT_PUBLISHED),
  },
  {
    key: "accreditedOnly",
    label: "Accredited investors only",
    group: "Compliance",
    value: (a) => a.compliance.accreditedOnly,
    format: (v) => (v === null ? NOT_PUBLISHED : v ? "Yes" : "No"),
  },
  { key: "lockupDays", label: "Lock-up period", group: "Compliance", better: "lower", value: (a) => a.compliance.lockupDays, format: (v) => (typeof v === "number" ? (v === 0 ? "None" : `${v} days`) : NOT_PUBLISHED) },
  { key: "approvedPercent", label: "Approved allowlist records", group: "Compliance", better: "higher", value: (a) => a.compliance.approvedPercent, format: fmtNumber(1, "%") },
];

export interface ComparisonCell {
  value: number | string | boolean | null;
  display: string;
  /** Differs from the first (baseline) asset. */
  differs: boolean;
  /** Best / worst among the compared assets, for metrics with a direction. */
  rank: "best" | "worst" | null;
  /** Signed numeric difference from the baseline, formatted, e.g. "+0.120". */
  delta: string | null;
}

export interface ComparisonRow {
  metric: MetricDefinition;
  cells: ComparisonCell[];
  /** True when at least one asset differs from the baseline. */
  hasDifference: boolean;
}

function deltaText(metric: MetricDefinition, value: number, base: number): string | null {
  const diff = value - base;
  if (diff === 0) return null;
  const sign = diff > 0 ? "+" : "−";
  const abs = Math.abs(diff);
  switch (metric.key) {
    case "giniCoefficient":
      return `${sign}${abs.toFixed(3)}`;
    case "minInvestmentUsd":
    case "valuationUsd":
      return `${sign}${(abs < 100 ? usdCents : usd).format(abs)}`;
    case "lockupDays":
      return `${sign}${abs} days`;
    case "holderCount":
      return `${sign}${abs.toLocaleString("en-US")}`;
    default:
      return `${sign}${abs.toFixed(metric.key === "annualYieldPercent" ? 2 : 1)} pp`;
  }
}

/**
 * Builds one row per metric with baseline diffs (first asset) and best/worst
 * markers. Ranks are only assigned when every compared value is known and at
 * least two distinct values exist.
 */
export function buildComparisonRows(assets: readonly CompareAsset[]): ComparisonRow[] {
  return METRICS.map((metric) => {
    const values = assets.map((asset) => metric.value(asset));
    const base = values[0];
    const numeric = values.every((v) => typeof v === "number") ? (values as number[]) : null;
    const distinct = new Set(values.map((v) => JSON.stringify(v)));
    const canRank = Boolean(metric.better && numeric && distinct.size > 1 && assets.length > 1);
    const best = canRank ? (metric.better === "higher" ? Math.max(...numeric!) : Math.min(...numeric!)) : null;
    const worst = canRank ? (metric.better === "higher" ? Math.min(...numeric!) : Math.max(...numeric!)) : null;

    const cells = values.map((value, index): ComparisonCell => {
      const differs = index > 0 && JSON.stringify(value) !== JSON.stringify(base);
      const delta =
        index > 0 && typeof value === "number" && typeof base === "number" ? deltaText(metric, value, base) : null;
      return {
        value,
        display: metric.format(value),
        differs,
        rank: canRank ? (value === best ? "best" : value === worst ? "worst" : null) : null,
        delta,
      };
    });
    return { metric, cells, hasDifference: cells.some((c) => c.differs) };
  });
}

// ── Export ──────────────────────────────────────────────────────────────────

/**
 * Quotes a CSV field (RFC 4180) and neutralises spreadsheet formula injection
 * by prefixing cells that start with `=`, `+`, `-`, `@`, tab or CR with `'`.
 */
export function csvField(value: string): string {
  const guarded = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return /[",\r\n]/.test(guarded) ? `"${guarded.replace(/"/g, '""')}"` : guarded;
}

/** CSV of the comparison: one metric per row, one asset per column, plus raw values. */
export function buildComparisonCsv(assets: readonly CompareAsset[], rows: readonly ComparisonRow[] = buildComparisonRows(assets)): string {
  const header = ["Metric", ...assets.map((a) => `${a.name} (${a.symbol})`)];
  const lines = [header, ...rows.map((row) => [row.metric.label, ...row.cells.map((cell) => cell.display)])];
  return lines.map((line) => line.map(csvField).join(",")).join("\r\n") + "\r\n";
}

/** JSON export with machine-readable values (no display formatting). */
export function buildComparisonJson(assets: readonly CompareAsset[]): string {
  return JSON.stringify({ generatedAt: new Date().toISOString(), assets }, null, 2);
}
