"use client";

import { useEffect, useId, useMemo, useState } from "react";
import { API_BASE_URL } from "@/lib/api";
import {
  CsvFormatError,
  parseFilingCsv,
  reconcile,
  type FilingHolder,
  type OnChainHolder,
  type ReconciliationReport,
  type ReconciliationRow,
  type ReportSignature,
} from "@/lib/cap-table-reconcile";
import { buildSignedPdf, canonicalJson } from "@/lib/signed-pdf";

type FetchLike = (input: string, init?: RequestInit) => Promise<Pick<Response, "ok" | "status" | "json">>;

export interface CapTableEdgarCompareProps {
  /** Asset id in the Tessera API. */
  assetId: string | number;
  apiBaseUrl?: string;
  /** Pre-loaded on-chain data (skips the API fetch, e.g. in docs examples). */
  holders?: OnChainHolder[];
  /** Total supply in base units, with `holders`. */
  totalSupply?: string;
  /** Token decimals, with `holders`. Defaults to 0. */
  decimals?: number;
  /** Pre-loaded filing holders instead of an upload. */
  filing?: FilingHolder[];
  /** `fetch` replacement, for tests. */
  fetchImpl?: FetchLike;
}

const HIGHLIGHT_ROW: Record<ReconciliationRow["highlight"], string> = {
  none: "",
  yellow: "bg-amber-500/10",
  red: "bg-red-500/15",
};

const HIGHLIGHT_LABEL: Record<ReconciliationRow["highlight"], string | null> = {
  none: null,
  yellow: "Over 5 % change",
  red: "Over 10 % change",
};

const KIND_LABEL: Record<ReconciliationRow["kind"], string> = {
  new: "New holder",
  departed: "Departed holder",
  changed: "Changed balance",
  unchanged: "Unchanged",
};

const number = new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 });
const signed = (n: number) => `${n > 0 ? "+" : ""}${number.format(n)}`;
const short = (a: string) => (a.length > 14 ? `${a.slice(0, 6)}…${a.slice(-6)}` : a);

function reportLines(report: ReconciliationReport): string[] {
  const lines = [
    `Asset ${report.assetId}  -  source: ${report.source}  -  generated ${report.generatedAt}`,
    `Total supply ${number.format(report.totalSupply)} tokens  -  ${report.tokensPerShare} token(s) per share`,
    `New ${report.counts.new}, departed ${report.counts.departed}, changed ${report.counts.changed}, unchanged ${report.counts.unchanged}; ${report.complianceAlerts} over 1 % of supply`,
    "",
    "Holder                                                     On-chain        Filing        Change      %   Flags",
  ];
  for (const r of report.rows) {
    const flags = [HIGHLIGHT_LABEL[r.highlight], r.complianceAlert ? ">1% supply" : null].filter(Boolean).join(", ");
    lines.push(
      `${r.address.padEnd(56)} ${number.format(r.onChain).padStart(12)} ${number.format(r.filing).padStart(12)} ${signed(r.absoluteChange).padStart(12)} ${signed(r.percentChange).padStart(7)}   ${KIND_LABEL[r.kind]}${flags ? ` (${flags})` : ""}`
    );
    if (r.ownerName || r.ownerCik) lines.push(`    ${r.ownerName ?? ""}${r.ownerCik ? ` CIK ${r.ownerCik}` : ""}`);
  }
  return lines;
}

function download(filename: string, bytes: Uint8Array<ArrayBuffer>, type: string) {
  const blob = new Blob([bytes], { type });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/**
 * Side-by-side comparison of the on-chain cap table with holdings from SEC
 * EDGAR Form 4 filings (issue #476). Upload a CSV with `address` and `shares`
 * columns (optionally `owner_cik`, `owner_name`); the table flags new,
 * departed and changed holders, highlights changes over 5 % (yellow) and
 * 10 % (red), and marks discrepancies over 1 % of supply. "Export signed
 * PDF" has the API sign the report and embeds the signed JSON in the PDF.
 */
export function CapTableEdgarCompare({
  assetId,
  apiBaseUrl = API_BASE_URL,
  holders: initialHolders,
  totalSupply: initialSupply,
  decimals: initialDecimals,
  filing: initialFiling,
  fetchImpl,
}: CapTableEdgarCompareProps) {
  const ids = useId();
  const doFetch: FetchLike = fetchImpl ?? ((input, init) => fetch(input, init));
  const [holders, setHolders] = useState<OnChainHolder[] | null>(initialHolders ?? null);
  const [supply, setSupply] = useState<{ totalSupply: string; decimals: number } | null>(
    initialSupply !== undefined ? { totalSupply: initialSupply, decimals: initialDecimals ?? 0 } : null
  );
  const [filing, setFiling] = useState<FilingHolder[] | null>(initialFiling ?? null);
  const [fileName, setFileName] = useState<string | null>(null);
  const [tokensPerShare, setTokensPerShare] = useState("1");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [csvError, setCsvError] = useState<string | null>(null);
  const [adminToken, setAdminToken] = useState("");
  const [exportState, setExportState] = useState<{ status: "idle" | "signing" | "done" | "error"; message?: string }>({
    status: "idle",
  });

  const root = `${apiBaseUrl.replace(/\/$/, "")}/v1/assets/${assetId}`;

  useEffect(() => {
    if (initialHolders && initialSupply !== undefined) return;
    let cancelled = false;
    (async () => {
      try {
        const [asset, list] = await Promise.all([doFetch(root), doFetch(`${root}/holders?limit=1000`)]);
        if (!asset.ok || !list.ok) throw new Error(`HTTP ${asset.ok ? list.status : asset.status}`);
        const a = (await asset.json()) as { total_supply: string; decimals: number };
        const h = (await list.json()) as Array<{ address: string; balance: string }>;
        if (cancelled) return;
        setSupply({ totalSupply: a.total_supply, decimals: a.decimals });
        setHolders(h.map(({ address, balance }) => ({ address, balance })));
      } catch (e) {
        if (!cancelled) setLoadError(`Could not load on-chain holders (${(e as Error).message}).`);
      }
    })();
    return () => {
      cancelled = true;
    };
    // Load once per asset.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  const ratio = Number(tokensPerShare);
  const ratioValid = Number.isFinite(ratio) && ratio > 0;

  const report = useMemo<ReconciliationReport | null>(() => {
    if (!holders || !supply || !filing || !ratioValid) return null;
    return reconcile(holders, filing, {
      assetId: String(assetId),
      decimals: supply.decimals,
      totalSupply: supply.totalSupply,
      tokensPerShare: ratio,
      source: "csv",
    });
  }, [holders, supply, filing, ratio, ratioValid, assetId]);

  const onFile = async (file: File | undefined) => {
    setCsvError(null);
    setExportState({ status: "idle" });
    if (!file) return;
    try {
      const parsed = parseFilingCsv(await file.text());
      setFiling(parsed);
      setFileName(file.name);
    } catch (e) {
      setFiling(null);
      setCsvError(e instanceof CsvFormatError ? e.message : "The file could not be read as CSV.");
    }
  };

  const exportPdf = async () => {
    if (!report) return;
    setExportState({ status: "signing" });
    try {
      const resp = await doFetch(`${apiBaseUrl.replace(/\/$/, "")}/v1/admin/cap-table/reports/sign`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
        body: JSON.stringify({ report }),
      });
      if (!resp.ok) throw new Error(resp.status === 401 ? "the admin token was rejected" : `HTTP ${resp.status}`);
      const sig = (await resp.json()) as ReportSignature;
      const pdf = buildSignedPdf({
        title: `Cap table reconciliation - asset ${report.assetId}`,
        lines: reportLines(report),
        signature: {
          algorithm: sig.algorithm,
          digest: sig.digest,
          signature: sig.signature,
          publicKey: sig.public_key,
          signedAt: sig.signed_at,
        },
        signedJson: canonicalJson(report),
      });
      download(`cap-table-reconciliation-${report.assetId}.pdf`, pdf, "application/pdf");
      setExportState({ status: "done", message: `Signed (SHA-256 ${sig.digest.slice(0, 16)}…).` });
    } catch (e) {
      setExportState({ status: "error", message: `Signing failed: ${(e as Error).message}.` });
    }
  };

  return (
    <section aria-labelledby={`${ids}-title`} className="my-8 rounded-2xl border border-white/10 bg-base-900/70 p-6">
      <h3 id={`${ids}-title`} className="text-lg font-semibold text-white">
        On-chain cap table vs EDGAR Form 4 filings
      </h3>

      <div className="mt-4 grid gap-4 md:grid-cols-3">
        <label className="flex flex-col gap-1 text-sm text-base-100">
          Form 4 holdings CSV
          <input
            type="file"
            accept=".csv,text/csv"
            onChange={(e) => void onFile(e.target.files?.[0])}
            className="text-xs text-base-200"
          />
          {fileName && <span className="text-xs text-base-200/70">{fileName}</span>}
        </label>
        <label className="flex flex-col gap-1 text-sm text-base-100">
          Tokens per share
          <input
            inputMode="decimal"
            value={tokensPerShare}
            onChange={(e) => setTokensPerShare(e.target.value)}
            aria-invalid={!ratioValid}
            className="rounded-lg border border-white/10 bg-base-950/80 px-2 py-1 text-white"
          />
        </label>
        <label className="flex flex-col gap-1 text-sm text-base-100">
          Admin token (for signing)
          <input
            type="password"
            autoComplete="off"
            value={adminToken}
            onChange={(e) => setAdminToken(e.target.value)}
            className="rounded-lg border border-white/10 bg-base-950/80 px-2 py-1 text-white"
          />
        </label>
      </div>

      {loadError && (
        <p role="alert" className="mt-4 text-sm text-red-300">
          {loadError}
        </p>
      )}
      {csvError && (
        <p role="alert" className="mt-4 text-sm text-red-300">
          {csvError}
        </p>
      )}
      {!ratioValid && <p className="mt-2 text-xs text-red-300">Tokens per share must be a positive number.</p>}

      {!report && !loadError && !csvError && (
        <p className="mt-4 text-sm text-base-200/80">
          {holders ? "Upload a CSV exported from EDGAR Form 4 filings to compare." : "Loading on-chain holders…"}
        </p>
      )}

      {report && (
        <>
          <p className="mt-4 text-sm text-base-100" aria-live="polite">
            {report.counts.new} new, {report.counts.departed} departed, {report.counts.changed} changed,{" "}
            {report.counts.unchanged} unchanged.{" "}
            <strong>
              {report.complianceAlerts} discrepanc{report.complianceAlerts === 1 ? "y" : "ies"} over 1 % of total supply.
            </strong>
          </p>
          <div className="mt-3 overflow-x-auto">
            <table className="w-full text-left text-sm">
              <caption className="sr-only">Holder-by-holder reconciliation</caption>
              <thead className="text-xs uppercase text-base-200/70">
                <tr>
                  <th scope="col" className="py-2">Holder</th>
                  <th scope="col" className="py-2 text-right">On-chain</th>
                  <th scope="col" className="py-2 text-right">EDGAR filing</th>
                  <th scope="col" className="py-2 text-right">Change</th>
                  <th scope="col" className="py-2 text-right">Change %</th>
                  <th scope="col" className="py-2">Status</th>
                </tr>
              </thead>
              <tbody>
                {report.rows.map((r) => (
                  <tr key={r.address} data-highlight={r.highlight} data-kind={r.kind} className={`border-t border-white/5 ${HIGHLIGHT_ROW[r.highlight]}`}>
                    <th scope="row" className="py-2 font-normal">
                      <span className="font-mono text-xs text-white" title={r.address}>
                        {short(r.address)}
                      </span>
                      {r.ownerName && <span className="block text-xs text-base-200/70">{r.ownerName}</span>}
                    </th>
                    <td className="py-2 text-right tabular-nums">{number.format(r.onChain)}</td>
                    <td className="py-2 text-right tabular-nums">{number.format(r.filing)}</td>
                    <td className="py-2 text-right tabular-nums">{signed(r.absoluteChange)}</td>
                    <td className="py-2 text-right tabular-nums">{signed(r.percentChange)} %</td>
                    <td className="py-2">
                      <span>{KIND_LABEL[r.kind]}</span>
                      {HIGHLIGHT_LABEL[r.highlight] && (
                        <span className={`ml-2 rounded px-1.5 py-0.5 text-xs ${r.highlight === "red" ? "bg-red-500/30 text-red-100" : "bg-amber-500/30 text-amber-100"}`}>
                          {HIGHLIGHT_LABEL[r.highlight]}
                        </span>
                      )}
                      {r.complianceAlert && (
                        <span className="ml-2 rounded bg-fuchsia-500/30 px-1.5 py-0.5 text-xs text-fuchsia-100">Over 1 % of supply</span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="mt-4 flex flex-wrap items-center gap-3">
            <button
              type="button"
              onClick={() => void exportPdf()}
              disabled={!adminToken || exportState.status === "signing"}
              className="rounded-xl bg-brand-500 px-4 py-2 text-sm font-semibold text-base-950 disabled:opacity-50"
            >
              {exportState.status === "signing" ? "Signing…" : "Export signed PDF"}
            </button>
            {exportState.message && (
              <span role={exportState.status === "error" ? "alert" : "status"} className="text-xs text-base-200">
                {exportState.message}
              </span>
            )}
          </div>
        </>
      )}
    </section>
  );
}

export default CapTableEdgarCompare;
