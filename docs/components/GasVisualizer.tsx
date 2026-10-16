"use client";

import { useEffect, useId, useMemo, useState } from "react";
import { Bar, BarChart, CartesianGrid, Legend, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import {
  callsPerTransaction,
  DEFAULT_THRESHOLDS,
  fetchNetworkLimits,
  rankUtilization,
  RESOURCE_DEFINITIONS,
  SOROBAN_MAINNET_LIMITS,
  TESSERA_RESOURCE_RELEASES,
  type NetworkLimitsSnapshot,
  type ReleaseMeasurement,
  type ResourceKey,
  type ResourceUtilization,
  type Severity,
  type SeverityThresholds,
} from "@/lib/soroban-resources";

export interface GasVisualizerProps {
  /** Resource measurements per contract release, oldest first. Defaults to Tessera's metered core contracts. */
  releases?: readonly ReleaseMeasurement[];
  /** Per-transaction network limits used until (or instead of) a live read. Defaults to the Mainnet protocol 29 snapshot. */
  limits?: NetworkLimitsSnapshot;
  /** Soroban RPC endpoint to read the current limits from on mount, e.g. `https://soroban-testnet.stellar.org`. */
  rpcUrl?: string;
  /** Network name shown next to live limits read from `rpcUrl`. */
  network?: string;
  /** Utilization ratios (0–1) at which a resource is flagged as nearing its limit or critical. */
  thresholds?: SeverityThresholds;
}

type LimitsSource = { kind: "snapshot" } | { kind: "loading" } | { kind: "live"; host: string } | { kind: "failed"; message: string };

const SEVERITY_STYLES: Record<Severity, { bar: string; text: string; label: string; icon: string }> = {
  ok: { bar: "bg-brand-500", text: "text-brand-300", label: "Within budget", icon: "✓" },
  warning: { bar: "bg-gold-400", text: "text-gold-300", label: "Nearing limit", icon: "!" },
  critical: { bar: "bg-red-400", text: "text-red-300", label: "Critical", icon: "✕" },
};

/** Ordinal one-hue ramp (oldest → newest), validated against the dark chart surface. */
const RELEASE_COLORS = ["#1c5cab", "#3987e5", "#86b6ef"];

const resourceLabel = (key: ResourceKey) => RESOURCE_DEFINITIONS.find((definition) => definition.key === key)?.label ?? key;
const compact = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 2 });
const exact = new Intl.NumberFormat("en-US");

function formatPercent(fraction: number): string {
  const percent = fraction * 100;
  if (percent === 0) return "0%";
  if (percent < 0.01) return "<0.01%";
  return `${percent < 10 ? percent.toFixed(2) : percent.toFixed(1)}%`;
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function UsageMeter({ utilization, isBottleneck }: { utilization: ResourceUtilization; isBottleneck: boolean }) {
  const style = SEVERITY_STYLES[utilization.severity];
  const width = Math.min(utilization.fraction, 1) * 100;
  const label = resourceLabel(utilization.key);
  return (
    <div>
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 text-xs">
        <span className="font-medium text-base-100">
          {label}
          {isBottleneck && <span className="ml-2 rounded border border-white/15 px-1 py-px text-[0.65rem] uppercase text-base-200">Bottleneck</span>}
        </span>
        <span className="font-mono text-base-300">
          {exact.format(utilization.used)} / {exact.format(utilization.limit)} ·{" "}
          <span className={utilization.severity === "ok" ? "text-base-100" : style.text}>{formatPercent(utilization.fraction)}</span>
        </span>
      </div>
      <div
        role="meter"
        aria-label={`${label} used`}
        aria-valuemin={0}
        aria-valuemax={utilization.limit}
        aria-valuenow={Math.min(utilization.used, utilization.limit)}
        aria-valuetext={`${formatPercent(utilization.fraction)} of the per-transaction limit`}
        className="mt-1 flex h-2 overflow-hidden rounded-full bg-white/10"
      >
        {/* Used share and remaining headroom are stacked to 100% of the limit; any non-zero use stays visible. */}
        <div className={`${style.bar} rounded-full`} style={{ width: `${width}%`, minWidth: utilization.used > 0 ? 3 : 0 }} />
      </div>
      {utilization.severity !== "ok" && (
        <p className={`mt-1 text-xs ${style.text}`}>
          <span aria-hidden="true">{style.icon} </span>
          {style.label}
        </p>
      )}
    </div>
  );
}

/**
 * Soroban resource budget visualizer: meters each contract method's CPU
 * instructions, memory and ledger I/O against the network's per-transaction
 * limits, flags bottlenecks, and compares costs across contract releases.
 */
export function GasVisualizer({
  releases = TESSERA_RESOURCE_RELEASES,
  limits = SOROBAN_MAINNET_LIMITS,
  rpcUrl,
  network = "Network",
  thresholds = DEFAULT_THRESHOLDS,
}: GasVisualizerProps) {
  const titleId = useId();
  const releaseSelectId = useId();
  const resourceSelectId = useId();
  const [releaseId, setReleaseId] = useState(releases[releases.length - 1]?.id ?? "");
  const [chartResource, setChartResource] = useState<ResourceKey>("instructions");
  const [activeLimits, setActiveLimits] = useState(limits);
  const [source, setSource] = useState<LimitsSource>(rpcUrl ? { kind: "loading" } : { kind: "snapshot" });

  useEffect(() => {
    setActiveLimits(limits);
    if (!rpcUrl) {
      setSource({ kind: "snapshot" });
      return;
    }
    const controller = new AbortController();
    setSource({ kind: "loading" });
    fetchNetworkLimits(rpcUrl, network, controller.signal)
      .then((live) => {
        setActiveLimits(live);
        setSource({ kind: "live", host: hostOf(rpcUrl) });
      })
      .catch((error: Error) => {
        if (!controller.signal.aborted) setSource({ kind: "failed", message: error.message });
      });
    return () => controller.abort();
  }, [rpcUrl, network, limits]);

  const release = releases.find((candidate) => candidate.id === releaseId) ?? releases[releases.length - 1];

  // One pass per method over a fixed resource list: O(methods × resources).
  const methodRows = useMemo(
    () =>
      (release?.methods ?? []).map((measurement) => ({
        measurement,
        utilization: rankUtilization(measurement.usage, activeLimits.limits, thresholds),
        fits: callsPerTransaction(measurement.usage, activeLimits.limits),
      })),
    [release, activeLimits, thresholds]
  );

  const chartData = useMemo(() => {
    const rows = new Map<string, Record<string, number | string>>();
    for (const { id, methods } of releases) {
      for (const { contract, method, usage } of methods) {
        const name = `${contract}.${method}`;
        const row = rows.get(name) ?? { name };
        row[id] = usage[chartResource];
        rows.set(name, row);
      }
    }
    return Array.from(rows.values());
  }, [releases, chartResource]);

  const limitsDescription =
    source.kind === "live"
      ? `${activeLimits.network} · protocol ${activeLimits.protocolVersion ?? "?"} · read live from ${source.host} at ledger ${exact.format(activeLimits.ledger)}`
      : `${activeLimits.network} · protocol ${activeLimits.protocolVersion ?? "?"} · ledger ${exact.format(activeLimits.ledger)}${activeLimits.capturedAt ? ` (${activeLimits.capturedAt})` : ""}`;

  return (
    <section aria-labelledby={titleId} className="my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03]">
      <header className="border-b border-white/10 px-4 py-3">
        <h3 id={titleId} className="text-base font-bold text-base-50">
          Soroban resource budget
        </h3>
        <p className="mt-1 text-xs text-base-300">Per-transaction limits: {limitsDescription}</p>
        {source.kind === "loading" && (
          <p role="status" className="mt-1 text-xs text-base-300">
            Reading current limits from {hostOf(rpcUrl ?? "")}…
          </p>
        )}
        {source.kind === "failed" && (
          <p role="alert" className="mt-1 text-xs text-gold-300">
            Could not read live limits ({source.message}); showing the snapshot instead.
          </p>
        )}
      </header>

      <div className="space-y-6 p-4">
        {releases.length === 0 || !release ? (
          <p className="text-sm text-base-300">No resource measurements to display.</p>
        ) : (
          <>
            <div>
              <label htmlFor={releaseSelectId} className="mb-1 block text-xs font-semibold text-base-300">
                Contract release
              </label>
              <select
                id={releaseSelectId}
                value={release.id}
                onChange={(event) => setReleaseId(event.target.value)}
                className="rounded-md border border-white/10 bg-base-900 px-2.5 py-1.5 text-sm text-base-100 focus:border-brand-500/60 focus:outline-none"
              >
                {releases.map((candidate) => (
                  <option key={candidate.id} value={candidate.id}>
                    {candidate.label} ({candidate.id}, {candidate.date})
                  </option>
                ))}
              </select>
            </div>

            <ul className="grid gap-4 lg:grid-cols-2">
              {methodRows.map(({ measurement, utilization, fits }) => {
                const bottleneck = utilization[0];
                const flagged = utilization.filter((entry) => entry.severity !== "ok");
                const name = `${measurement.contract}.${measurement.method}`;
                return (
                  <li key={name} aria-label={name} className="rounded-lg border border-white/10 bg-base-900/60 p-3">
                    <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
                      <code className="font-mono text-sm text-base-50">{name}</code>
                      <span className="text-xs text-base-300">
                        {Number.isFinite(fits) ? `${exact.format(fits)}× per transaction` : "No metered cost"}
                      </span>
                    </div>
                    <p className="mb-3 text-xs text-base-200">
                      {flagged.length > 0
                        ? `${flagged.map((entry) => resourceLabel(entry.key)).join(", ")} ${flagged.length === 1 ? "is" : "are"} close to the limit.`
                        : `Bottleneck: ${resourceLabel(bottleneck.key)} at ${formatPercent(bottleneck.fraction)} of the limit.`}
                    </p>
                    <div className="space-y-2.5">
                      {utilization.map((entry) => (
                        <UsageMeter key={entry.key} utilization={entry} isBottleneck={entry === bottleneck && entry.used > 0} />
                      ))}
                    </div>
                  </li>
                );
              })}
            </ul>

            <div>
              <div className="mb-2 flex flex-wrap items-end justify-between gap-3">
                <h4 className="text-sm font-semibold text-base-50">Cost across releases</h4>
                <div>
                  <label htmlFor={resourceSelectId} className="mb-1 block text-xs font-semibold text-base-300">
                    Resource
                  </label>
                  <select
                    id={resourceSelectId}
                    value={chartResource}
                    onChange={(event) => setChartResource(event.target.value as ResourceKey)}
                    className="rounded-md border border-white/10 bg-base-900 px-2.5 py-1.5 text-sm text-base-100 focus:border-brand-500/60 focus:outline-none"
                  >
                    {RESOURCE_DEFINITIONS.map((definition) => (
                      <option key={definition.key} value={definition.key}>
                        {definition.label}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
              <div className="h-72" aria-hidden="true">
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart data={chartData} barGap={2} margin={{ top: 8, right: 8, bottom: 8, left: 8 }}>
                    <CartesianGrid vertical={false} stroke="rgba(255,255,255,0.06)" />
                    <XAxis dataKey="name" tick={{ fill: "#a5abba", fontSize: 11 }} interval={0} angle={-20} textAnchor="end" height={64} />
                    <YAxis tickFormatter={(value: number) => compact.format(value)} tick={{ fill: "#a5abba", fontSize: 11 }} width={56} />
                    <Tooltip
                      cursor={{ fill: "rgba(255,255,255,0.04)" }}
                      contentStyle={{ background: "#11141b", border: "1px solid rgba(255,255,255,0.1)", borderRadius: 8, fontSize: 12 }}
                      labelStyle={{ color: "#f6f8fb" }}
                      itemStyle={{ color: "#c7cbd4" }}
                      formatter={(value: number) => exact.format(value)}
                    />
                    <Legend
                      wrapperStyle={{ fontSize: 12 }}
                      formatter={(value: string) => <span style={{ color: "#c7cbd4" }}>{value}</span>}
                    />
                    {releases.map((candidate, index) => (
                      <Bar
                        key={candidate.id}
                        dataKey={candidate.id}
                        name={`${candidate.label} (${candidate.id})`}
                        fill={RELEASE_COLORS[Math.min(index, RELEASE_COLORS.length - 1)]}
                        radius={[4, 4, 0, 0]}
                      />
                    ))}
                  </BarChart>
                </ResponsiveContainer>
              </div>
              <details className="mt-2 text-xs">
                <summary className="cursor-pointer text-base-200">Data table</summary>
                <div className="mt-2 overflow-x-auto">
                  <table className="w-full text-left">
                    <caption className="sr-only">{resourceLabel(chartResource)} per method and release</caption>
                    <thead className="text-base-300">
                      <tr>
                        <th scope="col" className="py-1.5 pr-4 font-semibold">Method</th>
                        {releases.map((candidate) => (
                          <th key={candidate.id} scope="col" className="py-1.5 pr-4 font-semibold">
                            {candidate.id}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {chartData.map((row) => (
                        <tr key={row.name} className="border-t border-white/5">
                          <th scope="row" className="py-1.5 pr-4 font-mono font-normal text-base-100">
                            {row.name}
                          </th>
                          {releases.map((candidate) => (
                            <td key={candidate.id} className="py-1.5 pr-4 font-mono text-base-100">
                              {typeof row[candidate.id] === "number" ? exact.format(row[candidate.id] as number) : "—"}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </details>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

export default GasVisualizer;
