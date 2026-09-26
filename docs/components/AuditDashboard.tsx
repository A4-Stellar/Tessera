"use client";

import { ChangeEvent, useCallback, useMemo, useState } from "react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { API_BASE_URL } from "@/lib/api";
import {
  AUDIT_ISSUE_LABELS,
  AuditIssue,
  AuditReport,
  AuditRow,
  buildAuditPdf,
  buildAuditRows,
  calculateAuditSummary,
  createTimestampProof,
  LegalCapTableRecord,
  OnChainHolder,
  parseCapTableText,
} from "@/lib/cap-table-audit";

export interface AuditDashboardProps {
  /** Asset registry id used by GET /v1/assets/:id/holders. */
  assetId: string | number;
  /** Overrides NEXT_PUBLIC_API_BASE_URL for demos/tests. */
  apiBaseUrl?: string;
  /** Optional authorized/expected share count. Defaults to the uploaded legal total. */
  expectedTotalShares?: string | number;
}

interface TimestampProofState {
  generatedAt: string;
  hash: string;
}

const PAGE_SIZE = 100;
const MAX_HOLDERS = 100_000;
const CHART_COLORS = ["#34d399", "#f59e0b", "#ef4444", "#38bdf8", "#a78bfa"];

function apiV1Base(baseUrl: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  return trimmed.endsWith("/v1") ? trimmed : `${trimmed}/v1`;
}

function truncateAddress(address: string): string {
  return address.length > 18 ? `${address.slice(0, 8)}…${address.slice(-6)}` : address;
}

/** Fetch every holder page so an audit never silently compares a partial cap table. */
export async function fetchAllAssetHolders(
  baseUrl: string,
  assetId: string | number,
): Promise<OnChainHolder[]> {
  const holders: OnChainHolder[] = [];
  const root = apiV1Base(baseUrl);
  let offset = 0;

  while (offset < MAX_HOLDERS) {
    const response = await fetch(
      `${root}/assets/${encodeURIComponent(String(assetId))}/holders?offset=${offset}&limit=${PAGE_SIZE}`,
      {
        headers: { Accept: "application/json" },
        cache: "no-store",
      },
    );
    if (!response.ok) {
      throw new Error(`Live holder request failed (${response.status}).`);
    }

    const batch = (await response.json()) as unknown;
    if (!Array.isArray(batch)) {
      throw new Error("Live holder endpoint returned an invalid response.");
    }

    const normalized = batch.map((value, index) => {
      if (typeof value !== "object" || value === null) {
        throw new Error(`Invalid holder at API row ${offset + index + 1}.`);
      }
      const holder = value as Record<string, unknown>;
      if (typeof holder.address !== "string") {
        throw new Error(`Holder at API row ${offset + index + 1} is missing an address.`);
      }
      return {
        address: holder.address,
        balance: String(holder.balance ?? ""),
        share_percent:
          typeof holder.share_percent === "number" ? holder.share_percent : undefined,
      } satisfies OnChainHolder;
    });

    holders.push(...normalized);
    if (normalized.length < PAGE_SIZE) return holders;
    offset += normalized.length;
  }

  throw new Error(`Holder list exceeded the ${MAX_HOLDERS.toLocaleString()} row safety limit.`);
}

function findingClass(issue: AuditIssue): string {
  switch (issue) {
    case "missing_kyc":
      return "border-amber-500/30 bg-amber-500/10 text-amber-200";
    case "unexpected_on_chain":
      return "border-sky-500/30 bg-sky-500/10 text-sky-200";
    default:
      return "border-red-500/30 bg-red-500/10 text-red-200";
  }
}

function rowClass(row: AuditRow): string {
  if (row.issues.length === 0) return "bg-emerald-500/[0.03]";
  if (row.issues.some((issue) => issue !== "missing_kyc")) return "bg-red-500/[0.04]";
  return "bg-amber-500/[0.04]";
}

/**
 * Real-time cap-table audit dashboard.
 *
 * Upload a legal CSV/JSON registry, compare it against live indexed Stellar
 * balances, inspect discrepancies, and export a cryptographically timestamped
 * PDF report without sending the legal file to the server.
 */
export function AuditDashboard({
  assetId,
  apiBaseUrl = API_BASE_URL,
  expectedTotalShares,
}: AuditDashboardProps) {
  const [legalRecords, setLegalRecords] = useState<LegalCapTableRecord[] | null>(null);
  const [holders, setHolders] = useState<OnChainHolder[] | null>(null);
  const [sourceFile, setSourceFile] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [auditedAt, setAuditedAt] = useState<string | null>(null);
  const [lastProof, setLastProof] = useState<TimestampProofState | null>(null);

  const rows = useMemo(
    () => (legalRecords && holders ? buildAuditRows(legalRecords, holders) : []),
    [legalRecords, holders],
  );

  const summary = useMemo(
    () => (legalRecords && holders ? calculateAuditSummary(rows, expectedTotalShares) : null),
    [expectedTotalShares, holders, legalRecords, rows],
  );

  const chartData = useMemo(() => {
    if (!summary) return [];
    return [
      { name: "Matched", value: summary.matchedRows },
      { name: "Balance mismatch", value: summary.balanceMismatchCount },
      { name: "Missing on-chain", value: summary.missingOnChainCount },
      { name: "Unexpected holder", value: summary.unexpectedOnChainCount },
      { name: "Missing KYC", value: summary.missingKycCount },
    ];
  }, [summary]);

  const runAudit = useCallback(
    async (records: LegalCapTableRecord[]) => {
      setLoading(true);
      setError(null);
      setLastProof(null);
      try {
        const liveHolders = await fetchAllAssetHolders(apiBaseUrl, assetId);
        setLegalRecords(records);
        setHolders(liveHolders);
        setAuditedAt(new Date().toISOString());
      } catch (caught) {
        setHolders(null);
        setError(caught instanceof Error ? caught.message : "Audit failed.");
      } finally {
        setLoading(false);
      }
    },
    [apiBaseUrl, assetId],
  );

  const handleUpload = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;

    setError(null);
    setLastProof(null);
    try {
      const text = await file.text();
      const records = parseCapTableText(text, file.name);
      setSourceFile(file.name);
      await runAudit(records);
    } catch (caught) {
      setLegalRecords(null);
      setHolders(null);
      setAuditedAt(null);
      setError(caught instanceof Error ? caught.message : "Could not read the cap table.");
    }
  };

  const refreshAudit = async () => {
    if (legalRecords) await runAudit(legalRecords);
  };

  const exportPdf = async () => {
    if (!summary || rows.length === 0) return;
    setExporting(true);
    setError(null);

    try {
      const generatedAt = new Date().toISOString();
      const proof = await createTimestampProof(String(assetId), generatedAt, summary, rows);
      const report: AuditReport = {
        assetId: String(assetId),
        sourceFile: sourceFile || "uploaded-cap-table",
        generatedAt,
        timestampProof: proof,
        summary,
        rows,
      };
      const bytes = buildAuditPdf(report);
      const blob = new Blob([new Uint8Array(bytes).buffer as ArrayBuffer], { type: "application/pdf" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `tessera-cap-table-audit-${assetId}-${generatedAt.replace(/[:.]/g, "-")}.pdf`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      URL.revokeObjectURL(url);
      setLastProof({ generatedAt, hash: proof });
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : "Could not export the audit PDF.");
    } finally {
      setExporting(false);
    }
  };

  return (
    <section className="my-8 overflow-hidden rounded-2xl border border-white/10 bg-base-950/70 shadow-2xl shadow-black/20">
      <div className="border-b border-white/10 bg-gradient-to-r from-brand-500/10 via-transparent to-gold-500/10 p-5 sm:p-6">
        <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
          <div>
            <p className="mb-1 text-xs font-semibold uppercase tracking-[0.2em] text-brand-300">
              Real-time reconciliation
            </p>
            <h2 className="text-2xl font-bold text-base-50">Cap-Table Audit Dashboard</h2>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-base-300">
              Compare a legal shareholder registry with live Stellar holder balances for asset {String(assetId)}.
              The uploaded file stays in your browser.
            </p>
          </div>
          <div className="rounded-lg border border-white/10 bg-black/20 px-3 py-2 text-xs text-base-300">
            <span className="block font-semibold text-base-100">Live source</span>
            GET /v1/assets/{String(assetId)}/holders
          </div>
        </div>
      </div>

      <div className="p-5 sm:p-6">
        <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-end">
          <div>
            <label htmlFor="audit-cap-table-upload" className="mb-2 block text-sm font-semibold text-base-100">
              Legal cap table
            </label>
            <input
              id="audit-cap-table-upload"
              type="file"
              accept=".csv,.json,text/csv,application/json"
              onChange={handleUpload}
              disabled={loading}
              className="block w-full rounded-lg border border-white/10 bg-white/[0.04] px-3 py-2 text-sm text-base-200 file:mr-4 file:rounded-md file:border-0 file:bg-brand-500/15 file:px-3 file:py-1.5 file:font-semibold file:text-brand-200 hover:file:bg-brand-500/25 disabled:opacity-60"
            />
            <p className="mt-2 text-xs leading-5 text-base-400">
              CSV/JSON requires an address or wallet column and a balance or shares column. KYC aliases include
              {" "}<code>kyc_verified</code>, <code>kyc_status</code>, and <code>verified</code>.
            </p>
          </div>

          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              onClick={refreshAudit}
              disabled={!legalRecords || loading}
              className="rounded-lg border border-white/10 bg-white/[0.04] px-4 py-2 text-sm font-semibold text-base-100 transition hover:bg-white/[0.08] disabled:cursor-not-allowed disabled:opacity-40"
            >
              {loading ? "Auditing…" : "Refresh live data"}
            </button>
            <button
              type="button"
              onClick={exportPdf}
              disabled={!summary || rows.length === 0 || loading || exporting}
              className="rounded-lg bg-brand-500 px-4 py-2 text-sm font-semibold text-white transition hover:bg-brand-400 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {exporting ? "Signing report…" : "Export signed PDF"}
            </button>
          </div>
        </div>

        <div className="mt-4 min-h-6" aria-live="polite">
          {loading && <p className="text-sm text-brand-200">Fetching all live holder pages and reconciling balances…</p>}
          {error && (
            <p role="alert" className="rounded-lg border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-200">
              {error}
            </p>
          )}
          {!loading && !error && auditedAt && (
            <p className="text-xs text-base-400">
              Last compared {new Date(auditedAt).toLocaleString()} · {legalRecords?.length ?? 0} legal records · {holders?.length ?? 0} live holders
            </p>
          )}
        </div>

        {summary && (
          <>
            <div className="mt-5 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
              <div className="rounded-xl border border-white/10 bg-white/[0.03] p-4">
                <p className="text-xs font-semibold uppercase tracking-wide text-base-400">Legal shares</p>
                <p className="mt-1 break-all text-xl font-bold text-base-50">{summary.legalTotalShares}</p>
              </div>
              <div className="rounded-xl border border-white/10 bg-white/[0.03] p-4">
                <p className="text-xs font-semibold uppercase tracking-wide text-base-400">On-chain shares</p>
                <p className="mt-1 break-all text-xl font-bold text-base-50">{summary.onChainTotalShares}</p>
              </div>
              <div className={`rounded-xl border p-4 ${summary.discrepancyRows === 0 ? "border-emerald-500/20 bg-emerald-500/[0.06]" : "border-red-500/20 bg-red-500/[0.06]"}`}>
                <p className="text-xs font-semibold uppercase tracking-wide text-base-400">Discrepancy rows</p>
                <p className="mt-1 text-xl font-bold text-base-50">{summary.discrepancyRows}</p>
              </div>
              <div className={`rounded-xl border p-4 ${summary.unallocatedShares === "0" ? "border-white/10 bg-white/[0.03]" : "border-amber-500/20 bg-amber-500/[0.06]"}`}>
                <p className="text-xs font-semibold uppercase tracking-wide text-base-400">Unallocated shares</p>
                <p className="mt-1 break-all text-xl font-bold text-base-50">{summary.unallocatedShares}</p>
                {summary.excessOnChainShares !== "0" && (
                  <p className="mt-1 text-xs text-red-300">Excess on-chain: {summary.excessOnChainShares}</p>
                )}
              </div>
            </div>

            <div className="mt-5 grid gap-5 xl:grid-cols-[360px_minmax(0,1fr)]">
              <div className="rounded-xl border border-white/10 bg-white/[0.02] p-4">
                <h3 className="text-sm font-semibold text-base-100">Finding distribution</h3>
                <div
                  className="mt-3 h-56 w-full"
                  role="img"
                  aria-label={`Audit findings: ${summary.matchedRows} matched, ${summary.balanceMismatchCount} balance mismatches, ${summary.missingOnChainCount} missing on-chain, ${summary.unexpectedOnChainCount} unexpected holders, ${summary.missingKycCount} missing KYC`}
                >
                  <ResponsiveContainer width="100%" height="100%">
                    <BarChart data={chartData} layout="vertical" margin={{ left: 10, right: 12 }}>
                      <CartesianGrid strokeDasharray="3 3" stroke="rgba(255,255,255,0.07)" horizontal={false} />
                      <XAxis type="number" allowDecimals={false} stroke="#8b93a7" fontSize={11} />
                      <YAxis type="category" dataKey="name" width={110} stroke="#8b93a7" fontSize={10} />
                      <Tooltip
                        cursor={{ fill: "rgba(255,255,255,0.03)" }}
                        contentStyle={{ background: "#171b24", border: "1px solid rgba(255,255,255,0.1)" }}
                      />
                      <Bar dataKey="value" radius={[0, 4, 4, 0]}>
                        {chartData.map((entry, index) => (
                          <Cell key={entry.name} fill={CHART_COLORS[index % CHART_COLORS.length]} />
                        ))}
                      </Bar>
                    </BarChart>
                  </ResponsiveContainer>
                </div>
              </div>

              <div className="rounded-xl border border-white/10 bg-white/[0.02] p-4">
                <h3 className="text-sm font-semibold text-base-100">Audit controls</h3>
                <dl className="mt-3 grid gap-3 text-sm sm:grid-cols-2">
                  <div>
                    <dt className="text-xs text-base-400">Expected share total</dt>
                    <dd className="mt-0.5 break-all font-mono text-base-100">{summary.expectedTotalShares}</dd>
                  </div>
                  <div>
                    <dt className="text-xs text-base-400">Missing KYC verification</dt>
                    <dd className="mt-0.5 font-mono text-base-100">{summary.missingKycCount}</dd>
                  </div>
                  <div>
                    <dt className="text-xs text-base-400">Missing on-chain</dt>
                    <dd className="mt-0.5 font-mono text-base-100">{summary.missingOnChainCount}</dd>
                  </div>
                  <div>
                    <dt className="text-xs text-base-400">Unexpected on-chain</dt>
                    <dd className="mt-0.5 font-mono text-base-100">{summary.unexpectedOnChainCount}</dd>
                  </div>
                </dl>
                <p className="mt-4 text-xs leading-5 text-base-400">
                  KYC status is taken from the uploaded legal registry. A live holder with no legal record is also flagged as lacking a registry-backed KYC verification.
                </p>
              </div>
            </div>

            <div className="mt-5 overflow-hidden rounded-xl border border-white/10">
              <div className="flex flex-col gap-1 border-b border-white/10 bg-white/[0.03] px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
                <h3 className="text-sm font-semibold text-base-100">Visual diff</h3>
                <span className="text-xs text-base-400">Delta = on-chain − legal</span>
              </div>
              <div className="overflow-x-auto">
                <table className="min-w-[980px] w-full text-sm">
                  <caption className="sr-only">
                    Legal cap-table records compared with live on-chain holders for asset {String(assetId)}
                  </caption>
                  <thead className="bg-black/20 text-left text-xs uppercase tracking-wide text-base-400">
                    <tr>
                      <th scope="col" className="px-4 py-3">Holder</th>
                      <th scope="col" className="px-3 py-3">Legal</th>
                      <th scope="col" className="px-3 py-3">On-chain</th>
                      <th scope="col" className="px-3 py-3">Delta</th>
                      <th scope="col" className="px-3 py-3">KYC</th>
                      <th scope="col" className="px-4 py-3">Findings</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((row) => (
                      <tr key={row.address} className={`border-t border-white/5 align-top ${rowClass(row)}`}>
                        <td className="px-4 py-3">
                          {row.holderName && <span className="mb-1 block font-medium text-base-100">{row.holderName}</span>}
                          <span className="font-mono text-xs text-base-300" title={row.address}>{truncateAddress(row.address)}</span>
                        </td>
                        <td className="px-3 py-3 font-mono text-base-200">{row.legalBalance}</td>
                        <td className="px-3 py-3 font-mono text-base-200">{row.onChainBalance}</td>
                        <td className={`px-3 py-3 font-mono ${row.delta === "0" ? "text-base-400" : "font-semibold text-red-200"}`}>
                          {row.delta}
                        </td>
                        <td className="px-3 py-3">
                          <span className={row.kycVerified === true ? "text-emerald-300" : "font-semibold text-amber-200"}>
                            {row.kycVerified === true ? "Verified" : "Missing"}
                          </span>
                        </td>
                        <td className="px-4 py-3">
                          {row.issues.length === 0 ? (
                            <span className="inline-flex rounded-full border border-emerald-500/30 bg-emerald-500/10 px-2 py-1 text-xs font-medium text-emerald-200">
                              Matched
                            </span>
                          ) : (
                            <div className="flex flex-wrap gap-1.5">
                              {row.issues.map((issue) => (
                                <span
                                  key={issue}
                                  className={`inline-flex rounded-full border px-2 py-1 text-xs font-medium ${findingClass(issue)}`}
                                >
                                  {AUDIT_ISSUE_LABELS[issue]}
                                </span>
                              ))}
                            </div>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>

            {lastProof && (
              <div className="mt-5 rounded-xl border border-emerald-500/20 bg-emerald-500/[0.05] p-4">
                <h3 className="text-sm font-semibold text-emerald-200">Last exported timestamp proof</h3>
                <p className="mt-1 text-xs text-base-300">Generated {lastProof.generatedAt}</p>
                <code className="mt-2 block break-all rounded-lg bg-black/20 p-2 text-xs text-emerald-100">
                  sha256:{lastProof.hash}
                </code>
                <p className="mt-2 text-xs leading-5 text-base-400">
                  The hash binds the asset id, UTC generation timestamp, audit summary, and every diff row to the exported PDF.
                </p>
              </div>
            )}
          </>
        )}

        {!summary && !loading && !error && (
          <div className="mt-5 rounded-xl border border-dashed border-white/10 bg-white/[0.02] px-5 py-10 text-center">
            <p className="text-sm font-medium text-base-200">Upload a legal CSV or JSON cap table to start the audit.</p>
            <p className="mt-1 text-xs text-base-400">The dashboard will fetch live holder data automatically.</p>
          </div>
        )}
      </div>
    </section>
  );
}

export default AuditDashboard;
