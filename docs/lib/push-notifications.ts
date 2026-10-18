/**
 * Push Notification Utility & Subscription Manager (Issue #243)
 *
 * Provides client-side helpers to:
 * - Validate browser Web Push API and Notification API support
 * - Convert URL-safe base64 VAPID public keys to Uint8Array for PushManager
 * - Manage user alert preferences (Dividends, Compliance, System Upgrades)
 * - Subscribe/unsubscribe with Service Worker PushManager
 * - Trigger local test notifications
 */

export interface NotificationPreferences {
  dividends: boolean;
  compliance: boolean;
  system: boolean;
}

export const DEFAULT_PREFERENCES: NotificationPreferences = {
  dividends: true,
  compliance: true,
  system: false,
};

export const PREFERENCES_STORAGE_KEY = "tessera_notification_preferences";
export const SUBSCRIPTION_STORAGE_KEY = "tessera_push_subscription";

// Sample public VAPID key for development and testnet environment (P-256 raw uncompressed point in URL-safe base64)
export const DEFAULT_VAPID_PUBLIC_KEY =
  process.env.NEXT_PUBLIC_VAPID_PUBLIC_KEY ||
  "BEl62iUYgUivxIkv69yViEuiBIa-Ib9-SkvMeAtA3LFgDzkrxZJjSgSnfckjBJuBkr3qBUYIHBQFLXYp5Nksh8U";

/**
 * Converts a URL-safe Base64 string to a Uint8Array for PushManager.subscribe.
 * @param {string} base64String
 * @returns {Uint8Array}
 */
export function urlBase64ToUint8Array(base64String: string): Uint8Array {
  const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
  const base64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
  const rawData = window.atob(base64);
  const outputArray = new Uint8Array(rawData.length);
  for (let i = 0; i < rawData.length; ++i) {
    outputArray[i] = rawData.charCodeAt(i);
  }
  return outputArray;
}

/**
 * Returns true if Web Push API and Service Workers are supported in current browser.
 */
export function isPushNotificationSupported(): boolean {
  if (typeof window === "undefined") return false;
  return "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

/**
 * Retrieves stored notification preferences or defaults.
 */
export function getStoredPreferences(): NotificationPreferences {
  if (typeof window === "undefined") return DEFAULT_PREFERENCES;
  try {
    const raw = localStorage.getItem(PREFERENCES_STORAGE_KEY);
    if (!raw) return DEFAULT_PREFERENCES;
    return { ...DEFAULT_PREFERENCES, ...JSON.parse(raw) };
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

/**
 * Persists notification preferences to localStorage.
 */
export function saveStoredPreferences(prefs: NotificationPreferences): void {
  if (typeof window === "undefined") return;
  try {
    localStorage.setItem(PREFERENCES_STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Ignore storage write errors (e.g. private mode quota)
  }
}

/**
 * Checks current notification permission status.
 */
export function getNotificationPermissionStatus(): NotificationPermission {
  if (typeof window === "undefined" || !("Notification" in window)) {
    return "default";
  }
  return Notification.permission;
}

/**
 * Subscribes user to Web Push notifications using VAPID key.
 */
export async function subscribeToPushNotifications(
  vapidPublicKey: string = DEFAULT_VAPID_PUBLIC_KEY,
  preferences: NotificationPreferences = getStoredPreferences()
): Promise<PushSubscription | null> {
  if (!isPushNotificationSupported()) {
    throw new Error("Push notifications are not supported in this browser.");
  }

  // Request browser permission
  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    throw new Error("Notification permission was not granted.");
  }

  // Ensure service worker is ready
  const registration = await navigator.serviceWorker.ready;
  if (!registration.pushManager) {
    throw new Error("PushManager not available on ServiceWorkerRegistration.");
  }

  // Convert VAPID key
  const applicationServerKey = urlBase64ToUint8Array(vapidPublicKey);

  // Subscribe via PushManager
  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: applicationServerKey.buffer as ArrayBuffer,
  });

  saveStoredPreferences(preferences);

  // Save subscription to local state and notify server
  try {
    localStorage.setItem(SUBSCRIPTION_STORAGE_KEY, JSON.stringify(subscription.toJSON()));
    await fetch("/api/notifications/subscribe", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        subscription: subscription.toJSON(),
        preferences,
      }),
    }).catch(() => {
      // Offline fallback: subscription saved locally
    });
  } catch {
    // Ignore offline errors
  }

  return subscription;
}

/**
 * Unsubscribes from push notifications.
 */
export async function unsubscribeFromPushNotifications(): Promise<boolean> {
  if (!isPushNotificationSupported()) return false;
  try {
    const registration = await navigator.serviceWorker.ready;
    const subscription = await registration.pushManager.getSubscription();
    if (subscription) {
      await subscription.unsubscribe();
      localStorage.removeItem(SUBSCRIPTION_STORAGE_KEY);
      return true;
    }
    return false;
  } catch {
    return false;
  }
}

/**
 * Dispatches an immediate test push notification through Service Worker.
 */
export async function sendTestNotification(category: "dividends" | "compliance" | "system"): Promise<void> {
  if (!isPushNotificationSupported()) return;
  const registration = await navigator.serviceWorker.ready;

  const payloads = {
    dividends: {
      title: "Dividend Declared: $150,000 USDC",
      body: "Aspen Prime Commercial RWA declared a monthly dividend payout of $150,000 USDC.",
      deepLink: "/en/docs/api-dividends",
    },
    compliance: {
      title: "KYC Accreditation Approved",
      body: "Institutional investor identity attestation verified on Soroban testnet.",
      deepLink: "/en/docs/compliance-guide",
    },
    system: {
      title: "Contract Upgrade Announcement",
      body: "Scheduled multi-sig board resolution for Asset-Token v1.2 execution window.",
      deepLink: "/en/docs/architecture",
    },
  };

  const selected = payloads[category] || payloads.dividends;

  await registration.showNotification(selected.title, {
    body: selected.body,
    icon: "/icons/icon-192.png",
    badge: "/icons/icon-192.png",
    tag: `tessera-test-${category}-${Date.now()}`,
    data: {
      deepLink: selected.deepLink,
      category,
      timestamp: Date.now(),
    },
    actions: [
      { action: "view", title: "View Details" },
      { action: "dismiss", title: "Dismiss" },
    ],
  });
}
