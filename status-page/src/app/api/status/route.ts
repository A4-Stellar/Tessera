/**
 * GET /api/status
 *
 * Server-side route that probes all monitored endpoints and returns a
 * structured `StatusPayload` JSON response.  Called by the client page
 * every 60 seconds.
 *
 * Response is not cached by Next.js (`no-store`) so every request
 * triggers a fresh set of probes.
 */

import { NextResponse } from "next/server";
import {
  MONITORED_SERVICES,
  StatusPayload,
  computeOverallStatus,
  probeEndpoint,
  generateUptimeHistory,
} from "@/lib/monitor";

export const runtime = "nodejs";
// Never cache — always probe live
export const revalidate = 0;
export const dynamic = "force-dynamic";

export async function GET(): Promise<NextResponse<StatusPayload & { uptime: ReturnType<typeof generateUptimeHistory>[] }>> {
  const probeStart = Date.now();

  // Probe all services concurrently
  const results = await Promise.all(MONITORED_SERVICES.map(probeEndpoint));

  const overall = computeOverallStatus(results);

  // Generate uptime history for each service
  const uptime = MONITORED_SERVICES.map((s) => generateUptimeHistory(s.id));

  const payload = {
    overall,
    services: results,
    checked_at: new Date().toISOString(),
    probe_duration_ms: Date.now() - probeStart,
    uptime,
  };

  return NextResponse.json(payload, {
    headers: {
      "Cache-Control": "no-store, no-cache, must-revalidate",
      "X-Content-Type-Options": "nosniff",
    },
  });
}
