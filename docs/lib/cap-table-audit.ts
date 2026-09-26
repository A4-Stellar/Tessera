export interface OnChainHolder {
  address: string;
  balance: string;
  share_percent?: number;
}

export interface LegalCapTableRecord {
  address: string;
  balance: string;
  kycVerified: boolean | null;
  holderName?: string;
}

export type AuditIssue =
  | "balance_mismatch"
  | "missing_on_chain"
  | "unexpected_on_chain"
  | "missing_kyc";

export interface AuditRow {
  address: string;
  holderName?: string;
  legalBalance: string;
  onChainBalance: string;
  delta: string;
  kycVerified: boolean | null;
  issues: AuditIssue[];
}

export interface AuditSummary {
  legalTotalShares: string;
  onChainTotalShares: string;
  expectedTotalShares: string;
  unallocatedShares: string;
  excessOnChainShares: string;
  matchedRows: number;
  discrepancyRows: number;
  balanceMismatchCount: number;
  missingOnChainCount: number;
  unexpectedOnChainCount: number;
  missingKycCount: number;
}

export interface AuditReport {
  assetId: string;
  sourceFile: string;
  generatedAt: string;
  timestampProof: string;
  summary: AuditSummary;
  rows: AuditRow[];
}

const ADDRESS_KEYS = [
  "address",
  "wallet",
  "wallet_address",
  "stellar_address",
  "account",
  "account_id",
];
const BALANCE_KEYS = [
  "balance",
  "shares",
  "share_balance",
  "token_balance",
  "quantity",
  "units",
];
const KYC_KEYS = [
  "kyc_verified",
  "kyc",
  "kyc_status",
  "verification_status",
  "verified",
];
const NAME_KEYS = ["holder_name", "shareholder", "shareholder_name", "name", "legal_name"];

const KYC_TRUE = new Set(["true", "yes", "1", "verified", "approved", "passed", "complete", "completed"]);
const KYC_FALSE = new Set(["false", "no", "0", "unverified", "rejected", "pending", "missing", "expired", "suspended"]);

const POW10 = (power: number) => 10n ** BigInt(power);

interface DecimalAmount {
  units: bigint;
  scale: number;
}

function normalizeHeader(value: string): string {
  return value.trim().toLowerCase().replace(/[\s-]+/g, "_");
}

function readAlias(record: Record<string, unknown>, aliases: string[]): unknown {
  const normalized = new Map<string, unknown>();
  for (const [key, value] of Object.entries(record)) {
    normalized.set(normalizeHeader(key), value);
  }
  for (const alias of aliases) {
    if (normalized.has(alias)) return normalized.get(alias);
  }
  return undefined;
}

/** Normalize a non-negative decimal balance without losing precision. */
export function normalizeBalance(value: unknown): string {
  const raw = String(value ?? "")
    .trim()
    .replace(/[,_\s]/g, "");

  if (!/^\+?\d+(?:\.\d+)?$/.test(raw)) {
    throw new Error(`Invalid non-negative balance: ${String(value ?? "")}`);
  }

  const unsigned = raw.startsWith("+") ? raw.slice(1) : raw;
  const [integerPart, fractionalPart = ""] = unsigned.split(".");
  const integer = integerPart.replace(/^0+(?=\d)/, "") || "0";
  const fraction = fractionalPart.replace(/0+$/, "");
  return fraction ? `${integer}.${fraction}` : integer;
}

function parseDecimal(value: string): DecimalAmount {
  const normalized = normalizeBalance(value);
  const [integer, fraction = ""] = normalized.split(".");
  return {
    units: BigInt(`${integer}${fraction}`),
    scale: fraction.length,
  };
}

function alignDecimal(value: DecimalAmount, scale: number): bigint {
  return value.units * POW10(scale - value.scale);
}

function formatSignedUnits(units: bigint, scale: number): string {
  const negative = units < 0n;
  const absolute = negative ? -units : units;
  const digits = absolute.toString().padStart(scale + 1, "0");
  const integer = scale === 0 ? digits : digits.slice(0, -scale) || "0";
  const fraction = scale === 0 ? "" : digits.slice(-scale).replace(/0+$/, "");
  const value = fraction ? `${integer}.${fraction}` : integer;
  if (value === "0") return "0";
  return negative ? `-${value}` : value;
}

export function sumBalances(values: string[]): string {
  if (values.length === 0) return "0";
  const parsed = values.map(parseDecimal);
  const scale = Math.max(...parsed.map((value) => value.scale));
  const total = parsed.reduce((sum, value) => sum + alignDecimal(value, scale), 0n);
  return formatSignedUnits(total, scale);
}

/** Returns a - b with exact decimal arithmetic. */
export function subtractBalances(a: string, b: string): string {
  const left = parseDecimal(a);
  const right = parseDecimal(b);
  const scale = Math.max(left.scale, right.scale);
  return formatSignedUnits(alignDecimal(left, scale) - alignDecimal(right, scale), scale);
}

function positiveDifference(a: string, b: string): string {
  const difference = subtractBalances(a, b);
  return difference.startsWith("-") ? "0" : difference;
}

function parseKyc(value: unknown): boolean | null {
  if (typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (value === 1) return true;
    if (value === 0) return false;
    return null;
  }

  const normalized = String(value ?? "").trim().toLowerCase();
  if (!normalized) return null;
  if (KYC_TRUE.has(normalized)) return true;
  if (KYC_FALSE.has(normalized)) return false;
  return null;
}

function normalizeLegalRecord(record: Record<string, unknown>, rowLabel: string): LegalCapTableRecord {
  const addressValue = readAlias(record, ADDRESS_KEYS);
  const balanceValue = readAlias(record, BALANCE_KEYS);
  const kycValue = readAlias(record, KYC_KEYS);
  const nameValue = readAlias(record, NAME_KEYS);

  const address = String(addressValue ?? "").trim();
  if (!address) throw new Error(`${rowLabel}: missing holder address.`);
  if (balanceValue === undefined || balanceValue === null || String(balanceValue).trim() === "") {
    throw new Error(`${rowLabel}: missing balance for ${address}.`);
  }

  return {
    address,
    balance: normalizeBalance(balanceValue),
    kycVerified: parseKyc(kycValue),
    holderName: nameValue == null || String(nameValue).trim() === "" ? undefined : String(nameValue).trim(),
  };
}

function parseCsvMatrix(input: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;

  for (let index = 0; index < input.length; index += 1) {
    const character = input[index];

    if (quoted) {
      if (character === '"') {
        if (input[index + 1] === '"') {
          cell += '"';
          index += 1;
        } else {
          quoted = false;
        }
      } else {
        cell += character;
      }
      continue;
    }

    if (character === '"') {
      quoted = true;
    } else if (character === ",") {
      row.push(cell);
      cell = "";
    } else if (character === "\n" || character === "\r") {
      if (character === "\r" && input[index + 1] === "\n") index += 1;
      row.push(cell);
      if (row.some((value) => value.trim() !== "")) rows.push(row);
      row = [];
      cell = "";
    } else {
      cell += character;
    }
  }

  if (quoted) throw new Error("CSV contains an unterminated quoted field.");
  row.push(cell);
  if (row.some((value) => value.trim() !== "")) rows.push(row);
  return rows;
}

function ensureUniqueAddresses(records: LegalCapTableRecord[]): LegalCapTableRecord[] {
  const seen = new Set<string>();
  for (const record of records) {
    const key = record.address.toUpperCase();
    if (seen.has(key)) {
      throw new Error(`Duplicate holder address in legal cap table: ${record.address}`);
    }
    seen.add(key);
  }
  return records;
}

export function parseCsvCapTable(text: string): LegalCapTableRecord[] {
  const rows = parseCsvMatrix(text);
  if (rows.length < 2) throw new Error("CSV must contain a header row and at least one holder record.");

  const headers = rows[0].map(normalizeHeader);
  const records = rows.slice(1).map((values, index) => {
    const raw: Record<string, unknown> = {};
    headers.forEach((header, column) => {
      raw[header] = values[column] ?? "";
    });
    return normalizeLegalRecord(raw, `CSV row ${index + 2}`);
  });

  return ensureUniqueAddresses(records);
}

function extractJsonRows(value: unknown): unknown[] {
  if (Array.isArray(value)) return value;
  if (typeof value !== "object" || value === null) {
    throw new Error("JSON cap table must be an array or an object containing records.");
  }

  const object = value as Record<string, unknown>;
  for (const key of ["records", "cap_table", "capTable", "holders", "shareholders"]) {
    if (Array.isArray(object[key])) return object[key] as unknown[];
  }
  throw new Error("JSON must contain an array under records, cap_table, capTable, holders, or shareholders.");
}

export function parseJsonCapTable(text: string): LegalCapTableRecord[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("The uploaded JSON file is not valid JSON.");
  }

  const rows = extractJsonRows(parsed);
  if (rows.length === 0) throw new Error("JSON cap table contains no holder records.");
  const records = rows.map((value, index) => {
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      throw new Error(`JSON record ${index + 1} must be an object.`);
    }
    return normalizeLegalRecord(value as Record<string, unknown>, `JSON record ${index + 1}`);
  });
  return ensureUniqueAddresses(records);
}

export function parseCapTableText(text: string, fileName = "cap-table.csv"): LegalCapTableRecord[] {
  const trimmed = text.trim();
  if (!trimmed) throw new Error("The uploaded cap table is empty.");

  const lowerName = fileName.toLowerCase();
  if (lowerName.endsWith(".json") || trimmed.startsWith("[") || trimmed.startsWith("{")) {
    return parseJsonCapTable(trimmed);
  }
  return parseCsvCapTable(trimmed);
}

function mergeOnChainHolders(holders: OnChainHolder[]): Map<string, OnChainHolder> {
  const merged = new Map<string, OnChainHolder>();
  for (const holder of holders) {
    const address = holder.address.trim();
    if (!address) continue;
    const balance = normalizeBalance(holder.balance);
    const key = address.toUpperCase();
    const existing = merged.get(key);
    merged.set(key, {
      address: existing?.address ?? address,
      balance: existing ? sumBalances([existing.balance, balance]) : balance,
      share_percent: holder.share_percent,
    });
  }
  return merged;
}

export function buildAuditRows(
  legalRecords: LegalCapTableRecord[],
  onChainHolders: OnChainHolder[],
): AuditRow[] {
  const legalMap = new Map(legalRecords.map((record) => [record.address.toUpperCase(), record]));
  const chainMap = mergeOnChainHolders(onChainHolders);
  const orderedKeys = [
    ...legalRecords.map((record) => record.address.toUpperCase()),
    ...[...chainMap.keys()].filter((key) => !legalMap.has(key)).sort(),
  ];

  return orderedKeys.map((key) => {
    const legal = legalMap.get(key);
    const chain = chainMap.get(key);
    const legalBalance = legal?.balance ?? "0";
    const onChainBalance = chain?.balance ?? "0";
    const issues: AuditIssue[] = [];

    if (legal && !chain) issues.push("missing_on_chain");
    if (!legal && chain) issues.push("unexpected_on_chain");
    if (legal && chain && normalizeBalance(legalBalance) !== normalizeBalance(onChainBalance)) {
      issues.push("balance_mismatch");
    }
    if (!legal || legal.kycVerified !== true) issues.push("missing_kyc");

    return {
      address: legal?.address ?? chain?.address ?? key,
      holderName: legal?.holderName,
      legalBalance,
      onChainBalance,
      delta: subtractBalances(onChainBalance, legalBalance),
      kycVerified: legal?.kycVerified ?? null,
      issues,
    };
  });
}

export function calculateAuditSummary(
  rows: AuditRow[],
  expectedTotalShares?: string | number,
): AuditSummary {
  const legalTotalShares = sumBalances(rows.map((row) => row.legalBalance));
  const onChainTotalShares = sumBalances(rows.map((row) => row.onChainBalance));
  const expected = expectedTotalShares == null
    ? legalTotalShares
    : normalizeBalance(expectedTotalShares);

  return {
    legalTotalShares,
    onChainTotalShares,
    expectedTotalShares: expected,
    unallocatedShares: positiveDifference(expected, onChainTotalShares),
    excessOnChainShares: positiveDifference(onChainTotalShares, expected),
    matchedRows: rows.filter((row) => row.issues.length === 0).length,
    discrepancyRows: rows.filter((row) => row.issues.length > 0).length,
    balanceMismatchCount: rows.filter((row) => row.issues.includes("balance_mismatch")).length,
    missingOnChainCount: rows.filter((row) => row.issues.includes("missing_on_chain")).length,
    unexpectedOnChainCount: rows.filter((row) => row.issues.includes("unexpected_on_chain")).length,
    missingKycCount: rows.filter((row) => row.issues.includes("missing_kyc")).length,
  };
}

export const AUDIT_ISSUE_LABELS: Record<AuditIssue, string> = {
  balance_mismatch: "Balance mismatch",
  missing_on_chain: "Missing on-chain",
  unexpected_on_chain: "Unexpected on-chain holder",
  missing_kyc: "Missing KYC verification",
};

export function serializeAuditProofPayload(
  assetId: string,
  generatedAt: string,
  summary: AuditSummary,
  rows: AuditRow[],
): string {
  return JSON.stringify({
    version: "tessera-cap-table-audit-v1",
    assetId,
    generatedAt,
    summary,
    rows: rows.map((row) => ({
      address: row.address,
      legalBalance: row.legalBalance,
      onChainBalance: row.onChainBalance,
      delta: row.delta,
      kycVerified: row.kycVerified,
      issues: row.issues,
    })),
  });
}

export async function createTimestampProof(
  assetId: string,
  generatedAt: string,
  summary: AuditSummary,
  rows: AuditRow[],
): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error("Web Crypto is unavailable; cannot generate the audit timestamp proof.");
  }
  const payload = serializeAuditProofPayload(assetId, generatedAt, summary, rows);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", new TextEncoder().encode(payload));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function ascii(value: string): string {
  return value.replace(/[^\x20-\x7E]/g, "?");
}

function escapePdfText(value: string): string {
  return ascii(value).replace(/\\/g, "\\\\").replace(/\(/g, "\\(").replace(/\)/g, "\\)");
}

function wrapLine(value: string, maxLength = 92): string[] {
  const clean = ascii(value);
  if (clean.length <= maxLength) return [clean];
  const words = clean.split(/\s+/);
  const lines: string[] = [];
  let current = "";
  for (const word of words) {
    if (!current) {
      current = word;
    } else if (`${current} ${word}`.length <= maxLength) {
      current += ` ${word}`;
    } else {
      lines.push(current);
      current = word;
    }
  }
  if (current) lines.push(current);
  return lines;
}

function reportLines(report: AuditReport): string[] {
  const { summary } = report;
  const lines = [
    "TESSERA CAP-TABLE AUDIT REPORT",
    `Asset ID: ${report.assetId}`,
    `Source file: ${report.sourceFile}`,
    `Generated at (UTC): ${report.generatedAt}`,
    `Timestamp proof (SHA-256): ${report.timestampProof}`,
    "Proof scheme: SHA-256 over tessera-cap-table-audit-v1 JSON payload including generatedAt.",
    "",
    "SUMMARY",
    `Legal registry total: ${summary.legalTotalShares}`,
    `On-chain holder total: ${summary.onChainTotalShares}`,
    `Expected total shares: ${summary.expectedTotalShares}`,
    `Unallocated shares: ${summary.unallocatedShares}`,
    `Excess on-chain shares: ${summary.excessOnChainShares}`,
    `Matched rows: ${summary.matchedRows}`,
    `Rows with discrepancies: ${summary.discrepancyRows}`,
    `Balance mismatches: ${summary.balanceMismatchCount}`,
    `Missing on-chain: ${summary.missingOnChainCount}`,
    `Unexpected on-chain holders: ${summary.unexpectedOnChainCount}`,
    `Missing KYC verification: ${summary.missingKycCount}`,
    "",
    "HOLDER DIFF (on-chain delta = on-chain - legal)",
  ];

  report.rows.forEach((row, index) => {
    const findings = row.issues.length
      ? row.issues.map((issue) => AUDIT_ISSUE_LABELS[issue]).join("; ")
      : "Matched";
    lines.push(
      `${index + 1}. ${row.address} | legal=${row.legalBalance} | on-chain=${row.onChainBalance} | delta=${row.delta} | KYC=${row.kycVerified === true ? "verified" : "missing"}`,
      `   Findings: ${findings}`,
    );
  });

  return lines.flatMap((line) => wrapLine(line));
}

/**
 * Minimal dependency-free PDF generator for exportable audit reports. The
 * resulting PDF embeds the timestamp proof and every diff row as selectable
 * text, so reports remain portable without adding a client PDF dependency.
 */
export function buildAuditPdf(report: AuditReport): Uint8Array {
  const lines = reportLines(report);
  const linesPerPage = 46;
  const pages: string[][] = [];
  for (let index = 0; index < lines.length; index += linesPerPage) {
    pages.push(lines.slice(index, index + linesPerPage));
  }
  if (pages.length === 0) pages.push(["TESSERA CAP-TABLE AUDIT REPORT"]);

  const catalogId = 1;
  const pagesId = 2;
  const fontId = 3 + pages.length * 2;
  const objects: string[] = [];
  objects[catalogId] = `<< /Type /Catalog /Pages ${pagesId} 0 R >>`;

  const pageIds = pages.map((_, index) => 3 + index * 2);
  objects[pagesId] = `<< /Type /Pages /Kids [${pageIds.map((id) => `${id} 0 R`).join(" ")}] /Count ${pages.length} >>`;

  pages.forEach((pageLines, index) => {
    const pageId = pageIds[index];
    const contentId = pageId + 1;
    const textCommands = pageLines
      .map((line, lineIndex) => `${lineIndex === 0 ? "" : "T* "}(${escapePdfText(line)}) Tj`)
      .join("\n");
    const stream = `BT\n/F1 9 Tf\n48 770 Td\n14 TL\n${textCommands}\nET`;

    objects[pageId] = `<< /Type /Page /Parent ${pagesId} 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 ${fontId} 0 R >> >> /Contents ${contentId} 0 R >>`;
    objects[contentId] = `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`;
  });
  objects[fontId] = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

  let pdf = "%PDF-1.4\n%Tessera\n";
  const offsets: number[] = [0];
  for (let id = 1; id < objects.length; id += 1) {
    offsets[id] = pdf.length;
    pdf += `${id} 0 obj\n${objects[id]}\nendobj\n`;
  }

  const xrefOffset = pdf.length;
  pdf += `xref\n0 ${objects.length}\n`;
  pdf += "0000000000 65535 f \n";
  for (let id = 1; id < objects.length; id += 1) {
    pdf += `${offsets[id].toString().padStart(10, "0")} 00000 n \n`;
  }
  pdf += `trailer\n<< /Size ${objects.length} /Root ${catalogId} 0 R >>\nstartxref\n${xrefOffset}\n%%EOF`;

  return new TextEncoder().encode(pdf);
}
