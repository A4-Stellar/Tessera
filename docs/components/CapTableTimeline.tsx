"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Cell, Legend, Pie, PieChart, ResponsiveContainer, Tooltip } from "recharts";

export interface TimelineHolder {
  address: string;
  balance: string;
  share_percent: number;
}

export interface CapTableSnapshot {
  /** Ledger height this snapshot was taken at. */
  ledger: number;
  /** ISO timestamp of the snapshot (optional). */
  timestamp?: string;
  holders: TimelineHolder[];
  /** Optional label, e.g. "Seed round closed". */
  note?: string;
}

interface CapTableTimelineProps {
  /** Ordered cap-table snapshots from asset inception to current ledger height. */
  snapshots: CapTableSnapshot[];
  /** Registry id of the asset (display only). */
  assetId?: string | number;
  /** Snapshot index to show first. Defaults to the latest snapshot. */
  initialIndex?: number;
  /** Number of holders to break out before grouping the rest into "Other". */
  topHolderCount?: number;
  /** Start playing on mount. Defaults to false. */
  autoPlay?: boolean;
}

type Speed = 1 | 2 | 5 | 10;

const SPEEDS: Speed[] = [1, 2, 5, 10];
/** Base frame duration at 1x; higher speeds divide it. */
const BASE_FRAME_MS = 1200;

/** Colors drawn from the docs' Tailwind palette (matches CapTableVisualizer). */
const SLICE_COLORS = ["#10b981", "#34d399", "#f59e0b", "#0ea5e9", "#a5abba", "#ef4444", "#6ee7b7"];

function truncateAddress(address: string): string {
  return address.length > 12 ? `${address.slice(0, 6)}…${address.slice(-4)}` : address;
}

/**
 * Interactive historical timeline player for an asset's cap table.
 * Scrub from inception to the current ledger height while the holder
 * distribution pie chart and top-holder rankings update with smooth
 * transitions. Playback speed is adjustable (1x, 2x, 5x, 10x).
 */
export function CapTableTimeline({
  snapshots,
  assetId,
  initialIndex,
  topHolderCount = 6,
  autoPlay = false,
}: CapTableTimelineProps) {
  const lastIndex = snapshots.length - 1;
  const [index, setIndex] = useState(() =>
    initialIndex !== undefined ? Math.min(Math.max(initialIndex, 0), Math.max(lastIndex, 0)) : Math.max(lastIndex, 0),
  );
  const [playing, setPlaying] = useState(autoPlay && snapshots.length > 1);
  const [speed, setSpeed] = useState<Speed>(1);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);

  // Keep the index valid if the snapshot list changes.
  useEffect(() => {
    setIndex((prev) => Math.min(prev, Math.max(lastIndex, 0)));
  }, [lastIndex]);

  useEffect(() => {
    if (!playing) return;
    if (index >= lastIndex) {
      setPlaying(false);
      return;
    }
    timer.current = setInterval(() => {
      setIndex((prev) => {
        if (prev >= lastIndex) {
          setPlaying(false);
          return prev;
        }
        return prev + 1;
      });
    }, BASE_FRAME_MS / speed);
    return () => {
      if (timer.current) clearInterval(timer.current);
    };
  }, [playing, speed, index, lastIndex]);

  const snapshot = snapshots[index];

  const distributionData = useMemo(() => {
    if (!snapshot) return [];
    const sorted = [...snapshot.holders].sort((a, b) => b.share_percent - a.share_percent);
    const top = sorted.slice(0, topHolderCount);
    const rest = sorted.slice(topHolderCount);
    const restShare = rest.reduce((sum, h) => sum + h.share_percent, 0);
    const rows = top.map((h) => ({ name: truncateAddress(h.address), value: h.share_percent }));
    if (rest.length > 0) rows.push({ name: "Other", value: restShare });
    return rows;
  }, [snapshot, topHolderCount]);

  const ranking = useMemo(() => {
    if (!snapshot) return [];
    return [...snapshot.holders].sort((a, b) => b.share_percent - a.share_percent).slice(0, topHolderCount);
  }, [snapshot, topHolderCount]);

  if (snapshots.length === 0) {
    return (
      <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
        <p className="text-sm text-base-300">No cap-table history available yet.</p>
      </div>
    );
  }

  const atStart = index === 0;
  const atEnd = index >= lastIndex;

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4">
      <div className="mb-1 flex flex-wrap items-baseline justify-between gap-2" aria-live="polite">
        <p className="text-sm font-semibold text-base-100">
          {assetId !== undefined ? `Asset ${assetId} · ` : ""}Ledger {snapshot.ledger}
          {snapshot.timestamp && <span className="ml-2 font-normal text-base-300">{snapshot.timestamp}</span>}
        </p>
        <p className="text-xs text-base-300">
          Snapshot {index + 1} of {snapshots.length}
          {snapshot.note && <span className="ml-2 italic">{snapshot.note}</span>}
        </p>
      </div>

      {/* Scrubber */}
      <label htmlFor="cap-table-timeline-scrubber" className="sr-only">
        Scrub cap-table history
      </label>
      <input
        id="cap-table-timeline-scrubber"
        type="range"
        min={0}
        max={Math.max(lastIndex, 0)}
        step={1}
        value={index}
        onChange={(e) => {
          setPlaying(false);
          setIndex(Number(e.target.value));
        }}
        aria-valuetext={`Snapshot ${index + 1} of ${snapshots.length}, ledger ${snapshot.ledger}`}
        className="w-full accent-emerald-500"
      />
      <div className="flex justify-between text-xs text-base-300" aria-hidden="true">
        <span>Inception{snapshots[0] ? ` · L${snapshots[0].ledger}` : ""}</span>
        <span>
          Current{snapshots[lastIndex] ? ` · L${snapshots[lastIndex].ledger}` : ""}
        </span>
      </div>

      {/* Transport controls */}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => setIndex((prev) => Math.max(prev - 1, 0))}
          disabled={atStart}
          aria-label="Previous snapshot"
          className="rounded-md border border-white/10 px-3 py-1.5 text-sm font-medium text-base-100 transition-colors hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
        >
          ‹ Prev
        </button>
        <button
          type="button"
          onClick={() => {
            if (atEnd) {
              setIndex(0);
              setPlaying(true);
            } else {
              setPlaying((p) => !p);
            }
          }}
          aria-label={playing ? "Pause playback" : atEnd ? "Replay from inception" : "Play timeline"}
          className="rounded-md border border-brand-500/40 bg-brand-500/15 px-3 py-1.5 text-sm font-medium text-brand-300 transition-colors hover:bg-brand-500/25 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        >
          {playing ? "❚❚ Pause" : atEnd ? "↺ Replay" : "▶ Play"}
        </button>
        <button
          type="button"
          onClick={() => setIndex((prev) => Math.min(prev + 1, lastIndex))}
          disabled={atEnd}
          aria-label="Next snapshot"
          className="rounded-md border border-white/10 px-3 py-1.5 text-sm font-medium text-base-100 transition-colors hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
        >
          Next ›
        </button>
        <div className="ml-auto flex items-center gap-1" role="group" aria-label="Playback speed">
          {SPEEDS.map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => setSpeed(s)}
              aria-pressed={speed === s}
              aria-label={`Playback speed ${s}x`}
              className={`rounded-md border px-2.5 py-1.5 text-xs font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 ${
                speed === s
                  ? "border-brand-500/40 bg-brand-500/15 text-brand-300"
                  : "border-white/10 text-base-300 hover:text-base-100"
              }`}
            >
              {s}x
            </button>
          ))}
        </div>
      </div>

      {/* Distribution chart + rankings (smooth transitions via recharts animation + CSS). */}
      <div className="mt-4 grid gap-4 transition-all duration-300 ease-in-out md:grid-cols-2">
        <div
          role="img"
          aria-label={`Holder distribution at ledger ${snapshot.ledger} across ${distributionData.length} groups`}
          className="h-64 w-full transition-opacity duration-300"
          key={`chart-${snapshot.ledger}`}
        >
          <ResponsiveContainer width="100%" height="100%">
            <PieChart>
              <Pie
                data={distributionData}
                dataKey="value"
                nameKey="name"
                innerRadius="55%"
                outerRadius="80%"
                paddingAngle={2}
                isAnimationActive
                animationDuration={300}
              >
                {distributionData.map((entry, i) => (
                  <Cell key={entry.name} fill={SLICE_COLORS[i % SLICE_COLORS.length]} />
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

        <div>
          <h4 className="mb-2 text-xs font-semibold uppercase tracking-wide text-base-300">
            Top holders · L{snapshot.ledger}
          </h4>
          <ol className="space-y-1.5">
            {ranking.map((h, rank) => (
              <li
                key={h.address}
                className="flex items-center justify-between gap-2 rounded-lg border border-white/5 bg-white/[0.02] px-3 py-1.5 text-sm transition-all duration-300"
              >
                <span className="flex items-center gap-2">
                  <span className="w-5 text-xs font-bold text-gold-400">#{rank + 1}</span>
                  <span className="font-mono text-base-200">{truncateAddress(h.address)}</span>
                </span>
                <span className="text-base-300">{h.share_percent.toFixed(2)}%</span>
              </li>
            ))}
          </ol>
          {ranking.length === 0 && <p className="text-sm text-base-300">No holders at this snapshot.</p>}
        </div>
      </div>
    </div>
  );
}

export default CapTableTimeline;
