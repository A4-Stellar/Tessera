"use client";

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useWebSocket } from "@/hooks/useWebSocket";
import { API_BASE_URL } from "@/lib/api";
import {
  MAX_COMPARED_ASSETS,
  buildComparisonCsv,
  buildComparisonJson,
  buildComparisonRows,
  concentrationBand,
  giniCoefficient,
  inferDividendSchedule,
  trailingAnnualYieldPercent,
  type CompareAsset,
  type ComparisonRow,
} from "@/lib/cap-table-compare";

export type { CompareAsset } from "@/lib/cap-table-compare";

/** Illustrative assets used when no data or API source is given. */
export const SAMPLE_COMPARE_ASSETS: CompareAsset[] = [
  {
    id: "loft",
    name: "Manhattan Loft",
    symbol: "LOFT",
    assetType: "real_estate",
    valuationUsd: 5_000_000,
    holderCount: 412,
    giniCoefficient: 0.38,
    topHolderSharePercent: 6.5,
    minInvestmentUsd: 500,
    minInvestmentBasis: "issuer",
    annualYieldPercent: 5.4,
    dividendSchedule: { frequency: "quarterly", distributions: 8, medianIntervalDays: 91 },
    compliance: { jurisdictions: ["US"], accreditedOnly: true, lockupDays: 365, approvedPercent: 97.2 },
  },
  {
    id: "invc",
    name: "Invoice Pool 2026-A",
    symbol: "INVC",
    assetType: "invoice",
    valuationUsd: 1_200_000,
    holderCount: 58,
    giniCoefficient: 0.71,
    topHolderSharePercent: 31,
    minInvestmentUsd: 5_000,
    minInvestmentBasis: "issuer",
    annualYieldPercent: 9.1,
    dividendSchedule: { frequency: "monthly", distributions: 14, medianIntervalDays: 30 },
    compliance: { jurisdictions: ["DE", "FR", "NL"], accreditedOnly: true, lockupDays: 90, approvedPercent: 100 },
  },
  {
    id: "gold",
    name: "Vaulted Gold",
    symbol: "GOLDX",
    assetType: "commodity",
    valuationUsd: 18_750_000,
    holderCount: 2_890,
    giniCoefficient: 0.52,
    topHolderSharePercent: 12.4,
    minInvestmentUsd: 25,
    minInvestmentBasis: "issuer",
    annualYieldPercent: 0,
    dividendSchedule: { frequency: "none", distributions: 0, medianIntervalDays: null },
    compliance: { jurisdictions: ["AE", "GB", "SG", "US"], accreditedOnly: false, lockupDays: 0, approvedPercent: 88.9 },
  },
  {
    id: "solar",
    name: "Solar Farm Notes",
    symbol: "SOLR",
    assetType: "infrastructure",
    valuationUsd: 9_400_000,
    holderCount: 1_120,
    giniCoefficient: 0.44,
    topHolderSharePercent: 8.1,
    minInvestmentUsd: 1_000,
    minInvestmentBasis: "issuer",
    annualYieldPercent: 6.8,
    dividendSchedule: { frequency: "semi-annual", distributions: 4, medianIntervalDays: 183 },
    compliance: { jurisdictions: ["BR", "MX"], accreditedOnly: false, lockupDays: 180, approvedPercent: 92.5 },
  },
  {
    id: "art",
    name: "Contemporary Art Fund",
    symbol: "ARTF",
    assetType: "collectible",
    valuationUsd: 3_300_000,
    holderCount: 205,
    giniCoefficient: 0.63,
    topHolderSharePercent: 18.7,
    minInvestmentUsd: 2_500,
    minInvestmentBasis: "issuer",
    annualYieldPercent: null,
    dividendSchedule: { frequency: "irregular", distributions: 2, medianIntervalDays: 410 },
    compliance: { jurisdictions: ["CH", "GB"], accreditedOnly: true, lockupDays: null, approvedPercent: 95 },
  },
];

// ── API source ──────────────────────────────────────────────────────────────

interface ApiAsset {
  id: number;
  token_contract: string;
  name: string;
  symbol: string;
  asset_type: string;
  valuation_usd: number;
  decimals: number;
  total_supply: string;
  holders: number;
  indexed_at_ledger: number;
}

type FetchLike = (input: string) => Promise<{ ok: boolean; status: number; json: () => Promise<unknown> }>;

async function getJson<T>(fetchImpl: FetchLike, url: string): Promise<T> {
  const response = await fetchImpl(url);
  if (!response.ok) throw new Error(`GET ${url} failed with HTTP ${response.status}`);
  return (await response.json()) as T;
}

/**
 * Loads one asset's comparison metrics from the Tessera API. Values the API
 * does not expose (accreditation, lock-up) stay `null` and render as
 * "Not published"; the minimum investment falls back to the price of one token.
 */
export async function loadCompareAsset(baseUrl: string, asset: ApiAsset, fetchImpl: FetchLike = fetch): Promise<CompareAsset> {
  const root = `${baseUrl.replace(/\/+$/, "")}/v1/assets/${asset.id}`;
  const [analytics, holders, dividends, compliance] = await Promise.all([
    getJson<{ gini_coefficient?: number | null }>(fetchImpl, `${root}/analytics`).catch(() => null),
    getJson<Array<{ balance: string; share_percent: number }>>(fetchImpl, `${root}/holders?limit=1000`),
    getJson<Array<{ created_at_ledger: number; fiat_equivalent_usd?: number | null }>>(fetchImpl, `${root}/dividends`),
    getJson<{ total_records: number; approved: number; jurisdictions: Array<{ jurisdiction: string; count: number }> }>(
      fetchImpl,
      `${root}/compliance`
    ),
  ]);

  const supplyTokens = Number(asset.total_supply) / 10 ** asset.decimals;
  const gini = analytics?.gini_coefficient ?? giniCoefficient(holders.map((h) => Number(h.balance)));
  return {
    id: String(asset.id),
    name: asset.name,
    symbol: asset.symbol,
    assetType: asset.asset_type,
    valuationUsd: asset.valuation_usd > 0 ? asset.valuation_usd : null,
    holderCount: asset.holders,
    giniCoefficient: gini,
    topHolderSharePercent: holders.length > 0 ? Math.max(...holders.map((h) => h.share_percent)) : null,
    minInvestmentUsd: supplyTokens > 0 && asset.valuation_usd > 0 ? asset.valuation_usd / supplyTokens : null,
    minInvestmentBasis: "one-token",
    annualYieldPercent: trailingAnnualYieldPercent(
      dividends.map((d) => ({ createdAtLedger: d.created_at_ledger, fiatEquivalentUsd: d.fiat_equivalent_usd ?? null })),
      asset.valuation_usd,
      asset.indexed_at_ledger
    ),
    dividendSchedule: inferDividendSchedule(dividends.map((d) => d.created_at_ledger)),
    compliance: {
      jurisdictions: compliance.jurisdictions.map((j) => j.jurisdiction),
      accreditedOnly: null,
      lockupDays: null,
      approvedPercent: compliance.total_records > 0 ? (compliance.approved / compliance.total_records) * 100 : null,
    },
  };
}

function wsUrlFor(baseUrl: string): string {
  return `${baseUrl.replace(/\/+$/, "").replace(/\/v1$/, "").replace(/^http/, "ws")}/v1/ws`;
}

/** Re-fetches assets whose token contract appears in pushed events. */
function LiveRefresh({ url, contracts, onChange }: { url: string; contracts: Map<string, string>; onChange: (id: string) => void }) {
  const handleMessage = useCallback(
    (raw: unknown) => {
      if (typeof raw !== "object" || raw === null) return;
      const id = contracts.get(String((raw as { contract?: unknown }).contract ?? ""));
      if (id) onChange(id);
    },
    [contracts, onChange]
  );
  useWebSocket({ url, autoReconnect: true, onMessage: handleMessage });
  return null;
}

// ── Export ──────────────────────────────────────────────────────────────────

function download(filename: string, content: string, type: string) {
  if (typeof URL === "undefined" || typeof URL.createObjectURL !== "function") return;
  const url = URL.createObjectURL(new Blob([content], { type }));
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

// ── Component ───────────────────────────────────────────────────────────────

const BAND_STYLE: Record<ReturnType<typeof concentrationBand>, string> = {
  low: "bg-emerald-400",
  moderate: "bg-amber-400",
  high: "bg-rose-400",
  unknown: "bg-slate-500",
};

const RANK_LABEL = { best: "Most favourable", worst: "Least favourable" } as const;

interface CapTableCompareProps {
  /** Assets available for comparison. Defaults to an illustrative sample set when no API source is used. */
  assets?: CompareAsset[];
  /** IDs selected on first render (at most four). Defaults to the first two assets. */
  initialSelection?: string[];
  /** Load the asset list and metrics from the Tessera API instead of `assets`. */
  source?: "props" | "api";
  /** API base URL for `source="api"`; defaults to `NEXT_PUBLIC_API_BASE_URL`. */
  apiBaseUrl?: string;
  /** With `source="api"`, refresh an asset's metrics when the WebSocket stream reports activity for its token contract. */
  live?: boolean;
  /** Called with the export payload; the file is still downloaded. Useful for analytics or tests. */
  onExport?: (format: "csv" | "json", content: string) => void;
}

/**
 * Side-by-side comparison of up to four tokenized assets: holder concentration
 * (Gini coefficient, largest holder), minimum investment, dividend yield and
 * schedule, and compliance rules. Cells that differ from the first selected
 * asset are highlighted and annotated with the difference; the table can be
 * exported as CSV or JSON.
 */
export function CapTableCompare({
  assets: assetsProp,
  initialSelection,
  source = "props",
  apiBaseUrl = API_BASE_URL,
  live = false,
  onExport,
}: CapTableCompareProps) {
  const headingId = useId();
  const [catalog, setCatalog] = useState<CompareAsset[]>(assetsProp ?? (source === "props" ? SAMPLE_COMPARE_ASSETS : []));
  const [apiAssets, setApiAssets] = useState<Map<string, ApiAsset>>(new Map());
  const [selected, setSelected] = useState<string[]>(() =>
    (initialSelection ?? (assetsProp ?? SAMPLE_COMPARE_ASSETS).slice(0, 2).map((a) => a.id)).slice(0, MAX_COMPARED_ASSETS)
  );
  const [onlyDifferences, setOnlyDifferences] = useState(false);
  const [status, setStatus] = useState<"idle" | "loading" | "error">(source === "api" ? "loading" : "idle");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const [updatedAt, setUpdatedAt] = useState<Date | null>(null);
  const refreshTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  useEffect(() => {
    if (assetsProp) setCatalog(assetsProp);
  }, [assetsProp]);

  const refreshAsset = useCallback(
    async (id: string, summaries: Map<string, ApiAsset> = apiAssets) => {
      const summary = summaries.get(id);
      if (!summary) return;
      try {
        const loaded = await loadCompareAsset(apiBaseUrl, summary);
        setCatalog((prev) => prev.map((a) => (a.id === id ? loaded : a)));
        setUpdatedAt(new Date());
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [apiAssets, apiBaseUrl]
  );

  // API mode: list assets, then load metrics for the selection.
  useEffect(() => {
    if (source !== "api") return;
    let cancelled = false;
    (async () => {
      try {
        setStatus("loading");
        const list = await getJson<ApiAsset[]>(fetch as FetchLike, `${apiBaseUrl.replace(/\/+$/, "")}/v1/assets?active=true`);
        if (cancelled) return;
        const summaries = new Map(list.map((a) => [String(a.id), a] as const));
        setApiAssets(summaries);
        const initial = (initialSelection ?? list.slice(0, 2).map((a) => String(a.id))).filter((id) => summaries.has(id)).slice(0, MAX_COMPARED_ASSETS);
        setSelected(initial);
        const loaded = await Promise.all(list.map((a) =>
          initial.includes(String(a.id))
            ? loadCompareAsset(apiBaseUrl, a)
            : Promise.resolve<CompareAsset>({
                id: String(a.id),
                name: a.name,
                symbol: a.symbol,
                assetType: a.asset_type,
                valuationUsd: a.valuation_usd,
                holderCount: a.holders,
                giniCoefficient: null,
                topHolderSharePercent: null,
                minInvestmentUsd: null,
                annualYieldPercent: null,
                dividendSchedule: { frequency: "none", distributions: 0, medianIntervalDays: null },
                compliance: { jurisdictions: [], accreditedOnly: null, lockupDays: null, approvedPercent: null },
              })
        ));
        if (cancelled) return;
        setCatalog(loaded);
        setUpdatedAt(new Date());
        setStatus("idle");
      } catch (e) {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setStatus("error");
      }
    })();
    return () => {
      cancelled = true;
    };
    // initialSelection is only read on first load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source, apiBaseUrl]);

  useEffect(() => {
    const timers = refreshTimers.current;
    return () => timers.forEach((timer) => clearTimeout(timer));
  }, []);

  const scheduleRefresh = useCallback(
    (id: string) => {
      const timers = refreshTimers.current;
      clearTimeout(timers.get(id));
      timers.set(id, setTimeout(() => void refreshAsset(id), 750));
    },
    [refreshAsset]
  );

  const toggle = (id: string) => {
    setSelected((prev) => {
      if (prev.includes(id)) {
        setNotice("");
        return prev.filter((x) => x !== id);
      }
      if (prev.length >= MAX_COMPARED_ASSETS) {
        setNotice(`You can compare up to ${MAX_COMPARED_ASSETS} assets. Deselect one to add another.`);
        return prev;
      }
      setNotice("");
      if (source === "api") void refreshAsset(id);
      return [...prev, id];
    });
  };

  const compared = useMemo(
    () => selected.map((id) => catalog.find((a) => a.id === id)).filter((a): a is CompareAsset => Boolean(a)),
    [selected, catalog]
  );
  const rows = useMemo(() => buildComparisonRows(compared), [compared]);
  const visibleRows = useMemo<ComparisonRow[]>(
    () => (onlyDifferences && compared.length > 1 ? rows.filter((r) => r.hasDifference) : rows),
    [rows, onlyDifferences, compared.length]
  );
  const groups = useMemo(() => {
    const map = new Map<string, ComparisonRow[]>();
    for (const row of visibleRows) map.set(row.metric.group, [...(map.get(row.metric.group) ?? []), row]);
    return [...map.entries()];
  }, [visibleRows]);

  const liveContracts = useMemo(
    () => new Map([...apiAssets.values()].filter((a) => selected.includes(String(a.id))).map((a) => [a.token_contract, String(a.id)] as const)),
    [apiAssets, selected]
  );

  const exportAs = (format: "csv" | "json") => {
    const content = format === "csv" ? buildComparisonCsv(compared, rows) : buildComparisonJson(compared);
    onExport?.(format, content);
    download(`cap-table-comparison.${format}`, content, format === "csv" ? "text/csv;charset=utf-8" : "application/json");
  };

  const baseline = compared[0];

  return (
    <section aria-labelledby={headingId} className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      {live && source === "api" && liveContracts.size > 0 && (
        <LiveRefresh url={wsUrlFor(apiBaseUrl)} contracts={liveContracts} onChange={scheduleRefresh} />
      )}
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <h3 id={headingId} className="text-base font-semibold text-base-100">
            Cap-Table Comparison
          </h3>
          <p className="text-xs text-base-300">
            Compare up to {MAX_COMPARED_ASSETS} assets. Highlighted cells differ from the first selected asset
            {baseline ? ` (${baseline.symbol})` : ""}.
            {source === "props" && !assetsProp ? " Showing illustrative sample data." : ""}
          </p>
        </div>
        {live && source === "api" && (
          <span className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2.5 py-1 text-[11px] font-semibold uppercase tracking-wide text-emerald-300">
            ● Live{updatedAt ? ` · updated ${updatedAt.toLocaleTimeString()}` : ""}
          </span>
        )}
      </div>

      {status === "loading" && (
        <p role="status" className="text-sm text-base-300">
          Loading assets…
        </p>
      )}
      {error && (
        <p role="alert" className="mb-3 rounded-md border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-sm text-rose-200">
          Could not load comparison data: {error}
        </p>
      )}

      <fieldset className="mb-4">
        <legend className="mb-2 text-sm font-semibold text-base-200">
          Assets to compare ({selected.length} of {MAX_COMPARED_ASSETS} selected)
        </legend>
        <div className="flex flex-wrap gap-2">
          {catalog.map((asset) => {
            const checked = selected.includes(asset.id);
            const disabled = !checked && selected.length >= MAX_COMPARED_ASSETS;
            return (
              <label
                key={asset.id}
                className={`inline-flex items-center gap-2 rounded-md border px-2.5 py-1.5 text-sm ${
                  checked ? "border-sky-400/60 bg-sky-400/10 text-base-100" : "border-white/10 text-base-300"
                } ${disabled ? "opacity-50" : "cursor-pointer"}`}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={disabled}
                  onChange={() => toggle(asset.id)}
                  aria-describedby={disabled ? `${headingId}-limit` : undefined}
                />
                <span>
                  {asset.name} <span className="font-mono text-xs text-base-300">({asset.symbol})</span>
                </span>
              </label>
            );
          })}
        </div>
        <p id={`${headingId}-limit`} aria-live="polite" className="mt-2 min-h-[1rem] text-xs text-amber-300">
          {notice || (selected.length >= MAX_COMPARED_ASSETS ? `Maximum of ${MAX_COMPARED_ASSETS} assets selected.` : "")}
        </p>
      </fieldset>

      {compared.length === 0 ? (
        <p className="text-sm text-base-300">Select at least one asset to start comparing.</p>
      ) : (
        <>
          <div className="mb-4 grid gap-3" style={{ gridTemplateColumns: `repeat(${compared.length}, minmax(0, 1fr))` }}>
            {compared.map((asset) => {
              const band = concentrationBand(asset.giniCoefficient);
              const percent = asset.giniCoefficient === null ? 0 : Math.round(asset.giniCoefficient * 100);
              return (
                <div key={asset.id} className="rounded-lg border border-white/10 p-3">
                  <p className="text-sm font-semibold text-base-100">{asset.symbol}</p>
                  <p className="text-xs text-base-300">Investor concentration: {band}</p>
                  <div
                    role="img"
                    aria-label={`${asset.symbol} Gini coefficient ${asset.giniCoefficient === null ? "not published" : asset.giniCoefficient.toFixed(3)}, ${band} concentration`}
                    className="mt-2 h-2 w-full overflow-hidden rounded-full bg-white/10"
                  >
                    <div className={`h-full ${BAND_STYLE[band]}`} style={{ width: `${percent}%` }} />
                  </div>
                </div>
              );
            })}
          </div>

          <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
            <label className="inline-flex items-center gap-2 text-sm text-base-200">
              <input type="checkbox" checked={onlyDifferences} onChange={(e) => setOnlyDifferences(e.target.checked)} />
              Show only rows that differ
            </label>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => exportAs("csv")}
                className="rounded-md border border-white/15 px-3 py-1.5 text-sm text-base-100 hover:bg-white/10"
              >
                Export CSV
              </button>
              <button
                type="button"
                onClick={() => exportAs("json")}
                className="rounded-md border border-white/15 px-3 py-1.5 text-sm text-base-100 hover:bg-white/10"
              >
                Export JSON
              </button>
            </div>
          </div>

          <div className="overflow-x-auto">
            <table className="w-full min-w-[36rem] text-sm">
              <caption className="mb-2 text-left text-xs font-semibold text-base-300">
                Comparison of {compared.map((a) => a.symbol).join(", ")}
              </caption>
              <thead>
                <tr className="border-b border-white/10">
                  <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
                    Metric
                  </th>
                  {compared.map((asset, i) => (
                    <th key={asset.id} scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
                      {asset.name} <span className="font-mono text-xs text-base-300">({asset.symbol})</span>
                      {i === 0 && compared.length > 1 && <span className="block text-[11px] font-normal text-base-300">Baseline</span>}
                    </th>
                  ))}
                </tr>
              </thead>
              {groups.map(([group, groupRows]) => (
                <tbody key={group}>
                  <tr>
                    <th scope="colgroup" colSpan={compared.length + 1} className="px-2 pb-1 pt-3 text-left text-xs uppercase tracking-wide text-base-300">
                      {group}
                    </th>
                  </tr>
                  {groupRows.map((row) => (
                    <tr key={row.metric.key} className="border-b border-white/5">
                      <th scope="row" className="px-2 py-1.5 text-left font-medium text-base-200">
                        {row.metric.label}
                      </th>
                      {row.cells.map((cell, i) => {
                        const highlight = cell.differs
                          ? row.metric.emphasised
                            ? "bg-amber-400/15 ring-1 ring-inset ring-amber-400/50"
                            : "bg-sky-400/10"
                          : "";
                        return (
                          <td key={compared[i].id} data-differs={cell.differs || undefined} className={`px-2 py-1.5 align-top text-base-200 ${highlight}`}>
                            <span>{cell.display}</span>
                            {row.metric.key === "minInvestmentUsd" && compared[i].minInvestmentBasis === "one-token" && cell.value !== null && (
                              <span className="block text-[11px] text-base-300">price of one token</span>
                            )}
                            {cell.delta && (
                              <span className="block text-[11px] text-base-300">
                                {cell.delta} vs {baseline.symbol}
                              </span>
                            )}
                            {cell.differs && !cell.delta && <span className="sr-only"> (differs from {baseline.symbol})</span>}
                            {cell.rank && <span className="block text-[11px] font-semibold text-emerald-300">{RANK_LABEL[cell.rank]}</span>}
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
              ))}
            </table>
          </div>
          {onlyDifferences && visibleRows.length === 0 && (
            <p className="mt-2 text-sm text-base-300">The selected assets match on every metric.</p>
          )}
        </>
      )}
    </section>
  );
}

export default CapTableCompare;
