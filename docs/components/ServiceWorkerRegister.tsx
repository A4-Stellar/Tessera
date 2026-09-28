"use client";

import { useEffect } from "react";

/**
 * Registers the docs service worker (`/sw.js`) for offline reading.
 * Registration is a no-op on browsers without service-worker support and
 * failures are swallowed so docs always remain usable without a worker.
 */
export function ServiceWorkerRegister() {
  useEffect(() => {
    if (!("serviceWorker" in navigator)) return;
    const register = async () => {
      try {
        await navigator.serviceWorker.register("/sw.js", { scope: "/" });
      } catch {
        // Offline support is progressive enhancement; ignore failures.
      }
    };
    register();
  }, []);

  return null;
}

export default ServiceWorkerRegister;
