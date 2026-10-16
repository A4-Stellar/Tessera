"use client";

import { useCallback, useMemo, useRef, useState } from "react";
import { format as d3Format, scaleBand, scaleLinear } from "d3";

export type TrancheKind =
  | "senior-debt"
  | "mezz-debt"
  | "preferred"
  | "common"
  | "other";

export interface WaterfallTranche {
  id: string;
  label: string;
  claim: number;
  priority: number;
  kind: TrancheKind;
}

export interface WaterfallStep {
  id: string;
  label: string;
  kind: TrancheKind | "proceeds" | "residual";
  start: number;
  end: number;
  delta: number;
  paid: number;
  shortfall: number;
  priority?: number;
  isTotal: boolean;
}

export interface WaterfallResult {
  steps: WaterfallStep[];
  residual: number;
  totalPaid: number;
  totalShortfall: number;
}

export const DEFAULT_TRANCHES: WaterfallTranche[] = [
  { id: "senior", label: "Senior Secured Debt", claim: 4_000_000, priority: 1, kind: "senior-debt" },
  { id: "mezz", label: "Mezzanine Debt", claim: 1_500_000, priority: 2, kind: "mezz-debt" },
  { id: "pref", label: "Preferred Equity (1.0x liq. pref.)", claim: 2_500_000, priority: 3, kind: "preferred" },
  { id: "common", label: "Common Equity", claim: 5_000_000, priority: 4, kind: "common" },
];

export function computeWaterfall(
  exitValue: number,
  tranches: WaterfallTranche[]
): WaterfallResult {
  const proceeds = Math.max(exitValue, 0);
  const ordered = [...tranches].sort((a, b) => a.priority - b.priority);

  let remaining = proceeds;
  let totalPaid = 0;
  let totalShortfall = 0;

  const steps: WaterfallStep[] = [
    {
      id: "proceeds",
      label: "Exit Proceeds",
      kind: "proceeds",
      start: 0,
      end: proceeds,
      delta: proceeds,
      paid: proceeds,
      shortfall: 0,
      isTotal: true,
    },
  ];

  for (const tranche of ordered) {
    const claim = Math.max(tranche.claim, 0);
    const paid = Math.min(remaining, claim);
    const shortfall = claim - paid;
    const start = remaining;
    const end = remaining - paid;

    steps.push({
      id: tranche.id,
      label: tranche.label,
      kind: tranche.kind,
      start,
      end,
      delta: -paid,
      paid,
      shortfall,
      priority: tranche.priority,
      isTotal: false,
    });

    remaining = end;
    totalPaid += paid;
    totalShortfall += shortfall;
  }

  steps.push({
    id: "residual",
    label: "Residual to Common",
    kind: "residual",
    start: 0,
    end: remaining,
    delta: remaining,
    paid: remaining,
    shortfall: 0,
    isTotal: true,
  });

  return { steps, residual: remaining, totalPaid, totalShortfall };
}

const KIND_COLORS: Record<TrancheKind | "proceeds" | "residual", string> = {
  "senior-debt": "#10b981",
  "mezz-debt": "#34d399",
  preferred: "#f59e0b",
  common: "#0ea5e9",
  other: "#a5abba",
  proceeds: "#38bdf8",
  residual: "#a78bfa",
};

const SHORTFALL_COLOR = "#f43f5e";
const money = d3Format(",.0f");

function toSvgElement(node: SVGSVGElement): SVGSVGElement {
  const clone = node.cloneNode(true) as SVGSVGElement;
  clone.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  clone.setAttribute("xmlns:xlink", "http://www.w3.org/1999/xlink");
  return clone;
}

export function serializeChart(svg: SVGSVGElement): string {
  return `<?xml version="1.0" encoding="UTF-8"?>\n${new XMLSerializer().serializeToString(
    toSvgElement(svg)
  )}`;
}

export function downloadSvg(svg: SVGSVGElement, filename = "waterfall.svg"): void {
  const blob = new Blob([serializeChart(svg)], { type: "image/svg+xml;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

export function downloadPng(
  svg: SVGSVGElement,
  filename = "waterfall.png",
  scale = 2
): Promise<void> {
  return new Promise((resolve, reject) => {
    const markup = serializeChart(svg);
    const blob = new Blob([markup], { type: "image/svg+xml;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const image = new Image();

    image.onload = () => {
      const width = svg.viewBox.baseVal.width || svg.clientWidth || 800;
      const height = svg.viewBox.baseVal.height || svg.clientHeight || 420;
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(width * scale));
      canvas.height = Math.max(1, Math.round(height * scale));
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        URL.revokeObjectURL(url);
        reject(new Error("Canvas 2D context unavailable"));
        return;
      }
      ctx.fillStyle = "#0b0e14";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      ctx.drawImage(image, 0, 0, canvas.width, canvas.height);
      URL.revokeObjectURL(url);
      canvas.toBlob((out) => {
        if (!out) {
          reject(new Error("PNG encoding failed"));
          return;
        }
        const pngUrl = URL.createObjectURL(out);
        const anchor = document.createElement("a");
        anchor.href = pngUrl;
        anchor.download = filename;
        document.body.appendChild(anchor);
        anchor.click();
        anchor.remove();
        URL.revokeObjectURL(pngUrl);
        resolve();
      }, "image/png");
    };

    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("Failed to rasterize SVG"));
    };

    image.src = url;
  });
}

interface WaterfallVisualizerProps {
  initialExitValue?: number;
  initialTranches?: WaterfallTranche[];
}

export function WaterfallVisualizer({
  initialExitValue = 9_000_000,
  initialTranches = DEFAULT_TRANCHES,
}: WaterfallVisualizerProps) {
  const [exitValue, setExitValue] = useState(initialExitValue);
  const [tranches, setTranches] = useState<WaterfallTranche[]>(initialTranches);
  const [exportError, setExportError] = useState<string | null>(null);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);

  const result = useMemo(() => computeWaterfall(exitValue, tranches), [exitValue, tranches]);

  const layout = useMemo(() => {
    const width = 800;
    const height = 420;
    const margin = { top: 28, right: 24, bottom: 72, left: 84 };
    const innerWidth = width - margin.left - margin.right;
    const innerHeight = height - margin.top - margin.bottom;

    const claimCeiling = tranches.reduce(
      (max, t) => Math.max(max, Math.max(t.claim, 0)),
      exitValue
    );
    const yMax = Math.max(claimCeiling, exitValue, 1) * 1.08;

    const x = scaleBand<string>()
      .domain(result.steps.map((s) => s.id))
      .range([0, innerWidth])
      .padding(0.28);

    const y = scaleLinear().domain([0, yMax]).nice().range([innerHeight, 0]);

    const bars = result.steps.map((step) => {
      const bandX = x(step.id) ?? 0;
      const barWidth = x.bandwidth();

      if (step.isTotal) {
        return {
          step,
          x: bandX,
          width: barWidth,
          y: y(step.end),
          height: Math.max(innerHeight - y(step.end), 0),
          claimOutline: null as null | { x: number; y: number; width: number; height: number },
        };
      }

      const claimExtentTop = step.start;
      const claimExtentBottom = step.start - Math.max(step.paid + step.shortfall, 0);
      const paidBottom = step.end;

      return {
        step,
        x: bandX,
        width: barWidth,
        y: y(Math.max(step.start, step.end)),
        height: Math.max(y(Math.min(step.start, step.end)) - y(Math.max(step.start, step.end)), step.paid > 0 ? 2 : 0),
        claimOutline: {
          x: bandX,
          y: y(claimExtentTop),
          width: barWidth,
          height: Math.max(y(claimExtentBottom) - y(claimExtentTop), 0),
        },
        paidBottom,
      };
    });

    const ticks = y.ticks(6);
    const connectors = result.steps.slice(0, -1).map((step, i) => {
      const next = result.steps[i + 1];
      const level = step.isTotal ? step.end : step.end;
      const x1 = (x(step.id) ?? 0) + x.bandwidth();
      const x2 = x(next.id) ?? 0;
      return { key: `${step.id}-${next.id}`, x1, x2, y: y(level) };
    });

    return { width, height, margin, innerWidth, innerHeight, x, y, bars, ticks, connectors };
  }, [result, tranches, exitValue]);

  const updateClaim = useCallback((id: string, claim: number) => {
    setTranches((prev) =>
      prev.map((t) => (t.id === id ? { ...t, claim: Number.isFinite(claim) ? claim : 0 } : t))
    );
  }, []);

  const addTranche = useCallback(() => {
    setTranches((prev) => {
      const nextPriority = prev.reduce((max, t) => Math.max(max, t.priority), 0) + 1;
      return [
        ...prev,
        {
          id: `tranche-${Date.now()}`,
          label: `Custom Tranche ${nextPriority}`,
          claim: 500_000,
          priority: nextPriority,
          kind: "other",
        },
      ];
    });
  }, []);

  const removeTranche = useCallback((id: string) => {
    setTranches((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const handleExportSvg = useCallback(() => {
    if (!svgRef.current) return;
    setExportError(null);
    try {
      downloadSvg(svgRef.current);
      setExportStatus("SVG exported.");
    } catch (err) {
      setExportStatus(null);
      setExportError(err instanceof Error ? err.message : "SVG export failed.");
    }
  }, []);

  const handleExportPng = useCallback(() => {
    if (!svgRef.current) return;
    setExportError(null);
    setExportStatus("Rasterizing PNG…");
    downloadPng(svgRef.current)
      .then(() => setExportStatus("PNG exported."))
      .catch((err) => {
        setExportStatus(null);
        setExportError(err instanceof Error ? err.message : "PNG export failed.");
      });
  }, []);

  const chartLabel = `Waterfall chart of capital distribution across ${tranches.length} tranches. Exit proceeds ${money(exitValue)} dollars, ${money(result.totalPaid)} paid in priority order, ${money(result.totalShortfall)} shortfall, ${money(result.residual)} residual to common.`;

  const orderedForTable = [...tranches].sort((a, b) => a.priority - b.priority);

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="mb-4 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h3 className="text-base font-semibold text-base-100">
            Exit Waterfall Distribution
          </h3>
          <p className="text-xs text-base-300">
            Adjust the exit valuation and tranche claims to see seniority-ordered payouts and
            shortfall allocation.
          </p>
        </div>

        <label className="flex items-center gap-2 text-sm text-base-200">
          <span className="whitespace-nowrap">Exit proceeds ($)</span>
          <input
            type="number"
            min={0}
            step={100_000}
            value={exitValue}
            onChange={(e) => setExitValue(Number(e.target.value))}
            className="w-40 rounded-md border border-white/10 bg-black/40 px-2 py-1.5 text-right font-mono text-sm text-base-100 focus:border-brand-400 focus:outline-none"
            aria-label="Exit proceeds in dollars"
          />
        </label>
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={addTranche}
          className="rounded-md border border-white/10 px-3 py-1.5 text-sm font-medium text-base-200 transition-colors hover:text-white focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          + Add tranche
        </button>
        <button
          type="button"
          onClick={handleExportSvg}
          className="rounded-md border border-brand-500/40 bg-brand-500/15 px-3 py-1.5 text-sm font-medium text-brand-300 transition-colors hover:bg-brand-500/25 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          Export SVG
        </button>
        <button
          type="button"
          onClick={handleExportPng}
          className="rounded-md border border-gold-500/40 bg-gold-500/15 px-3 py-1.5 text-sm font-medium text-gold-300 transition-colors hover:bg-gold-500/25 focus:outline-none focus-visible:ring-2 focus-visible:ring-gold-400"
        >
          Export PNG
        </button>
        {exportStatus && (
          <span className="text-xs text-emerald-300" role="status">
            {exportStatus}
          </span>
        )}
        {exportError && (
          <span className="text-xs text-rose-300" role="alert">
            {exportError}
          </span>
        )}
      </div>

      <div className="overflow-x-auto">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${layout.width} ${layout.height}`}
          className="h-auto w-full min-w-[560px] max-w-3xl"
          role="img"
          aria-label={chartLabel}
        >
          <rect
            x={0}
            y={0}
            width={layout.width}
            height={layout.height}
            fill="#0b0e14"
            rx={8}
          />
          <g transform={`translate(${layout.margin.left},${layout.margin.top})`}>
            {layout.ticks.map((tick) => (
              <g key={tick} transform={`translate(0,${layout.y(tick)})`}>
                <line x1={0} x2={layout.innerWidth} stroke="#1e293b" strokeWidth={1} />
                <text x={-10} dy="0.32em" textAnchor="end" fontSize={11} fill="#94a3b8">
                  ${money(tick)}
                </text>
              </g>
            ))}

            {layout.connectors.map((c) => (
              <line
                key={c.key}
                x1={c.x1}
                x2={c.x2}
                y1={c.y}
                y2={c.y}
                stroke="#475569"
                strokeDasharray="3 3"
              />
            ))}

            <defs>
              <pattern
                id="shortfall-hatch"
                width={6}
                height={6}
                patternUnits="userSpaceOnUse"
                patternTransform="rotate(45)"
              >
                <rect width={6} height={6} fill={`${SHORTFALL_COLOR}22`} />
                <line x1={0} y1={0} x2={0} y2={6} stroke={SHORTFALL_COLOR} strokeWidth={2} />
              </pattern>
            </defs>

            {layout.bars.map(({ step, x, width, y, height, claimOutline }) => {
              const fill = KIND_COLORS[step.kind] ?? KIND_COLORS.other;
              return (
                <g key={step.id}>
                  {claimOutline && step.shortfall > 0 && (
                    <rect
                      x={claimOutline.x}
                      y={claimOutline.y}
                      width={claimOutline.width}
                      height={claimOutline.height}
                      fill="url(#shortfall-hatch)"
                      stroke={SHORTFALL_COLOR}
                      strokeWidth={1}
                      strokeDasharray="4 2"
                    >
                      <title>{`${step.label}: $${money(step.shortfall)} shortfall (unpaid)`}</title>
                    </rect>
                  )}
                  <rect
                    x={x}
                    y={y}
                    width={width}
                    height={height}
                    fill={fill}
                    opacity={step.isTotal ? 0.95 : 0.85}
                    rx={2}
                  >
                    <title>
                      {step.isTotal
                        ? `${step.label}: $${money(step.end)}`
                        : `${step.label}: paid $${money(step.paid)} of $${money(
                            step.paid + step.shortfall
                          )}${step.shortfall > 0 ? `, shortfall $${money(step.shortfall)}` : ""}`}
                    </title>
                  </rect>

                  <text
                    x={x + width / 2}
                    y={Math.max(y - 6, 12)}
                    textAnchor="middle"
                    fontSize={10}
                    fill={step.shortfall > 0 ? SHORTFALL_COLOR : "#e2e8f0"}
                  >
                    {step.isTotal
                      ? `$${money(step.end)}`
                      : step.shortfall > 0
                        ? `$${money(step.shortfall)} short`
                        : `$${money(step.paid)}`}
                  </text>

                  {step.priority !== undefined && (
                    <circle cx={x + 10} cy={y + 12} r={8} fill="#0b0e14" stroke={fill} />
                  )}
                  {step.priority !== undefined && (
                    <text
                      x={x + 10}
                      y={y + 12}
                      textAnchor="middle"
                      dy="0.34em"
                      fontSize={9}
                      fontWeight={700}
                      fill={fill}
                    >
                      {step.priority}
                    </text>
                  )}

                  <g transform={`translate(${x + width / 2},${layout.innerHeight + 12})`}>
                    <text fontSize={10} fill="#94a3b8" textAnchor="end" transform="rotate(-35)">
                      {step.label.length > 22 ? `${step.label.slice(0, 21)}…` : step.label}
                    </text>
                  </g>
                </g>
              );
            })}

            <line
              x1={0}
              x2={layout.innerWidth}
              y1={layout.innerHeight}
              y2={layout.innerHeight}
              stroke="#334155"
            />
          </g>
        </svg>
      </div>

      <div className="mt-4 flex flex-wrap gap-3 text-xs text-base-300">
        {(Object.keys(KIND_COLORS) as Array<keyof typeof KIND_COLORS>).map((key) => (
          <span key={key} className="inline-flex items-center gap-1.5">
            <span
              className="inline-block h-2.5 w-2.5 rounded-sm"
              style={{ backgroundColor: KIND_COLORS[key] }}
              aria-hidden="true"
            />
            <span className="capitalize">{key.replace(/-/g, " ")}</span>
          </span>
        ))}
        <span className="inline-flex items-center gap-1.5">
          <span
            className="inline-block h-2.5 w-2.5 rounded-sm border border-rose-500"
            style={{ backgroundColor: `${SHORTFALL_COLOR}44` }}
            aria-hidden="true"
          />
          <span>Shortfall</span>
        </span>
      </div>

      <div className="mt-4 grid gap-4 md:grid-cols-2">
        <div>
          <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-base-300">
            Tranche inputs (priority order)
          </h4>
          <ol className="space-y-2">
            {orderedForTable.map((tranche, index) => (
              <li
                key={tranche.id}
                className="flex flex-wrap items-center gap-2 rounded-lg border border-white/5 bg-white/[0.02] px-3 py-2 text-sm"
              >
                <span
                  className="flex h-5 w-5 items-center justify-center rounded-full text-[10px] font-bold"
                  style={{
                    color: KIND_COLORS[tranche.kind],
                    border: `1px solid ${KIND_COLORS[tranche.kind]}`,
                  }}
                  aria-label={`Priority ${index + 1}`}
                >
                  {index + 1}
                </span>
                <span className="flex-1 truncate text-base-200">{tranche.label}</span>
                <label className="flex items-center gap-1 text-xs text-base-300">
                  <span className="sr-only">Claim for {tranche.label}</span>
                  $
                  <input
                    type="number"
                    min={0}
                    step={50_000}
                    value={tranche.claim}
                    onChange={(e) => updateClaim(tranche.id, Number(e.target.value))}
                    className="w-28 rounded-md border border-white/10 bg-black/40 px-2 py-1 text-right font-mono text-xs text-base-100 focus:border-brand-400 focus:outline-none"
                  />
                </label>
                <button
                  type="button"
                  onClick={() => removeTranche(tranche.id)}
                  aria-label={`Remove ${tranche.label}`}
                  className="rounded-md border border-white/10 px-2 py-1 text-xs text-base-300 transition-colors hover:text-rose-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-rose-400"
                >
                  ✕
                </button>
              </li>
            ))}
          </ol>
          {orderedForTable.length === 0 && (
            <p className="text-sm text-base-300">No tranches configured.</p>
          )}
        </div>

        <div>
          <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-base-300">
            Distribution summary
          </h4>
          <table className="w-full text-sm">
            <caption className="sr-only">Waterfall distribution summary by tranche</caption>
            <thead>
              <tr className="border-b border-white/10">
                <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
                  Tranche
                </th>
                <th scope="col" className="px-2 py-1.5 text-right font-semibold text-base-100">
                  Paid
                </th>
                <th scope="col" className="px-2 py-1.5 text-right font-semibold text-base-100">
                  Shortfall
                </th>
              </tr>
            </thead>
            <tbody>
              {result.steps
                .filter((s) => !s.isTotal)
                .map((step) => (
                  <tr key={step.id} className="border-b border-white/5">
                    <td className="px-2 py-1.5 text-base-200">{step.label}</td>
                    <td className="px-2 py-1.5 text-right font-mono text-base-300">
                      ${money(step.paid)}
                    </td>
                    <td
                      className={`px-2 py-1.5 text-right font-mono ${
                        step.shortfall > 0 ? "text-rose-300" : "text-base-300"
                      }`}
                    >
                      {step.shortfall > 0 ? `$${money(step.shortfall)}` : "—"}
                    </td>
                  </tr>
                ))}
              <tr className="border-t border-white/10">
                <td className="px-2 py-1.5 font-semibold text-base-100">Residual to common</td>
                <td className="px-2 py-1.5 text-right font-mono text-base-300">
                  ${money(result.residual)}
                </td>
                <td className="px-2 py-1.5 text-right text-base-300">—</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

export default WaterfallVisualizer;
