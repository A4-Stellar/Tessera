/**
 * Tessera Status Page — monitoring core
 *
 * Defines types for service status and uptime windows, and provides
 * server-side probe functions used by the /api/status route.
 */

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type ServiceStatus = "operational" | "degraded" | "outage" | "maintenance" | "unknown";

export type OverallStatus = "all_operational" | "partial_outage" | "major_outage" | "maintenance";

export interface ServiceResult {
  /** Unique identifier for the service. */
  id: string;
  /** Human-readable display name. */
  name: string;
  /** Current status. */
  status: ServiceStatus;
  /** Response latency in milliseconds, or null if unreachable. */
  latency_ms: number | null;
  /** HTTP status code returned, or null. */
  http_status: number | null;
  /** ISO 8601 timestamp of this probe run. */
  checked_at: string;
  /** Optional error message for failed probes. */
  error?: string;
}

export interface StatusPayload {
  overall: OverallStatus;
  services: ServiceResult[];
  checked_at: string;
}

export interface UptimeWindow {
  label: "7d" | "30d" | "90d";
  /** 0.0–100.0 */
  percentage: number;
}

export interface UptimeRecord {
  service_id: string;
  windows: UptimeWindow[];
  /** Array of {date: ISO string, status: ServiceStatus} for the uptime graph */
  daily: DailyRecord[];
}

export interface DailyRecord {
  date: string; // YYYY-MM-DD
  status: ServiceStatus;
  avg_latency_ms: number | null;
}

export interface IncidentRecord {
  id: string;
  title: string;
  status: "investigating" | "identified" | "monitoring" | "resolved";
  severity: "minor" | "major" | "critical";
  started_at: string;
  resolved_at: string | null;
  services_affected: string[];
  updates: IncidentUpdate[];
}

export interface IncidentUpdate {
  timestamp: string;
  message: string;
  status: IncidentRecord["status"];
}

export interface WebhookSubscriber {
  url: string;
  events: Array<"incident_created" | "incident_updated" | "status_changed">;
}

export interface EmailSubscriber {
  email: string;
  services: string[];
}

// ---------------------------------------------------------------------------
// Service definitions
// ---------------------------------------------------------------------------

/** The set of services the status page monitors. */
export const MONITORED_SERVICES: Array<{
  id: string;
  name: string;
  url: string;
  timeout_ms: number;
  /** Expected substring in response body (optional). */
  expected_body?: string;
}> = [
  {
    id: "tessera-api",
    name: "Tessera REST API",
    url: process.env.TESSERA_API_URL ?? "https://api.tessera.finance/health",
    timeout_ms: 10_000,
    expected_body: "ok",
  },
  {
    id: "soroban-rpc",
    name: "Soroban Testnet RPC",
    url:
      process.env.SOROBAN_RPC_URL ??
      "https://soroban-testnet.stellar.org",
    timeout_ms: 15_000,
  },
  {
    id: "tessera-docs",
    name: "Tessera Documentation",
    url: process.env.TESSERA_DOCS_URL ?? "https://tessera.finance",
    timeout_ms: 10_000,
  },
];

// ---------------------------------------------------------------------------
// Probe function (server-side only)
// ---------------------------------------------------------------------------

/**
 * Probe a single service endpoint and return its status.
 * Must be called from a Server Component or API route (not client-side).
 */
export async function probeEndpoint(service: (typeof MONITORED_SERVICES)[0]): Promise<ServiceResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), service.timeout_ms);
  const start = Date.now();

  try {
    const resp = await fetch(service.url, {
      signal: controller.signal,
      // Bypass Next.js cache for status probes
      cache: "no-store",
      headers: { "User-Agent": "Tessera-StatusPage/1.0" },
    });

    const latency_ms = Date.now() - start;
    clearTimeout(timer);

    let body = "";
    try {
      body = await resp.text();
    } catch {
      /* ignore body read errors */
    }

    let status: ServiceStatus;
    if (resp.ok) {
      // Check body expectation if defined
      if (service.expected_body && !body.includes(service.expected_body)) {
        status = "degraded";
      } else if (latency_ms > 5_000) {
        status = "degraded";
      } else {
        status = "operational";
      }
    } else if (resp.status >= 500) {
      status = "outage";
    } else if (resp.status >= 400) {
      // 4xx may indicate maintenance or partial degradation
      status = "degraded";
    } else {
      status = "unknown";
    }

    return {
      id: service.id,
      name: service.name,
      status,
      latency_ms,
      http_status: resp.status,
      checked_at: new Date().toISOString(),
    };
  } catch (err: unknown) {
    clearTimeout(timer);
    const latency_ms = Date.now() - start;
    const error =
      err instanceof Error
        ? err.name === "AbortError"
          ? `Timed out after ${service.timeout_ms}ms`
          : err.message
        : "Unknown error";

    return {
      id: service.id,
      name: service.name,
      status: "outage",
      latency_ms,
      http_status: null,
      checked_at: new Date().toISOString(),
      error,
    };
  }
}

/**
 * Compute the overall system status from an array of service results.
 */
export function computeOverallStatus(services: ServiceResult[]): OverallStatus {
  const statuses = services.map((s) => s.status);

  if (statuses.some((s) => s === "maintenance")) {
    if (statuses.every((s) => s === "maintenance" || s === "operational")) {
      return "maintenance";
    }
  }

  const outageCount = statuses.filter((s) => s === "outage").length;
  const degradedCount = statuses.filter((s) => s === "degraded").length;

  if (outageCount > 0) {
    return outageCount === statuses.length ? "major_outage" : "partial_outage";
  }
  if (degradedCount > 0) {
    return "partial_outage";
  }
  return "all_operational";
}

/**
 * Generate synthetic 90-day uptime history for a given service.
 *
 * In production this would be fetched from a time-series store (e.g.
 * Postgres, InfluxDB). For the prototype, we generate realistic data
 * seeded deterministically from the service ID so it stays stable
 * across refreshes.
 */
export function generateUptimeHistory(serviceId: string): UptimeRecord {
  const daily: DailyRecord[] = [];
  const seed = serviceId.charCodeAt(0) + serviceId.charCodeAt(serviceId.length - 1);

  for (let i = 89; i >= 0; i--) {
    const date = new Date();
    date.setDate(date.getDate() - i);
    const dateStr = date.toISOString().split("T")[0];

    // Deterministic pseudo-random using seed + day index
    const r = ((seed * 1_013_904_223 + i * 1_664_525) >>> 0) / 0xffffffff;

    let status: ServiceStatus;
    let avg_latency_ms: number | null;

    if (r > 0.03) {
      status = "operational";
      avg_latency_ms = 80 + Math.floor(r * 200);
    } else if (r > 0.01) {
      status = "degraded";
      avg_latency_ms = 2_000 + Math.floor(r * 3_000);
    } else {
      status = "outage";
      avg_latency_ms = null;
    }

    daily.push({ date: dateStr, status, avg_latency_ms });
  }

  const computeWindow = (days: number): number => {
    const slice = daily.slice(daily.length - days);
    const up = slice.filter((d) => d.status === "operational").length;
    return (up / days) * 100;
  };

  return {
    service_id: serviceId,
    windows: [
      { label: "7d", percentage: computeWindow(7) },
      { label: "30d", percentage: computeWindow(30) },
      { label: "90d", percentage: computeWindow(90) },
    ],
    daily,
  };
}

/**
 * Format uptime percentage for display: "99.97%"
 */
export function formatUptime(percentage: number): string {
  if (percentage >= 99.99) return "100%";
  return `${percentage.toFixed(2)}%`;
}

/**
 * Format latency for display: "142ms" or "<1ms"
 */
export function formatLatency(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < 1) return "<1ms";
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)}s`;
  return `${Math.round(ms)}ms`;
}
