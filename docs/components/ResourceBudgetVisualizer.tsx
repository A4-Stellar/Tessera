"use client";

import { useId, useMemo } from "react";
import {
  DEFAULT_THRESHOLDS,
  RESOURCE_DEFINITIONS,
  SOROBAN_MAINNET_LIMITS,
  TESSERA_RESOURCE_RELEASES,
  rankUtilization,
  type NetworkLimitsSnapshot,
  type ResourceKey,
  type ResourceUsage,
  type Severity,
  type SeverityThresholds,
} from "@/lib/soroban-resources";
import {
  analyzeWasmInstructionBudget,
  type BudgetCategoryKey,
  type SorobanBudgetReport,
} from "@/lib/wasm-inspector";

export interface ResourceBudgetVisualizerProps {
  /** Metered resources to compare with the network limits. Defaults to the latest Tessera release's worst-case method. */
  usage?: ResourceUsage;
  /** Per-transaction network limits. Defaults to the Mainnet protocol 29 snapshot. */
  limits?: NetworkLimitsSnapshot;
  /** Utilization ratios at which a resource is flagged as nearing or exceeding its recommended limit. */
  thresholds?: SeverityThresholds;
  /** Optional compiled `.wasm` binary containing a `soroban_budget` custom section (`soroban build --debug`) to decode and visualize. */
  wasmBytes?: Uint8Array;
  /** Optional pre-parsed `soroban_budget` instruction category report to render in the stacked category chart. */
  budgetReport?: SorobanBudgetReport;
}

const SEVERITY_STYLES: Record<Severity, { bar: string; text: string; label: string }> = {
  ok: { bar: "bg-brand-500", text: "text-brand-300", label: "Within budget" },
  warning: { bar: "bg-gold-400", text: "text-gold-300", label: "Nearing limit" },
  critical: { bar: "bg-red-400", text: "text-red-300", label: "Critical" },
};

const CATEGORY_COLORS: Record<BudgetCategoryKey, string> = {
  compute: "bg-brand-500",
  memory: "bg-sky-400",
  storage: "bg-gold-400",
  host_calls: "bg-violet-400",
  crypto: "bg-emerald-400",
};

const RESOURCE_GROUPS: { label: string; keys: ResourceKey[] }[] = [
  { label: "Compute", keys: ["instructions"] },
  { label: "Memory", keys: ["memBytes"] },
  {
    label: "Ledger and storage",
    keys: ["diskReadEntries", "diskReadBytes", "writeEntries", "writeBytes", "eventsBytes"],
  },
];

const OPTIMIZATION_TIPS: Partial<Record<ResourceKey, string>> = {
  instructions: "Reduce loops and repeated host calls; batch independent work where possible.",
  memBytes: "Stream large values and avoid keeping duplicate collections in memory.",
  diskReadEntries: "Read only the ledger entries needed for this invocation.",
  diskReadBytes: "Use smaller, targeted reads instead of loading large values.",
  writeEntries: "Batch related state changes into fewer ledger entries.",
  writeBytes: "Store compact values and avoid rewriting unchanged state.",
  eventsBytes: "Emit concise events and omit data that clients can derive.",
};

const exact = new Intl.NumberFormat("en-US");

function formatPercent(fraction: number): string {
  const percent = fraction * 100;
  if (percent === 0) return "0%";
  if (percent < 0.01) return "<0.01%";
  return `${percent < 10 ? percent.toFixed(2) : percent.toFixed(1)}%`;
}

function defaultUsage(): ResourceUsage {
  const methods = TESSERA_RESOURCE_RELEASES.at(-1)?.methods ?? [];
  return RESOURCE_DEFINITIONS.reduce(
    (result, { key }) => ({
      ...result,
      [key]: Math.max(0, ...methods.map(({ usage }) => usage[key])),
    }),
    {} as ResourceUsage
  );
}

function resourceLabel(key: ResourceKey): string {
  return RESOURCE_DEFINITIONS.find((definition) => definition.key === key)?.label ?? key;
}

function BudgetBar({ fraction, severity, label }: { fraction: number; severity: Severity; label: string }) {
  const width = Math.min(fraction, 1) * 100;
  const style = SEVERITY_STYLES[severity];
  return (
    <div
      role="meter"
      aria-label={`${label} used`}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.min(fraction, 1) * 100}
      aria-valuetext={`${formatPercent(fraction)} of the per-transaction limit`}
      className="mt-1 flex h-2 overflow-hidden rounded-full bg-white/10"
    >
      <div className={`${style.bar} h-full`} style={{ width: `${width}%`, minWidth: fraction > 0 ? 3 : 0 }} />
      <div className="h-full flex-1 bg-white/5" />
    </div>
  );
}

/**
 * Soroban resource budget visualizer: renders stacked progress bars for CPU,
 * memory, and ledger/storage resources, highlights bottlenecks, suggests
 * optimizations when a resource reaches its recommended threshold, and parses
 * the `soroban_budget` WASM custom section to render a stacked bar chart of
 * instruction counts by category vs. Soroban limits.
 */
export function ResourceBudgetVisualizer({
  usage,
  limits = SOROBAN_MAINNET_LIMITS,
  thresholds = DEFAULT_THRESHOLDS,
  wasmBytes,
  budgetReport,
}: ResourceBudgetVisualizerProps) {
  const titleId = useId();

  const resolvedBudgetReport = useMemo<SorobanBudgetReport | null>(() => {
    if (budgetReport) return budgetReport;
    if (!wasmBytes) return null;
    try {
      return analyzeWasmInstructionBudget(wasmBytes);
    } catch {
      return null;
    }
  }, [wasmBytes, budgetReport]);

  const effectiveUsage = useMemo<ResourceUsage>(() => {
    if (usage) return usage;
    const base = defaultUsage();
    if (!resolvedBudgetReport) return base;
    return {
      ...base,
      instructions: resolvedBudgetReport.totalInstructions,
      memBytes: resolvedBudgetReport.memBytes > 0 ? resolvedBudgetReport.memBytes : base.memBytes,
    };
  }, [usage, resolvedBudgetReport]);

  const utilization = useMemo(
    () => rankUtilization(effectiveUsage, limits.limits, thresholds),
    [effectiveUsage, limits, thresholds]
  );
  const byKey = new Map(utilization.map((entry) => [entry.key, entry]));
  const bottleneck = utilization.find((entry) => entry.used > 0);
  const overThreshold = utilization.filter((entry) => entry.severity !== "ok");

  return (
    <section aria-labelledby={titleId} className="my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03]">
      <header className="border-b border-white/10 px-4 py-3">
        <h3 id={titleId} className="text-base font-bold text-base-50">
          Resource budget
        </h3>
        <p className="mt-1 text-xs text-base-300">
          {limits.network} · protocol {limits.protocolVersion ?? "?"} · per-transaction limits at ledger {exact.format(limits.ledger)}
        </p>
        {bottleneck && (
          <p className="mt-2 text-xs text-base-200">
            Bottleneck: <strong className="text-base-50">{resourceLabel(bottleneck.key)}</strong> at {formatPercent(bottleneck.fraction)} of its limit.
          </p>
        )}
      </header>

      <div className="space-y-5 p-4">
        {resolvedBudgetReport && (
          <div
            role="region"
            aria-label="Instruction counts by category vs. Soroban budget"
            className="rounded-lg border border-white/10 bg-base-900/60 p-3 space-y-3"
          >
            <div className="flex flex-wrap items-baseline justify-between gap-2 text-xs">
              <div>
                <h4 className="text-sm font-semibold text-base-50">
                  Instruction breakdown by category
                </h4>
                <p className="text-[11px] text-base-300">
                  Source:{" "}
                  <code>
                    {resolvedBudgetReport.source === "soroban_budget"
                      ? "soroban_budget custom section"
                      : "WASM static instruction profile"}
                  </code>
                </p>
              </div>
              <span className="font-mono text-base-200">
                {exact.format(resolvedBudgetReport.totalInstructions)} / {exact.format(limits.limits.instructions)} instructions (
                {formatPercent(
                  limits.limits.instructions > 0
                    ? resolvedBudgetReport.totalInstructions / limits.limits.instructions
                    : 0
                )}
                )
              </span>
            </div>

            {/* Stacked Category Bar Chart */}
            <div
              role="img"
              aria-label={`Stacked instruction category bar chart: ${exact.format(resolvedBudgetReport.totalInstructions)} of ${exact.format(limits.limits.instructions)} instructions`}
              className="flex h-3.5 w-full overflow-hidden rounded-full bg-white/10"
            >
              {resolvedBudgetReport.categories.map((cat) => {
                if (cat.instructions <= 0) return null;
                const widthPct = Math.min(100, Math.max(1, cat.fractionOfTotal * 100));
                return (
                  <div
                    key={cat.category}
                    title={`${cat.label}: ${exact.format(cat.instructions)} instructions (${formatPercent(cat.fractionOfLimit)} of limit)`}
                    className={`${CATEGORY_COLORS[cat.category]} h-full border-r border-base-950/40 last:border-r-0`}
                    style={{ width: `${widthPct}%` }}
                  />
                );
              })}
            </div>

            {/* Category Legend & Metrics */}
            <div className="grid gap-2 text-xs sm:grid-cols-2 lg:grid-cols-5">
              {resolvedBudgetReport.categories.map((cat) => (
                <div key={cat.category} className="rounded border border-white/5 bg-base-950/60 px-2.5 py-2">
                  <div className="flex items-center gap-1.5">
                    <span className={`inline-block h-2 w-2 rounded-full ${CATEGORY_COLORS[cat.category]}`} aria-hidden="true" />
                    <span className="font-medium text-base-100">{cat.label}</span>
                  </div>
                  <p className="mt-1 font-mono text-xs text-base-50">
                    {exact.format(cat.instructions)} instrs
                  </p>
                  <p className="text-[11px] font-mono text-base-300">
                    {formatPercent(cat.fractionOfTotal)} share · {formatPercent(cat.fractionOfLimit)} of limit
                  </p>
                </div>
              ))}
            </div>
          </div>
        )}

        {RESOURCE_GROUPS.map((group) => (
          <div key={group.label}>
            <h4 className="mb-2 text-sm font-semibold text-base-50">{group.label}</h4>
            <div className="space-y-3">
              {group.keys.map((key) => {
                const entry = byKey.get(key)!;
                const style = SEVERITY_STYLES[entry.severity];
                return (
                  <div key={key}>
                    <div className="flex flex-wrap items-baseline justify-between gap-x-3 text-xs">
                      <span className="font-medium text-base-100">{resourceLabel(key)}</span>
                      <span className="font-mono text-base-300">
                        {exact.format(entry.used)} / {exact.format(entry.limit)} ·{" "}
                        <span className={entry.severity === "ok" ? "text-base-100" : style.text}>{formatPercent(entry.fraction)}</span>
                      </span>
                    </div>
                    <BudgetBar fraction={entry.fraction} severity={entry.severity} label={resourceLabel(key)} />
                    {entry.severity !== "ok" && (
                      <p className={`mt-1 text-xs ${style.text}`}>
                        <span aria-hidden="true">{entry.severity === "critical" ? "✕" : "!"} </span>
                        {style.label}
                      </p>
                    )}
                    {entry.severity !== "ok" && (
                      <p className="mt-1 text-xs text-base-300">
                        Tip: {OPTIMIZATION_TIPS[key]}
                      </p>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        ))}
        {overThreshold.length === 0 && <p className="text-xs text-base-300">All resources are below the recommended thresholds.</p>}
      </div>
    </section>
  );
}

/** Alias for `ResourceBudgetVisualizer` using British English spelling (`ResourceBudgetVisualiser`). */
export const ResourceBudgetVisualiser = ResourceBudgetVisualizer;

export default ResourceBudgetVisualizer;
