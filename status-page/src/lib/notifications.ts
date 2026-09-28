/**
 * Tessera Status Page — notification handlers
 *
 * Sends incident alerts to registered webhook URLs and formats email
 * notification payloads for downstream delivery.
 */

import type { IncidentRecord, WebhookSubscriber, EmailSubscriber } from "./monitor";

// ---------------------------------------------------------------------------
// Webhook notifications
// ---------------------------------------------------------------------------

export interface WebhookPayload {
  event: "incident_created" | "incident_updated" | "status_changed";
  incident?: IncidentRecord;
  service_ids?: string[];
  timestamp: string;
  source: "tessera-status-page";
}

/**
 * Send an incident notification to a registered webhook URL.
 *
 * The payload is a JSON object conforming to the `WebhookPayload` shape.
 * Returns `true` if the request succeeded (2xx), `false` otherwise.
 */
export async function sendWebhookNotification(
  subscriber: WebhookSubscriber,
  payload: WebhookPayload
): Promise<boolean> {
  // Only send if this subscriber has opted into this event type
  if (!subscriber.events.includes(payload.event)) {
    return false;
  }

  try {
    const resp = await fetch(subscriber.url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "User-Agent": "Tessera-StatusPage/1.0",
        // HMAC signature header would be added here in production using a shared secret
        // "X-Tessera-Signature": computeHmac(secret, JSON.stringify(payload)),
      },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(10_000),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Deliver a webhook notification to all registered subscribers for an event.
 * Runs all deliveries concurrently. Returns a map of subscriber URL → success.
 */
export async function notifySubscribers(
  subscribers: WebhookSubscriber[],
  payload: WebhookPayload
): Promise<Map<string, boolean>> {
  const results = await Promise.allSettled(
    subscribers.map((s) => sendWebhookNotification(s, payload))
  );

  const map = new Map<string, boolean>();
  subscribers.forEach((s, i) => {
    const r = results[i];
    map.set(s.url, r.status === "fulfilled" && r.value);
  });

  return map;
}

// ---------------------------------------------------------------------------
// Email payload generation
// ---------------------------------------------------------------------------

export interface EmailPayload {
  to: string;
  subject: string;
  text: string;
  html: string;
}

/**
 * Format an incident notification as an email payload.
 *
 * Does not send the email — the caller is responsible for passing this to
 * their email provider (SendGrid, SES, Resend, etc.).
 */
export function generateEmailPayload(
  subscriber: EmailSubscriber,
  incident: IncidentRecord
): EmailPayload {
  const affectedList = incident.services_affected.join(", ");
  const statusLabel = incident.status.charAt(0).toUpperCase() + incident.status.slice(1);
  const severityLabel = incident.severity.toUpperCase();
  const startedAt = new Date(incident.started_at).toLocaleString("en-US", {
    timeZone: "UTC",
    dateStyle: "medium",
    timeStyle: "short",
  });

  const latestUpdate = incident.updates.at(-1);
  const updateText = latestUpdate
    ? `Latest update: ${latestUpdate.message}`
    : "No updates yet.";

  const subject = `[${severityLabel}] Tessera Status: ${incident.title}`;

  const text = [
    `Tessera Status Page — Incident Notification`,
    ``,
    `Incident: ${incident.title}`,
    `Severity: ${severityLabel}`,
    `Status: ${statusLabel}`,
    `Started: ${startedAt} UTC`,
    `Affected services: ${affectedList}`,
    ``,
    updateText,
    ``,
    `View full status: https://status.tessera.finance`,
    ``,
    `You are receiving this because you subscribed to status updates for: ${subscriber.services.join(", ")}.`,
    `To unsubscribe, visit https://status.tessera.finance/unsubscribe`,
  ].join("\n");

  const html = `
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>${subject}</title>
</head>
<body style="margin:0;padding:0;background:#111827;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;color:#e5e7eb;">
  <table width="100%" cellpadding="0" cellspacing="0" style="max-width:600px;margin:32px auto;background:#1f2937;border-radius:8px;overflow:hidden;">
    <tr>
      <td style="padding:24px 32px;background:#0c4a6e;">
        <span style="font-size:20px;font-weight:700;color:#fff;">Tessera Status</span>
      </td>
    </tr>
    <tr>
      <td style="padding:24px 32px;">
        <p style="margin:0 0 8px;font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:${
          incident.severity === "critical"
            ? "#ef4444"
            : incident.severity === "major"
            ? "#f59e0b"
            : "#6366f1"
        };">${severityLabel} INCIDENT · ${statusLabel}</p>
        <h1 style="margin:0 0 16px;font-size:22px;font-weight:700;color:#f9fafb;">${incident.title}</h1>
        <table style="width:100%;border-collapse:collapse;margin-bottom:20px;">
          <tr>
            <td style="padding:8px 0;border-bottom:1px solid #374151;color:#9ca3af;font-size:14px;">Started</td>
            <td style="padding:8px 0;border-bottom:1px solid #374151;font-size:14px;text-align:right;">${startedAt} UTC</td>
          </tr>
          <tr>
            <td style="padding:8px 0;color:#9ca3af;font-size:14px;">Affected</td>
            <td style="padding:8px 0;font-size:14px;text-align:right;">${affectedList}</td>
          </tr>
        </table>
        ${
          latestUpdate
            ? `<div style="background:#111827;border-radius:6px;padding:16px;margin-bottom:20px;">
            <p style="margin:0 0 4px;font-size:12px;color:#6b7280;text-transform:uppercase;">Latest update</p>
            <p style="margin:0;font-size:15px;">${latestUpdate.message}</p>
          </div>`
            : ""
        }
        <a href="https://status.tessera.finance" style="display:inline-block;padding:10px 20px;background:#0284c7;color:#fff;border-radius:6px;text-decoration:none;font-size:14px;font-weight:600;">View Full Status →</a>
      </td>
    </tr>
    <tr>
      <td style="padding:16px 32px;background:#111827;font-size:12px;color:#6b7280;">
        You received this because you subscribed to ${subscriber.services.join(", ")} updates.
        <a href="https://status.tessera.finance/unsubscribe" style="color:#38bdf8;">Unsubscribe</a>
      </td>
    </tr>
  </table>
</body>
</html>
`.trim();

  return { to: subscriber.email, subject, text, html };
}

// ---------------------------------------------------------------------------
// RSS/Atom feed generation
// ---------------------------------------------------------------------------

/**
 * Generate an RSS 2.0 feed XML string from an array of incidents.
 */
export function generateRssFeed(incidents: IncidentRecord[], baseUrl: string): string {
  const buildDate = new Date().toUTCString();
  const items = incidents
    .slice(0, 20)
    .map((inc) => {
      const pubDate = new Date(inc.started_at).toUTCString();
      const statusLabel = inc.status.charAt(0).toUpperCase() + inc.status.slice(1);
      const description = inc.updates
        .map((u) => `${new Date(u.timestamp).toUTCString()}: ${u.message}`)
        .join("&#10;");

      return `
  <item>
    <title><![CDATA[${inc.severity.toUpperCase()}: ${inc.title} — ${statusLabel}]]></title>
    <link>${baseUrl}/incidents/${inc.id}</link>
    <guid isPermaLink="true">${baseUrl}/incidents/${inc.id}</guid>
    <pubDate>${pubDate}</pubDate>
    <description><![CDATA[${description || "No updates yet."}]]></description>
    <category>${inc.severity}</category>
  </item>`.trim();
    })
    .join("\n  ");

  return `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <title>Tessera Status — Incident Feed</title>
    <link>${baseUrl}</link>
    <description>Real-time incident notifications for Tessera API, Soroban RPC, and documentation.</description>
    <language>en-us</language>
    <lastBuildDate>${buildDate}</lastBuildDate>
    <atom:link href="${baseUrl}/api/rss" rel="self" type="application/rss+xml" />
    ${items}
  </channel>
</rss>`;
}
