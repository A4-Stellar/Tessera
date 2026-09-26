const MAX_BODY_BYTES = 1_048_576;
const CACHE_FRESH_SECONDS = 30;
const CACHE_STALE_SECONDS = 60;
const CACHE_ERROR_SECONDS = 300;

const BLOCKED_USER_AGENTS = /(?:sqlmap|nikto|masscan|zgrab|nmap|acunetix|nessus|wpscan|gobuster|dirbuster|scrapy|crawler4j|bytespider|petalbot|ahrefsbot|semrushbot|mj12bot|dotbot|dataforseobot|<script)/i;
const SQL_INJECTION = /(?:\bunion\s+(?:all\s+)?select\b|\bselect\b[\s\S]{0,80}\bfrom\b|\binsert\s+into\b|\b(?:update|delete)\s+\w+[\s\S]{0,40}\bset\b|\bdrop\s+(?:table|database)\b|\b(or|and)\s+['"\w]+\s*=\s*['"\w]+|--\s|;\s*(?:select|insert|update|delete|drop)\b|\/\*|\bbenchmark\s*\()/i;
const ALLOWED_METHODS = new Set(["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]);

function jsonError(status, error, headers = {}) {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "content-type": "application/json; charset=utf-8", ...headers },
  });
}

function base64UrlToBytes(value) {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

function decodeJwtPart(value) {
  return JSON.parse(new TextDecoder().decode(base64UrlToBytes(value)));
}

export async function verifyJwt(token, secret, now = Math.floor(Date.now() / 1000), issuer, audience) {
  if (!secret || typeof token !== "string") return false;
  const parts = token.split(".");
  if (parts.length !== 3 || parts.some((part) => !part)) return false;

  try {
    const header = decodeJwtPart(parts[0]);
    const claims = decodeJwtPart(parts[1]);
    if (header.alg !== "HS256" || typeof claims.exp !== "number" || claims.exp <= now) return false;
    if (claims.nbf !== undefined && (typeof claims.nbf !== "number" || claims.nbf > now)) return false;
    if (issuer && claims.iss !== issuer) return false;
    if (audience && !(claims.aud === audience || (Array.isArray(claims.aud) && claims.aud.includes(audience)))) return false;

    const key = await crypto.subtle.importKey(
      "raw",
      new TextEncoder().encode(secret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["verify"],
    );
    return await crypto.subtle.verify(
      "HMAC",
      key,
      base64UrlToBytes(parts[2]),
      new TextEncoder().encode(`${parts[0]}.${parts[1]}`),
    );
  } catch {
    return false;
  }
}

function isCacheablePath(pathname) {
  return pathname === "/stats" || pathname === "/assets" || pathname === "/v1/stats" || pathname === "/v1/assets";
}

function getIp(request) {
  return request.headers.get("cf-connecting-ip") || "unknown";
}

function cacheKey(request) {
  const url = new URL(request.url);
  return new Request(url.toString(), { method: "GET" });
}

function responseAge(response) {
  const fetchedAt = Number(response.headers.get("x-edge-fetched-at"));
  return Number.isFinite(fetchedAt) && fetchedAt > 0 ? (Date.now() - fetchedAt) / 1000 : Infinity;
}

async function readBodyWithinLimit(request) {
  if (!request.body) return "";
  const reader = request.clone().body.getReader();
  const chunks = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_BODY_BYTES) {
      reader.cancel().catch(() => undefined);
      return null;
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

function withCacheHeaders(response, fetchedAt = Date.now(), storage = false) {
  const headers = new Headers(response.headers);
  headers.set("cache-control", storage
    ? `public, max-age=${CACHE_ERROR_SECONDS}`
    : `public, max-age=${CACHE_FRESH_SECONDS}, s-maxage=${CACHE_FRESH_SECONDS}, stale-while-revalidate=${CACHE_STALE_SECONDS}, stale-if-error=${CACHE_ERROR_SECONDS}`);
  headers.set("x-edge-fetched-at", String(fetchedAt));
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

function asClientCacheResponse(response) {
  const headers = new Headers(response.headers);
  headers.set("cache-control", `public, max-age=${CACHE_FRESH_SECONDS}, s-maxage=${CACHE_FRESH_SECONDS}, stale-while-revalidate=${CACHE_STALE_SECONDS}, stale-if-error=${CACHE_ERROR_SECONDS}`);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

async function fetchAndCache(request, env, cache, key) {
  const originUrl = new URL(request.url);
  originUrl.hostname = new URL(env.API_ORIGIN).hostname;
  originUrl.protocol = new URL(env.API_ORIGIN).protocol;
  originUrl.port = new URL(env.API_ORIGIN).port;
  originUrl.username = "";
  originUrl.password = "";
  const originRequest = new Request(originUrl, request);
  const response = await fetch(originRequest);
  if (response.status === 200 && response.headers.get("content-type")?.includes("application/json") && !response.headers.has("set-cookie")) {
    const fetchedAt = Date.now();
    const cachedResponse = withCacheHeaders(response.clone(), fetchedAt, true);
    await cache.put(key, cachedResponse.clone());
    return withCacheHeaders(response, fetchedAt);
  }
  return response;
}

async function cachedFetch(request, env, context) {
  const cache = caches.default;
  const key = cacheKey(request);
  const cached = await cache.match(key);
  if (cached) {
    const age = responseAge(cached);
    if (age <= CACHE_FRESH_SECONDS) return asClientCacheResponse(cached);
    if (age <= CACHE_FRESH_SECONDS + CACHE_STALE_SECONDS) {
      context.waitUntil(fetchAndCache(request, env, cache, key).catch(() => undefined));
      return asClientCacheResponse(cached);
    }
    try {
      return await fetchAndCache(request, env, cache, key);
    } catch (error) {
      if (age <= CACHE_ERROR_SECONDS) return asClientCacheResponse(cached);
      throw error;
    }
  }
  return fetchAndCache(request, env, cache, key);
}

async function validateRequest(request, env) {
  const url = new URL(request.url);
  const suppliedHost = request.headers.get("host") || url.host;
  if (!suppliedHost || !ALLOWED_METHODS.has(request.method)) return jsonError(400, "invalid_request");
  const allowedHosts = env.ALLOWED_HOSTS?.split(",").map((host) => host.trim().toLowerCase()).filter(Boolean);
  if (allowedHosts?.length && !allowedHosts.includes(suppliedHost.toLowerCase())) return jsonError(421, "invalid_host");

  const userAgent = request.headers.get("user-agent") || "";
  if (!userAgent || BLOCKED_USER_AGENTS.test(userAgent)) return jsonError(403, "blocked_user_agent");
  const contentLength = Number(request.headers.get("content-length") || 0);
  if (!Number.isFinite(contentLength) || contentLength < 0 || contentLength > MAX_BODY_BYTES) return jsonError(413, "request_body_too_large");

  const bearer = request.headers.get("authorization");
  if (bearer !== null) {
    const match = /^Bearer ([A-Za-z0-9._~-]+)$/.exec(bearer);
    if (!match || !(await verifyJwt(match[1], env.JWT_SECRET, Math.floor(Date.now() / 1000), env.JWT_ISSUER, env.JWT_AUDIENCE))) {
      return jsonError(401, "invalid_token", { "www-authenticate": "Bearer" });
    }
  }

  let decodedPath;
  try {
    decodedPath = decodeURIComponent(url.pathname);
  } catch {
    return jsonError(400, "invalid_request");
  }
  let inspected = `${decodedPath} ${[...url.searchParams.entries()].flat().join(" ")}`;
  if (contentLength > 0 || !["GET", "HEAD"].includes(request.method)) {
    const body = await readBodyWithinLimit(request);
    if (body === null) return jsonError(413, "request_body_too_large");
    inspected += ` ${body}`;
  }
  if (SQL_INJECTION.test(inspected)) return jsonError(400, "malformed_request");
  return null;
}

export default {
  async fetch(request, env, context) {
    if (!env.API_ORIGIN) return jsonError(503, "origin_not_configured");

    const validationError = await validateRequest(request, env);
    if (validationError) return validationError;

    if (!env.IP_RATE_LIMIT) return jsonError(503, "rate_limit_not_configured");
    const limit = await env.IP_RATE_LIMIT.limit({ key: getIp(request) });
    if (!limit.success) return jsonError(429, "rate_limited", { "retry-after": "60" });

    const url = new URL(request.url);
    const cacheable = request.method === "GET" && isCacheablePath(url.pathname)
      && !request.headers.has("authorization") && !request.headers.has("cookie");
    try {
      if (cacheable) return await cachedFetch(request, env, context);
      const originUrl = new URL(request.url);
      const apiOrigin = new URL(env.API_ORIGIN);
      originUrl.protocol = apiOrigin.protocol;
      originUrl.hostname = apiOrigin.hostname;
      originUrl.port = apiOrigin.port;
      originUrl.username = "";
      originUrl.password = "";
      return await fetch(new Request(originUrl, request));
    } catch {
      return jsonError(502, "origin_unavailable");
    }
  },
};
