"use client";

import { useEffect, useMemo, useState } from "react";
import { Cell, Legend, Pie, PieChart, ResponsiveContainer, Tooltip } from "recharts";
import { API_BASE_URL } from "@/lib/api";

interface Holder {
  address: string;
  balance: string;
  share_percent: number;
}

interface JurisdictionCount {
  jurisdiction: string;
  count: number;
}

interface ComplianceSummary {
  total_records: number;
  approved: number;
  suspended: number;
  rejected: number;
  pending: number;
  with_expiry: number;
  jurisdictions: JurisdictionCount[];
}

interface CapTableVisualizerProps {
  /** Registry id of the asset to visualize. */
  assetId: string | number;
  /** Overrides the configured API base URL. */
  apiBaseUrl?: string;
  /** Number of individual holders to break out before grouping the rest into "Other". */
  topHolderCount?: number;
}

type View = "distribution" | "jurisdictions";

/** Colors drawn from the docs' Tailwind palette (base, brand, gold, plus sky/red accents). */
const SLICE_COLORS = ["#10b981", "#34d399", "#f59e0b", "#0ea5e9", "#a5abba", "#ef4444", "#6ee7b7"];

function truncateAddress(address: string): string {
  return address.length > 12 ? `${address.slice(0, 6)}…${address.slice(-4)}` : address;
}

/**
 * Interactive donut charts for a tokenized asset's holder concentration and
 * jurisdiction breakdown, fed from the live indexer API. Includes an
 * accessible data table so the same information is available without the
 * chart.
 */
export function CapTableVisualizer({ assetId, apiBaseUrl, topHolderCount = 6 }: CapTableVisualizerProps) {
  const baseUrl = apiBaseUrl ?? API_BASE_URL;
  const [view, setView] = useState<View>("distribution");
  const [holders, setHolders] = useState<Holder[] | null>(null);
  const [compliance, setCompliance] = useState<ComplianceSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    Promise.all([
      fetch(`${baseUrl.replace(/\/+$/, "")}/assets/${assetId}/holders`).then((r) => {
        if (!r.ok) throw new Error(`Holders request failed (${r.status})`);
        return r.json() as Promise<Holder[]>;
      }),
      fetch(`${baseUrl.replace(/\/+$/, "")}/assets/${assetId}/compliance`).then((r) => {
        if (!r.ok) throw new Error(`Compliance request failed (${r.status})`);
        return r.json() as Promise<ComplianceSummary>;
      }),
    ])
      .then(([holdersData, complianceData]) => {
        if (cancelled) return;
        setHolders(holdersData);
        setCompliance(complianceData);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : "Failed to load holder data.");
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [assetId, baseUrl]);

  const distributionData = useMemo(() => {
    if (!holders) return [];
    const sorted = [...holders].sort((a, b) => b.share_percent - a.share_percent);
    const top = sorted.slice(0, topHolderCount);
    const rest = sorted.slice(topHolderCount);
    const restShare = rest.reduce((sum, h) => sum + h.share_percent, 0);
    const rows = top.map((h) => ({ name: truncateAddress(h.address), value: h.share_percent }));
    if (rest.length > 0) rows.push({ name: "Other", value: restShare });
    return rows;
  }, [holders, topHolderCount]);

  const jurisdictionData = useMemo(() => {
    if (!compliance) return [];
    return compliance.jurisdictions.map((j) => ({ name: j.jurisdiction, value: j.count }));
  }, [compliance]);

  const activeData = view === "distribution" ? distributionData : jurisdictionData;
  const chartLabel =
    view === "distribution"
      ? `Token distribution across ${distributionData.length} holder groups for asset ${assetId}`
      : `Compliance jurisdiction breakdown across ${jurisdictionData.length} jurisdictions for asset ${assetId}`;

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="mb-4 flex gap-2" role="tablist" aria-label="Cap table view">
        <button
          type="button"
          role="tab"
          aria-selected={view === "distribution"}
          onClick={() => setView("distribution")}
          className={`rounded-md border px-3 py-1.5 text-sm font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
            view === "distribution"
              ? "border-brand-500/40 bg-brand-500/15 text-brand-300"
              : "border-white/10 text-base-300 hover:text-base-100"
          }`}
        >
          Token Distribution
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={view === "jurisdictions"}
          onClick={() => setView("jurisdictions")}
          className={`rounded-md border px-3 py-1.5 text-sm font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
            view === "jurisdictions"
              ? "border-brand-500/40 bg-brand-500/15 text-brand-300"
              : "border-white/10 text-base-300 hover:text-base-100"
          }`}
        >
          Jurisdictions
        </button>
      </div>

      <div aria-live="polite">
        {loading && <p className="text-sm text-base-300">Loading holder data…</p>}
        {error && (
          <p role="alert" className="text-sm text-red-300">
            {error}
          </p>
        )}
      </div>

      {!loading && !error && activeData.length > 0 && (
        <>
          <div
            role="img"
            aria-label={chartLabel}
            className="h-72 w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
            tabIndex={0}
          >
            <ResponsiveContainer width="100%" height="100%">
              <PieChart>
                <Pie
                  data={activeData}
                  dataKey="value"
                  nameKey="name"
                  innerRadius="55%"
                  outerRadius="80%"
                  paddingAngle={2}
                >
                  {activeData.map((entry, index) => (
                    <Cell key={entry.name} fill={SLICE_COLORS[index % SLICE_COLORS.length]} />
                  ))}
                </Pie>
                <Tooltip
                  formatter={(value: number) => `${value.toFixed(2)}%`}
                  contentStyle={{ background: "#171b24", border: "1px solid rgba(255,255,255,0.1)" }}
                />
                <Legend />
              </PieChart>
            </ResponsiveContainer>
          </div>

          <table className="mt-4 w-full text-sm">
            <caption className="mb-2 text-left text-xs font-semibold text-base-300">
              {view === "distribution" ? "Holder breakdown" : "Jurisdiction breakdown"} (data table)
            </caption>
            <thead>
              <tr className="border-b border-white/10">
                <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
                  {view === "distribution" ? "Holder" : "Jurisdiction"}
                </th>
                <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
                  {view === "distribution" ? "Share" : "Count"}
                </th>
              </tr>
            </thead>
            <tbody>
              {activeData.map((row) => (
                <tr key={row.name} className="border-b border-white/5">
                  <td className="px-2 py-1.5 font-mono text-base-200">{row.name}</td>
                  <td className="px-2 py-1.5 text-base-300">
                    {view === "distribution" ? `${row.value.toFixed(2)}%` : row.value}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {!loading && !error && activeData.length === 0 && (
        <p className="text-sm text-base-300">No data available for this asset yet.</p>
      )}
    </div>
  );
}

export default CapTableVisualizer;
