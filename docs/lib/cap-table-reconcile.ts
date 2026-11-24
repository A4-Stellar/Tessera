/**
 * On-chain cap table vs SEC EDGAR Form 4 holdings (issue #476).
 *
 * Pure helpers behind `CapTableCompare mode="edgar"`: CSV parsing, the diff,
 * threshold highlighting and the report payload that the API signs. The rules
 * match the API's `services/edgar.rs`, so a browser report and an
 * `import-edgar` report classify the same holders the same way.
 */

/** One row of the uploaded CSV, built from EDGAR Form 4 filings. */
export interface FilingHolder {
  /** Stellar address the issuer maps the reporting owner to. */
  address: string;
  ownerCik: string | null;
  ownerName: string | null;
  /** Shares owned following the owner's latest reported transaction. */
  shares: number;
}

/** A holder from `GET /v1/assets/:id/holders`, balance in base units. */
export interface OnChainHolder {
  address: string;
  balance: string;
}

export type DiffKind = "new" | "departed" | "changed" | "unchanged";
export type Highlight = "none" | "yellow" | "red";

export interface ReconciliationRow {
  address: string;
  ownerCik: string | null;
  ownerName: string | null;
  /** Whole tokens (decimals applied). */
  onChain: number;
  /** Filing shares converted to tokens. */
  filing: number;
  kind: DiffKind;
  absoluteChange: number;
  /** Relative to the filing. ±100 for new and departed holders. */
  percentChange: number;
  highlight: Highlight;
  /** |absoluteChange| as a share of total supply, 0–100. */
  supplyPercent: number;
  /** Above 1 % of total supply: reported to the compliance webhook. */
  complianceAlert: boolean;
}

export interface ReconciliationReport {
  assetId: string;
  source: "csv" | "edgar";
  totalSupply: number;
  tokensPerShare: number;
  generatedAt: string;
  rows: ReconciliationRow[];
  counts: Record<DiffKind, number>;
  complianceAlerts: number;
}

export const YELLOW_PERCENT = 5;
export const RED_PERCENT = 10;
export const COMPLIANCE_SUPPLY_PERCENT = 1;

/** Yellow above 5 % change, red above 10 %. */
export function highlightFor(percentChange: number): Highlight {
  const p = Math.abs(percentChange);
  if (p > RED_PERCENT) return "red";
  if (p > YELLOW_PERCENT) return "yellow";
  return "none";
}

/** RFC 4180 CSV: quoted fields, doubled quotes, CRLF or LF. */
export function parseCsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') {
        quoted = false;
      } else {
        field += c;
      }
    } else if (c === '"') {
      quoted = true;
    } else if (c === ",") {
      row.push(field);
      field = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else {
      field += c;
    }
  }
  if (field !== "" || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows.filter((r) => r.some((cell) => cell.trim() !== ""));
}

const ADDRESS_COLUMNS = ["address", "stellar_address", "holder"];
const SHARES_COLUMNS = ["shares", "shares_owned_following_transaction", "sharesownedfollowingtransaction"];

export class CsvFormatError extends Error {}

/**
 * Reads the filing CSV. Needs an `address` column and a `shares` (or
 * `shares_owned_following_transaction`) column; `owner_cik` and
 * `owner_name` are optional. Numbers may use thousands separators. Several
 * rows for one address are summed.
 */
export function parseFilingCsv(text: string): FilingHolder[] {
  const [header, ...body] = parseCsv(text.replace(/^﻿/, ""));
  if (!header) throw new CsvFormatError("The CSV is empty.");
  const cols = header.map((h) => h.trim().toLowerCase().replace(/[\s-]+/g, "_"));
  const find = (names: string[]) => cols.findIndex((c) => names.includes(c));
  const addr = find(ADDRESS_COLUMNS);
  const shares = find(SHARES_COLUMNS);
  if (addr < 0 || shares < 0) {
    throw new CsvFormatError("The CSV needs an `address` column and a `shares` column.");
  }
  const cik = find(["owner_cik", "rptownercik", "cik"]);
  const name = find(["owner_name", "rptownername", "name"]);

  const byAddress = new Map<string, FilingHolder>();
  body.forEach((row, i) => {
    const address = (row[addr] ?? "").trim();
    const raw = (row[shares] ?? "").replace(/[,\s]/g, "");
    const value = Number(raw);
    if (!address) throw new CsvFormatError(`Row ${i + 2} has no address.`);
    if (raw === "" || !Number.isFinite(value) || value < 0) {
      throw new CsvFormatError(`Row ${i + 2} has an invalid share count.`);
    }
    const existing = byAddress.get(address);
    if (existing) {
      existing.shares += value;
    } else {
      byAddress.set(address, {
        address,
        ownerCik: cik >= 0 ? (row[cik] ?? "").trim() || null : null,
        ownerName: name >= 0 ? (row[name] ?? "").trim() || null : null,
        shares: value,
      });
    }
  });
  return [...byAddress.values()];
}

/** Base units → whole tokens. */
export function toTokens(balance: string, decimals: number): number {
  const n = Number(balance);
  return Number.isFinite(n) ? n / 10 ** decimals : 0;
}

export interface ReconcileOptions {
  assetId: string;
  decimals: number;
  /** Total supply in base units. */
  totalSupply: string;
  tokensPerShare?: number;
  source?: "csv" | "edgar";
  now?: Date;
}

const KIND_ORDER: Record<DiffKind, number> = { new: 0, departed: 1, changed: 2, unchanged: 3 };

export function reconcile(
  onChain: readonly OnChainHolder[],
  filing: readonly FilingHolder[],
  options: ReconcileOptions
): ReconciliationReport {
  const tokensPerShare = options.tokensPerShare ?? 1;
  const totalSupply = toTokens(options.totalSupply, options.decimals);
  const chain = new Map<string, number>();
  for (const h of onChain) {
    const t = toTokens(h.balance, options.decimals);
    if (t > 0) chain.set(h.address, (chain.get(h.address) ?? 0) + t);
  }
  const filed = new Map(filing.map((f) => [f.address, f]));
  const addresses = new Set([...chain.keys(), ...filed.keys()]);

  const rows: ReconciliationRow[] = [...addresses].map((address) => {
    const f = filed.get(address);
    const onChainTokens = chain.get(address) ?? 0;
    const filingTokens = (f?.shares ?? 0) * tokensPerShare;
    let kind: DiffKind;
    if (onChainTokens > 0 && filingTokens <= 0) kind = "new";
    else if (onChainTokens <= 0 && filingTokens > 0) kind = "departed";
    else if (Math.abs(onChainTokens - filingTokens) < 1e-9) kind = "unchanged";
    else kind = "changed";
    const absoluteChange = onChainTokens - filingTokens;
    const percentChange =
      kind === "new" ? 100 : kind === "departed" ? -100 : filingTokens > 0 ? (absoluteChange / filingTokens) * 100 : 0;
    const supplyPercent = totalSupply > 0 ? (Math.abs(absoluteChange) / totalSupply) * 100 : 0;
    return {
      address,
      ownerCik: f?.ownerCik ?? null,
      ownerName: f?.ownerName ?? null,
      onChain: onChainTokens,
      filing: filingTokens,
      kind,
      absoluteChange,
      percentChange,
      highlight: highlightFor(percentChange),
      supplyPercent,
      complianceAlert: supplyPercent > COMPLIANCE_SUPPLY_PERCENT,
    };
  });

  rows.sort(
    (a, b) =>
      KIND_ORDER[a.kind] - KIND_ORDER[b.kind] ||
      Math.abs(b.absoluteChange) - Math.abs(a.absoluteChange) ||
      a.address.localeCompare(b.address)
  );
  const counts: Record<DiffKind, number> = { new: 0, departed: 0, changed: 0, unchanged: 0 };
  rows.forEach((r) => counts[r.kind]++);

  return {
    assetId: options.assetId,
    source: options.source ?? "csv",
    totalSupply,
    tokensPerShare,
    generatedAt: (options.now ?? new Date()).toISOString(),
    rows,
    counts,
    complianceAlerts: rows.filter((r) => r.complianceAlert).length,
  };
}

/** Signature returned by `POST /v1/admin/cap-table/reports/sign`. */
export interface ReportSignature {
  algorithm: string;
  digest: string;
  signature: string;
  public_key: string;
  signed_at: string;
}
