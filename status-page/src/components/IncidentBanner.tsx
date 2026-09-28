"use client";

import type { IncidentRecord } from "@/lib/monitor";
import clsx from "clsx";

interface IncidentBannerProps {
  incidents: IncidentRecord[];
}

const SEVERITY_STYLES: Record<
  IncidentRecord["severity"],
  { container: string; badge: string; label: string }
> = {
  critical: {
    container: "bg-red-900/50 border-red-600/60",
    badge: "bg-red-600 text-white",
    label: "Critical",
  },
  major: {
    container: "bg-amber-900/50 border-amber-600/60",
    badge: "bg-amber-500 text-white",
    label: "Major",
  },
  minor: {
    container: "bg-blue-900/40 border-blue-700/50",
    badge: "bg-blue-600 text-white",
    label: "Minor",
  },
};

const STATUS_LABEL: Record<IncidentRecord["status"], string> = {
  investigating: "Investigating",
  identified: "Identified",
  monitoring: "Monitoring",
  resolved: "Resolved",
};

/**
 * Renders active (unresolved) incidents as stacked alert banners.
 * Returns null when there are no active incidents.
 */
export function IncidentBanner({ incidents }: IncidentBannerProps) {
  const active = incidents.filter((i) => i.resolved_at === null);

  if (active.length === 0) return null;

  return (
    <section aria-label="Active incidents" className="flex flex-col gap-3">
      {active.map((incident) => {
        const styles = SEVERITY_STYLES[incident.severity];
        const latestUpdate = incident.updates.at(-1);

        return (
          <div
            key={incident.id}
            className={clsx("rounded-xl border p-4", styles.container)}
            role="alert"
          >
            <div className="flex flex-wrap items-center gap-2 mb-2">
              <span
                className={clsx(
                  "inline-flex items-center rounded px-2 py-0.5 text-xs font-semibold uppercase tracking-wide",
                  styles.badge
                )}
              >
                {styles.label}
              </span>
              <span className="text-xs text-gray-400 font-medium uppercase tracking-wide">
                {STATUS_LABEL[incident.status]}
              </span>
              <span className="ml-auto text-xs text-gray-500">
                {formatRelativeTime(incident.started_at)}
              </span>
            </div>

            <p className="font-semibold text-gray-100 mb-1">{incident.title}</p>

            {latestUpdate && (
              <p className="text-sm text-gray-300">{latestUpdate.message}</p>
            )}

            {incident.services_affected.length > 0 && (
              <p className="mt-2 text-xs text-gray-500">
                Affected:{" "}
                <span className="text-gray-400">
                  {incident.services_affected.join(", ")}
                </span>
              </p>
            )}
          </div>
        );
      })}
    </section>
  );
}

/**
 * Incident history card showing all past incidents (resolved and unresolved).
 */
export function IncidentHistory({ incidents }: { incidents: IncidentRecord[] }) {
  if (incidents.length === 0) {
    return (
      <p className="text-sm text-gray-500 py-4 text-center">
        No incidents recorded in the past 90 days.
      </p>
    );
  }

  return (
    <div className="flex flex-col divide-y divide-gray-800">
      {incidents.map((incident) => {
        const styles = SEVERITY_STYLES[incident.severity];
        return (
          <div key={incident.id} className="py-4 first:pt-0 last:pb-0">
            <div className="flex flex-wrap items-center gap-2 mb-1">
              <span
                className={clsx(
                  "inline-flex items-center rounded px-2 py-0.5 text-xs font-semibold uppercase",
                  styles.badge
                )}
              >
                {styles.label}
              </span>
              {incident.resolved_at && (
                <span className="text-xs text-green-500 font-medium">Resolved</span>
              )}
              <span className="ml-auto text-xs text-gray-500">
                {new Date(incident.started_at).toLocaleDateString("en-US", {
                  month: "short",
                  day: "numeric",
                  year: "numeric",
                })}
              </span>
            </div>
            <p className="font-medium text-gray-200">{incident.title}</p>
            <p className="text-xs text-gray-500 mt-1">
              Affected: {incident.services_affected.join(", ")}
            </p>
            {incident.resolved_at && (
              <p className="text-xs text-gray-600 mt-1">
                Duration:{" "}
                {formatDuration(incident.started_at, incident.resolved_at)}
              </p>
            )}
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatRelativeTime(isoString: string): string {
  const diff = Date.now() - new Date(isoString).getTime();
  const mins = Math.floor(diff / 60_000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);

  if (days > 0) return `${days}d ago`;
  if (hours > 0) return `${hours}h ago`;
  if (mins > 0) return `${mins}m ago`;
  return "just now";
}

function formatDuration(start: string, end: string): string {
  const diffMs = new Date(end).getTime() - new Date(start).getTime();
  const mins = Math.floor(diffMs / 60_000);
  const hours = Math.floor(mins / 60);

  if (hours > 0) return `${hours}h ${mins % 60}m`;
  return `${mins}m`;
}
