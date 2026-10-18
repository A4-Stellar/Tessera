/* Progressive Web App (PWA) Offline Push Notification Engine (Issue #243)
 *
 * Implements W3C Push API and Notifications API for background alerts:
 * - Real-time alerts for dividend payouts, compliance updates, and system upgrades
 * - Background push payload parsing with deep linking
 * - Notification click handling and window focus/navigation
 * - Push subscription change renewal using VAPID
 *
 * Time Complexity: O(1) event handling and payload parsing.
 * Space Complexity: O(1) ephemeral memory footprint per notification.
 */

const DEFAULT_ICON = "/icons/icon-192.png";
const DEFAULT_BADGE = "/icons/icon-192.png";
const DEFAULT_TITLE = "Tessera Platform Notification";

/**
 * Supported notification alert categories matching user preference toggles:
 * - 'dividends': Payout declarations, yield distributions, DRIP claims
 * - 'compliance': KYC status updates, accreditation renewals, jurisdiction alerts
 * - 'system': Contract upgrades, emergency pause notices, protocol changes
 */
const NOTIFICATION_CATEGORIES = Object.freeze({
  DIVIDENDS: "dividends",
  COMPLIANCE: "compliance",
  SYSTEM: "system",
});

/**
 * Parses push payload safely from arrayBuffer, json, or text.
 * @param {PushMessageData|null} data
 * @returns {object}
 */
function parsePushPayload(data) {
  if (!data) {
    return {
      title: DEFAULT_TITLE,
      body: "New update received from Tessera Platform.",
      category: NOTIFICATION_CATEGORIES.SYSTEM,
      deepLink: "/",
      tag: "tessera-generic",
    };
  }

  try {
    return data.json();
  } catch (_jsonErr) {
    try {
      const text = data.text();
      return {
        title: DEFAULT_TITLE,
        body: text || "New update received from Tessera Platform.",
        category: NOTIFICATION_CATEGORIES.SYSTEM,
        deepLink: "/",
        tag: "tessera-text-" + Date.now(),
      };
    } catch (_textErr) {
      return {
        title: DEFAULT_TITLE,
        body: "New encrypted notification received.",
        category: NOTIFICATION_CATEGORIES.SYSTEM,
        deepLink: "/",
        tag: "tessera-raw-" + Date.now(),
      };
    }
  }
}

/**
 * Determines deep link based on payload category if not explicitly provided.
 * @param {string} category
 * @param {string|undefined} deepLink
 * @returns {string}
 */
function resolveDeepLink(category, deepLink) {
  if (deepLink && deepLink.startsWith("/")) {
    return deepLink;
  }
  switch (category) {
    case NOTIFICATION_CATEGORIES.DIVIDENDS:
      return "/en/docs/api-dividends";
    case NOTIFICATION_CATEGORIES.COMPLIANCE:
      return "/en/docs/compliance-guide";
    case NOTIFICATION_CATEGORIES.SYSTEM:
      return "/en/docs/architecture";
    default:
      return "/";
  }
}

// ---------------------------------------------------------------------------
// Push Event Handler
// ---------------------------------------------------------------------------
self.addEventListener("push", (event) => {
  const payload = parsePushPayload(event.data);
  const category = payload.category || NOTIFICATION_CATEGORIES.SYSTEM;
  const deepLink = resolveDeepLink(category, payload.deepLink);

  const title = payload.title || DEFAULT_TITLE;
  const options = {
    body: payload.body || "New notification from Tessera RWA platform.",
    icon: payload.icon || DEFAULT_ICON,
    badge: payload.badge || DEFAULT_BADGE,
    tag: payload.tag || `tessera-${category}-${Date.now()}`,
    data: {
      deepLink,
      category,
      timestamp: payload.timestamp || Date.now(),
      actionUrl: payload.actionUrl || deepLink,
    },
    requireInteraction: payload.requireInteraction ?? (category === NOTIFICATION_CATEGORIES.COMPLIANCE),
    renotify: true,
    actions: payload.actions || [
      {
        action: "view",
        title: "View Details",
      },
      {
        action: "dismiss",
        title: "Dismiss",
      },
    ],
  };

  event.waitUntil(self.registration.showNotification(title, options));
});

// ---------------------------------------------------------------------------
// Notification Click Handler (Deep Linking & Client Window Navigation)
// ---------------------------------------------------------------------------
self.addEventListener("notificationclick", (event) => {
  event.notification.close();

  if (event.action === "dismiss") {
    return;
  }

  const notificationData = event.notification.data || {};
  const targetUrl = new URL(
    notificationData.deepLink || notificationData.actionUrl || "/",
    self.location.origin
  ).href;

  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((windowClients) => {
        // Focus existing matching client window if already open
        for (const client of windowClients) {
          if (client.url === targetUrl && "focus" in client) {
            return client.focus();
          }
        }
        // Focus any existing client of same origin and navigate
        for (const client of windowClients) {
          if (client.url.startsWith(self.location.origin) && "focus" in client && "navigate" in client) {
            return client.navigate(targetUrl).then((focusedClient) => focusedClient.focus());
          }
        }
        // Open new window if none active
        if (self.clients.openWindow) {
          return self.clients.openWindow(targetUrl);
        }
      })
  );
});

// ---------------------------------------------------------------------------
// Push Subscription Change Handler (Automatic VAPID Re-registration)
// ---------------------------------------------------------------------------
self.addEventListener("pushsubscriptionchange", (event) => {
  event.waitUntil(
    self.registration.pushManager
      .subscribe(event.oldSubscription ? event.oldSubscription.options : { userVisibleOnly: true })
      .then((newSubscription) => {
        // Broadcast new subscription details to active clients or backend API
        return fetch("/api/notifications/refresh", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            oldEndpoint: event.oldSubscription ? event.oldSubscription.endpoint : null,
            newSubscription: newSubscription.toJSON(),
          }),
        }).catch(() => {
          // Swallow network errors if offline; subscription will retry on next launch
        });
      })
  );
});
