/* Tessera Docs service worker (issue #130).
 *
 * Caching strategies:
 * - Docs pages & static assets (`/_next/static`, images, fonts): stale-while-revalidate.
 * - API routes (`/api/*`): network-first with cached fallback.
 * - Navigations: stale-while-revalidate with an offline fallback to the cached app shell.
 */

const VERSION = "tessera-docs-v1";
const DOCS_CACHE = `${VERSION}-docs`;
const API_CACHE = `${VERSION}-api`;
const PRECACHE_URLS = ["/", "/manifest.webmanifest"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(DOCS_CACHE)
      .then((cache) => cache.addAll(PRECACHE_URLS))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys.filter((key) => key !== DOCS_CACHE && key !== API_CACHE).map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

function isApiRequest(url) {
  return url.pathname.startsWith("/api/");
}

function isStaticAsset(url) {
  return (
    url.pathname.startsWith("/_next/static/") ||
    /\.(?:js|css|png|jpg|jpeg|svg|gif|webp|avif|ico|woff2?)$/.test(url.pathname)
  );
}

/** Stale-while-revalidate: serve cache immediately, refresh in background. */
async function staleWhileRevalidate(request, cacheName) {
  const cache = await caches.open(cacheName);
  const cached = await cache.match(request);
  const refresh = fetch(request)
    .then((response) => {
      if (response && response.ok) cache.put(request, response.clone());
      return response;
    })
    .catch(() => cached);
  return cached || refresh;
}

/** Network-first: try the network, fall back to cache (offline reading). */
async function networkFirst(request, cacheName) {
  const cache = await caches.open(cacheName);
  try {
    const response = await fetch(request);
    if (response && response.ok) cache.put(request, response.clone());
    return response;
  } catch (error) {
    const cached = await cache.match(request);
    if (cached) return cached;
    throw error;
  }
}

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (isApiRequest(url)) {
    event.respondWith(networkFirst(request, API_CACHE));
  } else if (request.mode === "navigate") {
    event.respondWith(
      staleWhileRevalidate(request, DOCS_CACHE).catch(() => caches.match("/")),
    );
  } else if (isStaticAsset(url)) {
    event.respondWith(staleWhileRevalidate(request, DOCS_CACHE));
  }
});
