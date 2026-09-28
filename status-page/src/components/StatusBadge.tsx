"use client";

import type { ServiceStatus } from "@/lib/monitor";
import clsx from "clsx";

interface StatusBadgeProps {
  status: ServiceStatus;
  size?: "sm" | "md" | "lg";
  showLabel?: boolean;
}

const STATUS_CONFIG: Record<
  ServiceStatus,
  { dotClass: string; labelClass: string; label: string }
> = {
  operational: {
    dotClass: "bg-green-500 status-dot-operational",
    labelClass: "text-green-400",
    label: "Operational",
  },
  degraded: {
    dotClass: "bg-amber-500",
    labelClass: "text-amber-400",
    label: "Degraded Performance",
  },
  outage: {
    dotClass: "bg-red-500",
    labelClass: "text-red-400",
    label: "Outage",
  },
  maintenance: {
    dotClass: "bg-indigo-500",
    labelClass: "text-indigo-400",
    label: "Under Maintenance",
  },
  unknown: {
    dotClass: "bg-gray-500",
    labelClass: "text-gray-400",
    label: "Unknown",
  },
};

const DOT_SIZE: Record<"sm" | "md" | "lg", string> = {
  sm: "h-2 w-2",
  md: "h-2.5 w-2.5",
  lg: "h-3 w-3",
};

/** Small coloured status dot with optional text label. */
export function StatusBadge({ status, size = "md", showLabel = false }: StatusBadgeProps) {
  const config = STATUS_CONFIG[status];
  return (
    <span className="inline-flex items-center gap-2">
      <span
        className={clsx("inline-block rounded-full flex-shrink-0", DOT_SIZE[size], config.dotClass)}
        role="img"
        aria-label={config.label}
      />
      {showLabel && (
        <span className={clsx("text-sm font-medium", config.labelClass)}>{config.label}</span>
      )}
    </span>
  );
}

/** Full-width banner summarising overall system status. */
export function OverallStatusBanner({
  overall,
}: {
  overall: "all_operational" | "partial_outage" | "major_outage" | "maintenance";
}) {
  const config = {
    all_operational: {
      bg: "bg-green-900/40 border-green-700/50",
      text: "text-green-300",
      dot: "bg-green-500 status-dot-operational",
      label: "All Systems Operational",
    },
    partial_outage: {
      bg: "bg-amber-900/40 border-amber-700/50",
      text: "text-amber-300",
      dot: "bg-amber-500",
      label: "Partial System Outage",
    },
    major_outage: {
      bg: "bg-red-900/40 border-red-700/50",
      text: "text-red-300",
      dot: "bg-red-500",
      label: "Major System Outage",
    },
    maintenance: {
      bg: "bg-indigo-900/40 border-indigo-700/50",
      text: "text-indigo-300",
      dot: "bg-indigo-500",
      label: "Scheduled Maintenance",
    },
  }[overall];

  return (
    <div
      className={clsx("flex items-center gap-3 rounded-xl border px-5 py-4", config.bg)}
      role="status"
      aria-live="polite"
    >
      <span className={clsx("h-3.5 w-3.5 rounded-full flex-shrink-0", config.dot)} />
      <span className={clsx("text-lg font-semibold", config.text)}>{config.label}</span>
    </div>
  );
}
