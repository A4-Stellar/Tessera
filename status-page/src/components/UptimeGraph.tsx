"use client";

import type { UptimeRecord, DailyRecord, UptimeWindow } from "@/lib/monitor";
import { formatUptime } from "@/lib/monitor";
import clsx from "clsx";

// ---------------------------------------------------------------------------
// 90-day uptime bar chart
// ---------------------------------------------------------------------------

const STATUS_BAR_COLOUR: Record<DailyRecord["status"], string> = {
  operational: "#22c55e",
  degraded: "#f59e0b",
  outage: "#ef4444",
  maintenance: "#6366f1",
  unknown: "#4b5563",
};

interface UptimeBarProps {
  record: DailyRecord;
}

function UptimeBar({ record }: UptimeBarProps) {
  const colour = STATUS_BAR_COLOUR[record.status];
  const latencyLabel =
    record.avg_latency_ms !== null
      ? `${Math.round(record.avg_latency_ms)}ms avg`
      : "No data";
  const title = `${record.date} — ${record.status} (${latencyLabel})`;

  return (
    <div
      className="uptime-bar flex-1 rounded-sm cursor-default"
      style={{ height: "32px", backgroundColor: colour, minWidth: "2px" }}
      title={title}
      aria-label={title}
      role="img"
    />
  );
}

// ---------------------------------------------------------------------------
// Uptime window pill (7d / 30d / 90d)
// ---------------------------------------------------------------------------

interface UptimeWindowPillProps {
  window: UptimeWindow;
}

function UptimeWindowPill({ window }: UptimeWindowPillProps) {
  const pct = window.percentage;
  const colour =
    pct >= 99.9
      ? "text-green-400"
      : pct >= 99.0
      ? "text-amber-400"
      : "text-red-400";

  return (
    <div className="flex flex-col items-center gap-0.5">
      <span className={clsx("text-sm font-semibold tabular-nums", colour)}>
        {formatUptime(pct)}
      </span>
      <span className="text-xs text-gray-500">{window.label}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Full uptime graph card
// ---------------------------------------------------------------------------

interface UptimeGraphProps {
  uptimeRecord: UptimeRecord;
  serviceName: string;
}

/**
 * Renders a 90-day uptime bar chart for one service, plus 7d/30d/90d
 * uptime percentage pills.
 */
export function UptimeGraph({ uptimeRecord, serviceName }: UptimeGraphProps) {
  return (
    <div aria-label={`${serviceName} uptime history`}>
      {/* Bar chart */}
      <div className="flex items-end gap-px w-full mb-2">
        {uptimeRecord.daily.map((day) => (
          <UptimeBar key={day.date} record={day} />
        ))}
      </div>

      {/* Axis labels */}
      <div className="flex justify-between text-xs text-gray-600 mb-3 px-0.5">
        <span>90 days ago</span>
        <span>Today</span>
      </div>

      {/* Uptime percentage pills */}
      <div className="flex items-center gap-5">
        {uptimeRecord.windows.map((w) => (
          <UptimeWindowPill key={w.label} window={w} />
        ))}
      </div>
    </div>
  );
}
