"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { hierarchy, treemap, type TreemapLayout, type HierarchyRectangularNode } from "d3";
import { useWebSocket } from "@/hooks/useWebSocket";
import { API_BASE_URL } from "@/lib/api";

export interface HeatmapHolder {
  address: string;
  /** Token balance as a positive number (whole tokens or stroops — display only). */
  balance: number;
  /** ISO country/region code or free-form jurisdiction label. */
  region?: string;
  /** Optional compliance classification for color-coding. */
  compliance?: "approved" | "pending" | "suspended" | "rejected";
}

export interface TransferEvent {
  from?: string;
  to?: string;
  amount?: number | string;
  [key: string]: unknown;
}

export type ColorMode = "region" | "compliance";

interface HolderNode {
  name: string;
  group: string;
  holder: HeatmapHolder;
  value: number;
}

interface GroupNode {
  name: string;
  children: HolderNode[];
}

export const REGION_COLORS: Record<string, string> = {
  "North America": "#38bdf8",
  "Europe/EEA": "#34d399",
  "Asia-Pacific": "#f59e0b",
  "Middle East": "#a78bfa",
  "Latin America": "#f472b6",
  Africa: "#2dd4bf",
  Unknown: "#64748b",
};

export const COMPLIANCE_COLORS: NonNullable<HeatmapHolder["compliance"]>[] = [
  "approved",
  "pending",
  "suspended",
  "rejected",
];

const COMPLIANCE_FILL: Record<string, string> = {
  approved: "#10b981",
  pending: "#f59e0b",
  suspended: "#a78bfa",
  rejected: "#f43f5e",
};

const FALLBACK_FILLS = [
  "#38bdf8",
  "#34d399",
  "#f59e0b",
  "#a78bfa",
  "#f472b6",
  "#2dd4bf",
  "#818cf8",
  "#64748b",
];

export const DEFAULT_HEATMAP_HOLDERS: HeatmapHolder[] = [
  { address: "GALCB4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAMPLE", balance: 420_000, region: "North America", compliance: "approved" },
  { address: "GBQT4Z5K7TQ6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAMP", balance: 260_000, region: "Europe/EEA", compliance: "approved" },
  { address: "GCLK9Z5K7TQ6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAM", balance: 180_000, region: "Asia-Pacific", compliance: "pending" },
  { address: "GDMN4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXAM", balance: 95_000, region: "Latin America", compliance: "approved" },
  { address: "GEPR9Z5K7TQ6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXA", balance: 62_000, region: "Middle East", compliance: "suspended" },
  { address: "GFSA4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EXA", balance: 41_000, region: "Africa", compliance: "pending" },
  { address: "GGTY9Z5K7TQ6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EX", balance: 28_000, region: "North America", compliance: "rejected" },
  { address: "GHNU4XQM7Q6RZ3B2K5Z3QWPJ7X2YJ6VQm3nD2S4EX", balance: 14_000, region: undefined, compliance: "approved" },
];

function truncateAddress(address: string): string {
  if (address.length <= 14) return address;
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

function groupKey(holder: HeatmapHolder, mode: ColorMode): string {
  if (mode === "compliance") return holder.compliance ?? "approved";
  return holder.region?.trim() ? holder.region : "Unknown";
}

function fillFor(group: string, mode: ColorMode, index: number): string {
  if (mode === "compliance") {
    return COMPLIANCE_FILL[group] ?? FALLBACK_FILLS[index % FALLBACK_FILLS.length];
  }
  return REGION_COLORS[group] ?? FALLBACK_FILLS[index % FALLBACK_FILLS.length];
}

/**
 * Pure reducer applying a transfer event to a holder set. Transfers mutate
 * balances of known addresses; unknown recipients are appended so the map
 * reacts to newly-arriving holders without a full refetch.
 */
export function applyTransfer(
  holders: HeatmapHolder[],
  event: TransferEvent
): HeatmapHolder[] {
  const from = typeof event.from === "string" ? event.from : null;
  const to = typeof event.to === "string" ? event.to : null;
  const rawAmount = event.amount ?? event.value ?? 0;
  const amount = typeof rawAmount === "number" ? rawAmount : Number(rawAmount);
  if (!Number.isFinite(amount) || amount <= 0) return holders;

  const next = holders.map((h) => ({ ...h }));
  const byAddress = new Map(next.map((h) => [h.address, h] as const));

  if (from) {
    const sender = byAddress.get(from);
    if (sender) {
      sender.balance = Math.max(0, sender.balance - amount);
    }
  }
  if (to) {
    const recipient = byAddress.get(to);
    if (recipient) {
      recipient.balance += amount;
    } else {
      const created: HeatmapHolder = { address: to, balance: amount };
      next.push(created);
      byAddress.set(to, created);
    }
  }

  return next;
}

function getWsUrl(baseUrl: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  const httpBase = trimmed.endsWith("/v1") ? trimmed.slice(0, -3) : trimmed;
  const wsBase = httpBase.replace(/^http/, "ws");
  return `${wsBase}/v1/ws`;
}

/**
 * Child bridge so the WebSocket hook only mounts (and connects) when live
 * updates are requested.
 */
function LiveTransferBridge({
  url,
  onTransfer,
}: {
  url: string;
  onTransfer: (event: TransferEvent) => void;
}) {
  const handleMessage = useCallback(
    (raw: unknown) => {
      if (typeof raw !== "object" || raw === null) return;
      const event = raw as { event_type?: string; data?: Record<string, unknown> };
      const type = (event.event_type ?? "").toLowerCase();
      if (!type.includes("transfer") && !type.includes("payment")) return;
      onTransfer((event.data ?? {}) as TransferEvent);
    },
    [onTransfer]
  );

  useWebSocket({ url, autoReconnect: true, onMessage: handleMessage });
  return null;
}

interface CapTableHeatmapProps {
  /** Initial holder snapshot. Defaults to a demo cap table. */
  holders?: HeatmapHolder[];
  /** Color nodes by geographic region or compliance status. */
  colorBy?: ColorMode;
  /** Overrides NEXT_PUBLIC_API_BASE_URL for live transfer feeds. */
  apiBaseUrl?: string;
  /** Subscribe to live transfer events over WebSocket. */
  live?: boolean;
  /** Chart height in pixels. */
  height?: number;
}

export function CapTableHeatmap({
  holders: holdersProp,
  colorBy = "region",
  apiBaseUrl = API_BASE_URL,
  live = false,
  height = 420,
}: CapTableHeatmapProps) {
  const [holders, setHolders] = useState<HeatmapHolder[]>(
    holdersProp ?? DEFAULT_HEATMAP_HOLDERS
  );
  const [hovered, setHovered] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(720);

  useEffect(() => {
    if (holdersProp) setHolders(holdersProp);
  }, [holdersProp]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next && next > 0) setWidth(Math.round(next));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const wsUrl = useMemo(() => getWsUrl(apiBaseUrl), [apiBaseUrl]);

  const handleTransfer = useCallback((event: TransferEvent) => {
    setHolders((prev) => applyTransfer(prev, event));
  }, []);

  const groups = useMemo<GroupNode[]>(() => {
    const bucketed = new Map<string, HolderNode[]>();
    for (const holder of holders) {
      if (holder.balance <= 0) continue;
      const key = groupKey(holder, colorBy);
      const list = bucketed.get(key) ?? [];
      list.push({ name: holder.address, group: key, holder, value: holder.balance });
      bucketed.set(key, list);
    }
    return [...bucketed.entries()].map(([name, children]) => ({ name, children }));
  }, [holders, colorBy]);

  const totalBalance = useMemo(
    () => holders.reduce((sum, h) => sum + Math.max(h.balance, 0), 0),
    [holders]
  );

  const layout = useMemo(() => {
    const w = Math.max(width, 320);
    const innerHeight = Math.max(height, 240);

    type HeatmapDatum = {
      name: string;
      value?: number;
      holder?: HeatmapHolder;
      group?: string;
      children?: HeatmapDatum[];
    };

    const data: HeatmapDatum = {
      name: "root",
      children: groups as unknown as HeatmapDatum[],
    };

    const root = hierarchy<HeatmapDatum>(data, (node) => node.children)
      .sum((node) => node.value ?? 0)
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));

    const layoutFn: TreemapLayout<HeatmapDatum> = treemap<HeatmapDatum>()
      .size([w, innerHeight])
      .paddingOuter(3)
      .paddingTop(20)
      .paddingInner(2)
      .round(true);

    layoutFn(root);
    return root
      .leaves()
      .map((leaf) => ({
        rect: leaf as HierarchyRectangularNode<HeatmapDatum>,
        holder: leaf.data.holder as HeatmapHolder,
        group: leaf.data.group ?? "Unknown",
        value: leaf.value ?? 0,
      }));
  }, [groups, width, height]);

  const legendEntries = useMemo(() => {
    return groups.map((g, i) => ({
      name: g.name,
      fill: fillFor(g.name, colorBy, i),
      value: g.children.reduce((sum, c) => sum + c.value, 0),
    }));
  }, [groups, colorBy]);

  const heatmapLabel = `Cap table concentration heatmap across ${holders.length} holders grouped by ${
    colorBy === "region" ? "geographic region" : "compliance status"
  }.`;

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      {live && <LiveTransferBridge url={wsUrl} onTransfer={handleTransfer} />}
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-base font-semibold text-base-100">
            Cap-Table Concentration Heatmap
          </h3>
          <p className="text-xs text-base-300">
            Box area is proportional to token balance share
            {live ? "; live transfer events update the layout in real time." : "."}
          </p>
        </div>
        {live && (
          <span className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2.5 py-1 text-[11px] font-semibold uppercase tracking-wide text-emerald-300">
            ● Live
          </span>
        )}
      </div>

      <div
        ref={containerRef}
        role="img"
        aria-label={heatmapLabel}
        className="w-full"
        style={{ minHeight: height }}
      >
        <svg
          viewBox={`0 0 ${Math.max(width, 320)} ${Math.max(height, 240)}`}
          className="h-auto w-full"
        >
          <rect
            x={0}
            y={0}
            width={Math.max(width, 320)}
            height={Math.max(height, 240)}
            fill="#0b0e14"
            rx={8}
          />
          {layout.map(({ rect, holder, group, value }, i) => {
            const fill = fillFor(group, colorBy, i);
            const nodeWidth = rect.x1 - rect.x0;
            const nodeHeight = rect.y1 - rect.y0;
            const share = totalBalance > 0 ? (value / totalBalance) * 100 : 0;
            const isHovered = hovered === holder.address;
            const showLabel = nodeWidth > 64 && nodeHeight > 28;

            return (
              <g
                key={`${holder.address}-${i}`}
                onMouseEnter={() => setHovered(holder.address)}
                onMouseLeave={() => setHovered((prev) => (prev === holder.address ? null : prev))}
                style={{ cursor: "default" }}
              >
                <title>
                  {`${truncateAddress(holder.address)} — ${holder.balance.toLocaleString()} tokens (${share.toFixed(
                    2
                  )}%)${holder.region ? ` · ${holder.region}` : ""}${
                    holder.compliance ? ` · ${holder.compliance}` : ""
                  }`}
                </title>
                <rect
                  x={rect.x0}
                  y={rect.y0}
                  width={nodeWidth}
                  height={nodeHeight}
                  fill={fill}
                  fillOpacity={isHovered ? 0.9 : 0.55}
                  stroke={isHovered ? "#f8fafc" : fill}
                  strokeWidth={isHovered ? 2 : 1}
                  rx={3}
                  style={{ transition: "fill-opacity 200ms ease" }}
                />
                {showLabel && (
                  <>
                    <text
                      x={rect.x0 + 6}
                      y={rect.y0 + 16}
                      fontSize={11}
                      fontWeight={600}
                      fill="#f8fafc"
                    >
                      {truncateAddress(holder.address)}
                    </text>
                    <text x={rect.x0 + 6} y={rect.y0 + 30} fontSize={10} fill="#e2e8f0">
                      {share.toFixed(1)}%
                    </text>
                    {nodeHeight > 56 && (
                      <text x={rect.x0 + 6} y={rect.y0 + 44} fontSize={9} fill="#cbd5e1">
                        {value.toLocaleString()}
                      </text>
                    )}
                  </>
                )}
              </g>
            );
          })}
        </svg>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-3 text-xs text-base-300">
        <span className="font-semibold text-base-200">
          {colorBy === "region" ? "Regions:" : "Compliance:"}
        </span>
        {legendEntries.map((entry) => (
          <span key={entry.name} className="inline-flex items-center gap-1.5">
            <span
              className="inline-block h-2.5 w-2.5 rounded-sm"
              style={{ backgroundColor: entry.fill }}
              aria-hidden="true"
            />
            <span>
              {entry.name} ({((entry.value / Math.max(totalBalance, 1)) * 100).toFixed(1)}%)
            </span>
          </span>
        ))}
      </div>

      <table className="mt-4 w-full text-sm">
        <caption className="mb-2 text-left text-xs font-semibold text-base-300">
          Holder concentration (data table)
        </caption>
        <thead>
          <tr className="border-b border-white/10">
            <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
              Holder
            </th>
            <th scope="col" className="px-2 py-1.5 text-left font-semibold text-base-100">
              {colorBy === "region" ? "Region" : "Compliance"}
            </th>
            <th scope="col" className="px-2 py-1.5 text-right font-semibold text-base-100">
              Balance
            </th>
            <th scope="col" className="px-2 py-1.5 text-right font-semibold text-base-100">
              Share
            </th>
          </tr>
        </thead>
        <tbody>
          {[...holders]
            .sort((a, b) => b.balance - a.balance)
            .map((holder) => {
              const share = totalBalance > 0 ? (holder.balance / totalBalance) * 100 : 0;
              return (
                <tr key={holder.address} className="border-b border-white/5">
                  <td className="px-2 py-1.5 font-mono text-base-200">
                    {truncateAddress(holder.address)}
                  </td>
                  <td className="px-2 py-1.5 text-base-300">
                    {groupKey(holder, colorBy)}
                  </td>
                  <td className="px-2 py-1.5 text-right font-mono text-base-300">
                    {holder.balance.toLocaleString()}
                  </td>
                  <td className="px-2 py-1.5 text-right text-base-300">
                    {share.toFixed(2)}%
                  </td>
                </tr>
              );
            })}
        </tbody>
      </table>
    </div>
  );
}

export default CapTableHeatmap;
