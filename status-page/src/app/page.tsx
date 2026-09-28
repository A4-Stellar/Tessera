"use client";

import { useState, useEffect, useCallback } from "react";
import { OverallStatusBanner, StatusBadge } from "@/components/StatusBadge";
import { UptimeGraph } from "@/components/UptimeGraph";
import { IncidentBanner, IncidentHistory } from "@/components/IncidentBanner";
import { SubscribeModal } from "@/components/SubscribeModal";
import { formatLatency } from "@/lib/monitor";
import type { StatusPayload, UptimeRecord, ServiceResult, IncidentRecord } from "@/lib/monitor";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type ApiStatusResponse = StatusPayload & {
  probe_duration_ms: number;
  uptime: UptimeRecord[];
};

// ---------------------------------------------------------------------------
// Example incident history (in production, fetched from /api/incidents)
// ---------------------------------------------------------------------------

const EXAMPLE_INCIDENTS: IncidentRecord[] = [
  {
    id: "inc-2026-001",
    title: "Elevated API latency on /assets endpoint",
    status: "resolved",
    severity: "minor",
    started_at: "2026-09-20T14:23:00Z",
    resolved_at: "2026-09-20T15:10:00Z",
    services_affected: ["tessera-api"],
    updates: [
      {
        timestamp: "2026-09-20T14:23:00Z",
        status: "investigating",
        message:
          "We are investigating elevated response times on the /assets endpoint.",
      },
      {
        timestamp: "2026-09-20T15:10:00Z",
        status: "resolved",
        message: "Resolved. All endpoints are responding normally.",
      },
    ],
  },
];

// ---------------------------------------------------------------------------
// Service card
// ---------------------------------------------------------------------------

function ServiceCard({
  service,
  uptimeRecord,
}: {
  service: ServiceResult;
  uptimeRecord: UptimeRecord | undefined;
}) {
  return (
    <div className="bg-gray-900 rounded-xl border border-gray-800 p-5">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2.5">
          <StatusBadge status={service.status} size="md" />
          <span className="font-medium text-gray-100">{service.name}</span>
        </div>
        <div className="flex items-center gap-4 text-sm">
          <span className="text-gray-500 tabular-nums">
            {formatLatency(service.latency_ms)}
          </span>
          <span
            className={
              service.status === "operational"
                ? "text-green-400"
                : service.status === "degraded"
                ? "text-amber-400"
                : service.status === "outage"
                ? "text-red-400"
                : "text-gray-400"
            }
          >
            {service.status === "operational"
              ? "Operational"
              : service.status === "degraded"
              ? "Degraded"
              : service.status === "outage"
              ? "Outage"
              : service.status === "maintenance"
              ? "Maintenance"
              : "Unknown"}
          </span>
        </div>
      </div>

      {uptimeRecord && (
        <UptimeGraph uptimeRecord={uptimeRecord} serviceName={service.name} />
      )}

      {service.error && (
        <p className="mt-2 text-xs text-red-400 font-mono truncate" title={service.error}>
          {service.error}
        </p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

const REFRESH_INTERVAL_MS = 60_000; // 60 seconds

export default function StatusPage() {
  const [data, setData] = useState<ApiStatusResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);
  const [countdown, setCountdown] = useState(REFRESH_INTERVAL_MS / 1000);
  const [subscribeOpen, setSubscribeOpen] = useState(false);

  const fetchStatus = useCallback(async () => {
    try {
      const resp = await fetch("/api/status", { cache: "no-store" });
      if (resp.ok) {
        const json: ApiStatusResponse = await resp.json();
        setData(json);
        setLastChecked(new Date());
      }
    } catch {
      // Network error — keep showing stale data
    } finally {
      setLoading(false);
      setCountdown(REFRESH_INTERVAL_MS / 1000);
    }
  }, []);

  // Initial fetch
  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);

  // Auto-refresh every 60s
  useEffect(() => {
    const interval = setInterval(fetchStatus, REFRESH_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchStatus]);

  // Countdown timer displayed to users
  useEffect(() => {
    const tick = setInterval(() => {
      setCountdown((c) => (c > 0 ? c - 1 : REFRESH_INTERVAL_MS / 1000));
    }, 1000);
    return () => clearInterval(tick);
  }, []);

  // Derive uptime map
  const uptimeMap = new Map<string, UptimeRecord>(
    data?.uptime?.map((u) => [u.service_id, u]) ?? []
  );

  return (
    <div className="min-h-screen bg-gray-950">
      {/* Header */}
      <header className="border-b border-gray-800 bg-gray-950/80 backdrop-blur-sm sticky top-0 z-40">
        <div className="max-w-3xl mx-auto px-4 sm:px-6 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            {/* Tessera wordmark */}
            <span className="text-xl font-bold text-gray-100 tracking-tight">
              <span className="text-brand-400">Tessera</span>
              <span className="text-gray-400 font-normal ml-2 text-sm">Status</span>
            </span>
          </div>
          <div className="flex items-center gap-3">
            <a
              href="/api/rss"
              className="text-xs text-gray-500 hover:text-gray-300 flex items-center gap-1 transition-colors"
              title="RSS incident feed"
            >
              <svg className="h-3.5 w-3.5" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                <path d="M3.75 3a.75.75 0 00-.75.75v.5c0 .414.336.75.75.75H4c6.075 0 11 4.925 11 11v.25c0 .414.336.75.75.75h.5a.75.75 0 00.75-.75V16C17 8.82 11.18 3 4 3h-.25zM3 8.75A.75.75 0 013.75 8H4a8 8 0 018 8v.25a.75.75 0 01-.75.75h-.5a.75.75 0 01-.75-.75V16a6 6 0 00-6-6h-.25A.75.75 0 013 9.25v-.5zM7 15a2 2 0 11-4 0 2 2 0 014 0z" />
              </svg>
              RSS
            </a>
            <button
              onClick={() => setSubscribeOpen(true)}
              className="text-xs px-3 py-1.5 rounded-lg bg-brand-700 hover:bg-brand-600 text-white font-medium transition-colors"
            >
              Subscribe
            </button>
          </div>
        </div>
      </header>

      <main className="max-w-3xl mx-auto px-4 sm:px-6 py-8 space-y-8">
        {/* Overall status banner */}
        <section>
          {loading ? (
            <div className="h-14 rounded-xl bg-gray-800 animate-pulse" />
          ) : data ? (
            <OverallStatusBanner overall={data.overall} />
          ) : (
            <OverallStatusBanner overall="major_outage" />
          )}
        </section>

        {/* Active incident banners */}
        <IncidentBanner incidents={EXAMPLE_INCIDENTS} />

        {/* Last checked / refresh info */}
        <div className="flex items-center justify-between text-xs text-gray-600">
          <span>
            {lastChecked
              ? `Last checked: ${lastChecked.toLocaleTimeString()}`
              : "Checking…"}
          </span>
          <button
            onClick={fetchStatus}
            className="flex items-center gap-1.5 text-gray-500 hover:text-gray-300 transition-colors"
            title={`Auto-refreshes in ${countdown}s`}
          >
            <svg
              className="h-3.5 w-3.5"
              viewBox="0 0 20 20"
              fill="currentColor"
              aria-hidden="true"
            >
              <path
                fillRule="evenodd"
                d="M15.312 11.424a5.5 5.5 0 01-9.201 2.466l-.312-.311h2.433a.75.75 0 000-1.5H3.989a.75.75 0 00-.75.75v4.242a.75.75 0 001.5 0v-2.43l.31.31a7 7 0 0011.712-3.138.75.75 0 00-1.449-.389zm1.23-3.723a.75.75 0 00.219-.53V2.929a.75.75 0 00-1.5 0V5.36l-.31-.31A7 7 0 003.239 8.188a.75.75 0 101.448.389A5.5 5.5 0 0113.89 6.11l.311.31h-2.432a.75.75 0 000 1.5h4.243a.75.75 0 00.53-.219z"
                clipRule="evenodd"
              />
            </svg>
            Refresh ({countdown}s)
          </button>
        </div>

        {/* Service cards */}
        <section aria-label="Service status">
          <h2 className="text-sm font-semibold text-gray-500 uppercase tracking-wider mb-4">
            Services
          </h2>

          {loading ? (
            <div className="space-y-3">
              {[1, 2, 3].map((i) => (
                <div key={i} className="h-32 rounded-xl bg-gray-800 animate-pulse" />
              ))}
            </div>
          ) : data ? (
            <div className="space-y-3">
              {data.services.map((svc) => (
                <ServiceCard
                  key={svc.id}
                  service={svc}
                  uptimeRecord={uptimeMap.get(svc.id)}
                />
              ))}
            </div>
          ) : (
            <p className="text-sm text-gray-500 py-4 text-center">
              Unable to fetch service status. Retrying…
            </p>
          )}
        </section>

        {/* Incident history */}
        <section>
          <h2 className="text-sm font-semibold text-gray-500 uppercase tracking-wider mb-4">
            Incident History
          </h2>
          <div className="bg-gray-900 rounded-xl border border-gray-800 p-5">
            <IncidentHistory incidents={EXAMPLE_INCIDENTS} />
          </div>
        </section>

        {/* Response metrics (probe timing) */}
        {data && (
          <section>
            <h2 className="text-sm font-semibold text-gray-500 uppercase tracking-wider mb-4">
              Response Metrics
            </h2>
            <div className="bg-gray-900 rounded-xl border border-gray-800 p-5">
              <div className="grid grid-cols-3 gap-4">
                {data.services.map((svc) => (
                  <div key={svc.id} className="flex flex-col gap-1">
                    <span className="text-xs text-gray-500 truncate">{svc.name}</span>
                    <span className="text-lg font-semibold tabular-nums text-gray-100">
                      {formatLatency(svc.latency_ms)}
                    </span>
                  </div>
                ))}
              </div>
              <p className="mt-4 text-xs text-gray-600">
                Probe completed in {data.probe_duration_ms}ms ·{" "}
                {new Date(data.checked_at).toLocaleTimeString()}
              </p>
            </div>
          </section>
        )}
      </main>

      {/* Footer */}
      <footer className="max-w-3xl mx-auto px-4 sm:px-6 py-8 flex flex-wrap gap-4 items-center justify-between border-t border-gray-800 mt-4">
        <p className="text-xs text-gray-600">
          © {new Date().getFullYear()} Tessera. All rights reserved.
        </p>
        <div className="flex items-center gap-4 text-xs text-gray-600">
          <a href="https://tessera.finance" className="hover:text-gray-400 transition-colors">
            Tessera.finance
          </a>
          <a href="/api/rss" className="hover:text-gray-400 transition-colors">
            RSS Feed
          </a>
          <a
            href="https://github.com/A4-Stellar/Tessera"
            className="hover:text-gray-400 transition-colors"
          >
            GitHub
          </a>
        </div>
      </footer>

      {/* Subscribe modal */}
      <SubscribeModal isOpen={subscribeOpen} onClose={() => setSubscribeOpen(false)} />
    </div>
  );
}
