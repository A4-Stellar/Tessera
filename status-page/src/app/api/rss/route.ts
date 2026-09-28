/**
 * GET /api/rss
 *
 * Returns an RSS 2.0 feed of recent Tessera incidents.
 *
 * Subscribe in your feed reader or use as a webhook trigger source:
 *   https://status.tessera.finance/api/rss
 */

import { NextResponse } from "next/server";
import { generateRssFeed } from "@/lib/notifications";
import type { IncidentRecord } from "@/lib/monitor";

export const runtime = "nodejs";
export const revalidate = 300; // re-generate feed at most every 5 minutes

// In production, incidents would be loaded from a database.
// This file ships with a representative example to seed the feed.
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
          "We are investigating elevated response times on the /assets endpoint. " +
          "The issue appears to be related to an unusually large XDR payload from the Soroban RPC.",
      },
      {
        timestamp: "2026-09-20T14:51:00Z",
        status: "identified",
        message:
          "Root cause identified: the indexer was processing a large ledger batch. " +
          "Memory pressure caused GC pauses extending response times. Fix being deployed.",
      },
      {
        timestamp: "2026-09-20T15:10:00Z",
        status: "resolved",
        message:
          "The latency issue has been resolved. All endpoints are responding normally. " +
          "We will add a circuit-breaker to prevent large ledger batches from affecting the API.",
      },
    ],
  },
];

export async function GET(request: Request): Promise<Response> {
  const baseUrl =
    process.env.NEXT_PUBLIC_STATUS_URL ??
    `${new URL(request.url).protocol}//${new URL(request.url).host}`;

  const xml = generateRssFeed(EXAMPLE_INCIDENTS, baseUrl);

  return new Response(xml, {
    headers: {
      "Content-Type": "application/rss+xml; charset=UTF-8",
      "Cache-Control": "public, max-age=300, s-maxage=300",
      "X-Content-Type-Options": "nosniff",
    },
  });
}
