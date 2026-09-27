/**
 * Tessera edge shield — Cloudflare Worker (module syntax).
 *
 * Runs in front of the Tessera REST API to stop abusive traffic before it
 * reaches the origin. On every request the worker:
 *
 *   1. validates the HTTP method and request headers,
 *   2. rejects SQL-injection / path-traversal / scanner traffic heuristically,
 *   3. verifies JWT signatures at the edge (HMAC via `JWT_SECRET`, or
 *      RSA/ECDSA via a `JWT_JWKS_URL` JWKS document),
 *   4. rate limits per client IP with a sliding-window counter kept in the
 *      `RATE_LIMIT` Workers KV namespace,
 *   5. serves `GET /stats` and `GET /assets` from the edge Cache API with a
 *      stale-while-revalidate policy, and
 *   6. proxies every other request to `ORIGIN_URL`.
 *
 * The module deliberately avoids Node-only APIs so the same file runs both in
 * the Workers runtime and under `node --test` (see `test/worker.test.js`).
 * Runtime facilities (`fetch`, `crypto`, `caches`) are resolved through the
 * optional `deps` argument so behaviour is unit-testable without a Workers
 * runtime; in production `deps` is omitted and the platform globals are used.
 */

/* ------------------------------------------------------------------------- *
 * Defaults
 * ------------------------------------------------------------------------- */

/** Paths (default-branch ``/stats`` aliases included) that get edge caching. */
export const DEFAULT_CACHE_PATHS = ['/stats', '/assets', '/v1/stats', '/v1/assets'];

/** Methods the edge forwards. Everything else is rejected with `405`. */
export const DEFAULT_ALLOWED_METHODS = ['GET', 'HEAD', 'POST', 'OPTIONS'];

/**
 * Headers that only exist to bypass an edge/CDN or smuggle a second request.
 * They are rejected outright rather than forwarded.
 */
export const DEFAULT_BLOCKED_HEADERS = [
  'x-http-method-override',
  'x-method-override',
  'x-original-url',
  'x-rewrite-url',
  'x-forwarded-host',
  'x-forwarded-prefix',
  'x-forwarded-server',
  'proxy',
  'proxy-connection',
];

/**
 * Header allow-list: the only request headers copied onto the origin request.
 * Everything else (cookies are kept, since the API may need them) is dropped
 * before proxying so the origin never sees attacker-controlled hop-by-hop data.
 */
export const DEFAULT_FORWARDED_HEADERS = [
  'accept',
  'accept-encoding',
  'accept-language',
  'authorization',
  'cache-control',
  'content-type',
  'cookie',
  'etag',
  'if-match',
  'if-modified-since',
  'if-none-match',
  'origin',
  'pragma',
  'referer',
  'user-agent',
  'x-api-key',
  'x-request-id',
  'x-tessera-client',
  'x-tessera-request-id',
  'cf-connecting-ip',
  'cf-ipcountry',
];

/**
 * Well-known offensive-security scanners. These product names have no
 * legitimate reason to call the API, so a User-Agent match is blocked.
 */
export const DEFAULT_BLOCKED_BOTS = [
  'sqlmap',
  'nikto',
  'nmap',
  'masscan',
  'acunetix',
  'zgrab',
  'gobuster',
  'dirbuster',
  'dirb',
  'nuclei',
  'wfuzz',
  'commix',
  'wpscan',
  'hydra',
  'jaeles',
];

/** Algorithms the edge is willing to verify. `none` is never accepted. */
export const JWT_ALGORITHMS = Object.freeze({
  HS256: { family: 'hmac', importParams: { name: 'HMAC', hash: 'SHA-256' }, verifyParams: { name: 'HMAC' } },
  HS384: { family: 'hmac', importParams: { name: 'HMAC', hash: 'SHA-384' }, verifyParams: { name: 'HMAC' } },
  HS512: { family: 'hmac', importParams: { name: 'HMAC', hash: 'SHA-512' }, verifyParams: { name: 'HMAC' } },
  RS256: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' },
    verifyParams: { name: 'RSASSA-PKCS1-v1_5' },
  },
  RS384: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-384' },
    verifyParams: { name: 'RSASSA-PKCS1-v1_5' },
  },
  RS512: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-512' },
    verifyParams: { name: 'RSASSA-PKCS1-v1_5' },
  },
  PS256: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSA-PSS', hash: 'SHA-256' },
    verifyParams: { name: 'RSA-PSS', saltLength: 32 },
  },
  PS384: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSA-PSS', hash: 'SHA-384' },
    verifyParams: { name: 'RSA-PSS', saltLength: 48 },
  },
  PS512: {
    family: 'rsa',
    kty: 'RSA',
    importParams: { name: 'RSA-PSS', hash: 'SHA-512' },
    verifyParams: { name: 'RSA-PSS', saltLength: 64 },
  },
  ES256: {
    family: 'ec',
    kty: 'EC',
    importParams: { name: 'ECDSA', namedCurve: 'P-256' },
    verifyParams: { name: 'ECDSA', hash: 'SHA-256' },
  },
  ES384: {
    family: 'ec',
    kty: 'EC',
    importParams: { name: 'ECDSA', namedCurve: 'P-384' },
    verifyParams: { name: 'ECDSA', hash: 'SHA-384' },
  },
  ES512: {
    family: 'ec',
    kty: 'EC',
    importParams: { name: 'ECDSA', namedCurve: 'P-521' },
    verifyParams: { name: 'ECDSA', hash: 'SHA-512' },
  },
});

/**
 * Heuristics applied to the decoded path and query string. Each entry is a
 * named pattern so blocked requests can be attributed in logs/metrics.
 */
export const INJECTION_PATTERNS = Object.freeze([
  { name: 'null_byte', pattern: /\u0000/ },
  { name: 'sql_union_select', pattern: /\bunion\b[\s\S]{0,24}\bselect\b/ },
  {
    name: 'sql_statement',
    pattern:
      /\b(select|insert|update|delete|drop|alter|truncate|create)\b[\s\S]{0,48}\b(from|into|table|database|users|information_schema)\b/,
  },
  { name: 'sql_or_tautology', pattern: /\bor\b\s+['"]?\w+['"]?\s*=\s*['"]?\w+['"]?/ },
  { name: 'sql_comment', pattern: /(--|#)\s*$/ },
  { name: 'sql_block_comment', pattern: /\/\*/ },
  { name: 'sql_time_delay', pattern: /\b(sleep|benchmark|pg_sleep|waitfor)\s*\(/ },
  { name: 'sql_system_catalog', pattern: /\b(information_schema|sys\.tables|xp_cmdshell)\b/ },
  { name: 'path_traversal', pattern: /(\.\.[\\/]|[/\\]etc[/\\](passwd|shadow))/ },
  { name: 'xss_probe', pattern: /(<script\b|onerror\s*=|onload\s*=|javascript:)/ },
  { name: 'shell_injection', pattern: /[;&|`$]\s*(cat|curl|wget|nc|bash|sh|python)\b/ },
]);

/** Health/liveness probes are exempt from the empty-User-Agent rule. */
const HEALTH_PATHS = new Set(['/health', '/version', '/']);

/* ------------------------------------------------------------------------- *
 * Tiny env parsers + configuration resolution
 * ------------------------------------------------------------------------- */

function envString(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function envInt(value, fallback) {
  const parsed = Number.parseInt(envString(value), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function envBool(value, fallback) {
  const text = envString(value).toLowerCase();
  if (['true', '1', 'yes', 'on'].includes(text)) return true;
  if (['false', '0', 'no', 'off'].includes(text)) return false;
  return fallback;
}

function envList(value, fallback) {
  const text = envString(value);
  if (!text) return [...fallback];
  return text
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean);
}

/**
 * Resolve the effective worker configuration from the Worker `env` bindings.
 * Every knob is optional apart from `ORIGIN_URL`; sane production defaults are
 * applied otherwise.
 */
export function resolveConfig(env = {}) {
  const bindings = env && typeof env === 'object' ? env : {};
  return {
    originUrl: envString(bindings.ORIGIN_URL),
    allowedMethods: envList(bindings.ALLOWED_METHODS, DEFAULT_ALLOWED_METHODS).map((m) => m.toUpperCase()),
    requiredHeaders: envList(bindings.REQUIRED_HEADERS, []).map((h) => h.toLowerCase()),
    blockedHeaders: DEFAULT_BLOCKED_HEADERS,
    forwardedHeaders: DEFAULT_FORWARDED_HEADERS,
    maxHeaderCount: envInt(bindings.MAX_HEADER_COUNT, 100),
    maxHeaderBytes: envInt(bindings.MAX_HEADER_BYTES, 8192),
    maxUrlLength: envInt(bindings.MAX_URL_LENGTH, 2048),
    botProtection: envBool(bindings.BOT_PROTECTION, true),
    blockedBots: envList(bindings.BLOCKED_BOTS, DEFAULT_BLOCKED_BOTS).map((bot) => bot.toLowerCase()),
    rateLimitMax: envInt(bindings.RATE_LIMIT_MAX, 300),
    rateLimitWindowSeconds: envInt(bindings.RATE_LIMIT_WINDOW_SECONDS, 60),
    rateLimitFailOpen: envBool(bindings.RATE_LIMIT_FAIL_OPEN, true),
    cachePaths: envList(bindings.CACHE_PATHS, DEFAULT_CACHE_PATHS),
    cacheTtlSeconds: envInt(bindings.CACHE_TTL_SECONDS, 30),
    cacheStaleSeconds: envInt(bindings.CACHE_STALE_SECONDS, 120),
    cacheMaxBytes: envInt(bindings.CACHE_MAX_BYTES, 524288),
    jwtAlgorithms: envList(bindings.JWT_ALGORITHMS, ['RS256', 'ES256', 'HS256']).map((alg) => alg.toUpperCase()),
    jwtSecret: envString(bindings.JWT_SECRET),
    jwtJwksUrl: envString(bindings.JWT_JWKS_URL),
    jwtIssuer: envString(bindings.JWT_ISSUER),
    jwtAudience: envString(bindings.JWT_AUDIENCE),
    jwtRequiredPaths: envList(bindings.JWT_REQUIRED_PATHS, []),
    jwtClockSkewSeconds: envInt(bindings.JWT_CLOCK_SKEW_SECONDS, 30),
    jwtRequireExp: envBool(bindings.JWT_REQUIRE_EXP, true),
    jwksTtlSeconds: envInt(bindings.JWKS_TTL_SECONDS, 300),
  };
}

/* ------------------------------------------------------------------------- *
 * Small response helpers
 * ------------------------------------------------------------------------- */

function jsonResponse(status, payload, headers = {}) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { 'content-type': 'application/json; charset=utf-8', ...headers },
  });
}

/** Copy a response, replacing/adding the supplied headers. */
function withHeaders(response, headers) {
  const merged = new Headers(response.headers);
  for (const [name, value] of Object.entries(headers)) merged.set(name, value);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: merged,
  });
}

/* ------------------------------------------------------------------------- *
 * Request validation
 * ------------------------------------------------------------------------- */

/** Client IP as seen by Cloudflare, falling back to conventional proxy headers. */
export function resolveClientIp(request) {
  const candidates = ['cf-connecting-ip', 'x-real-ip', 'x-forwarded-for'];
  for (const header of candidates) {
    const value = request.headers.get(header);
    if (value) {
      const first = value.split(',')[0].trim();
      if (first) return first;
    }
  }
  return 'unknown';
}

/**
 * Validate the method and headers of an inbound request.
 *
 * @returns {{ok: true} | {ok: false, status: number, error: string, message: string}}
 */
export function validateHeaders(request, config) {
  const method = (request.method || 'GET').toUpperCase();
  if (!config.allowedMethods.includes(method)) {
    return {
      ok: false,
      status: 405,
      error: 'method_not_allowed',
      message: `Method ${method} is not accepted by the edge shield.`,
    };
  }

  let headerCount = 0;
  for (const [name, value] of request.headers) {
    headerCount += 1;
    const lower = name.toLowerCase();
    if (config.blockedHeaders.includes(lower)) {
      return {
        ok: false,
        status: 400,
        error: 'blocked_header',
        message: `Request header "${name}" is not permitted.`,
      };
    }
    if (value.length > config.maxHeaderBytes) {
      return {
        ok: false,
        status: 431,
        error: 'header_too_large',
        message: `Request header "${name}" exceeds the ${config.maxHeaderBytes}-byte limit.`,
      };
    }
  }

  if (headerCount > config.maxHeaderCount) {
    return {
      ok: false,
      status: 431,
      error: 'too_many_headers',
      message: `Request carries ${headerCount} headers; the edge accepts at most ${config.maxHeaderCount}.`,
    };
  }

  for (const required of config.requiredHeaders) {
    if (!request.headers.has(required)) {
      return {
        ok: false,
        status: 400,
        error: 'missing_header',
        message: `Required request header "${required}" is missing.`,
      };
    }
  }

  return { ok: true };
}

/**
 * Build the header set forwarded to the origin: only allow-listed headers are
 * copied, and the client's forwarding/gateway headers are replaced with the
 * address Cloudflare actually observed so logs cannot be spoofed.
 */
export function buildOriginHeaders(request, config, clientIp) {
  const forwarded = new Headers();
  for (const [name, value] of request.headers) {
    const lower = name.toLowerCase();
    if (config.forwardedHeaders.includes(lower) && !config.blockedHeaders.includes(lower)) {
      forwarded.set(lower, value);
    }
  }
  forwarded.delete('x-forwarded-for');
  forwarded.delete('x-real-ip');
  if (clientIp && clientIp !== 'unknown') {
    forwarded.set('x-forwarded-for', clientIp);
    forwarded.set('x-real-ip', clientIp);
  }
  forwarded.set('x-tessera-edge', 'tessera-edge-shield');
  return forwarded;
}

/* ------------------------------------------------------------------------- *
 * Injection & bot heuristics
 * ------------------------------------------------------------------------- */

/**
 * Scan a raw URL component for injection signatures. The value is URL-decoded
 * first so `%27%20OR%201%3D1` is caught just like the plain-text form.
 *
 * @returns {string|null} the matching rule name, or `null` when clean.
 */
export function detectInjection(rawValue) {
  if (!rawValue) return null;
  let value = String(rawValue);
  try {
    value = decodeURIComponent(value.replace(/\+/g, ' '));
  } catch {
    // Malformed percent-encoding is itself suspicious; scan the raw value.
  }
  const lower = value.toLowerCase();
  for (const rule of INJECTION_PATTERNS) {
    if (rule.pattern.test(lower)) return rule.name;
  }
  return null;
}

/** True when the User-Agent matches a known offensive-security scanner. */
export function isBlockedUserAgent(userAgent, config) {
  const ua = (userAgent || '').toLowerCase();
  if (!ua) return false;
  return config.blockedBots.some((signature) => signature && ua.includes(signature));
}

/* ------------------------------------------------------------------------- *
 * Rate limiting (sliding-window counter in Workers KV)
 * ------------------------------------------------------------------------- */

/**
 * Weighted sliding-window estimate: the previous window's traffic is decayed
 * linearly as the current window elapses, which removes the double-burst flaw
 * of a naive fixed window while still needing only one KV key per client.
 */
export function slidingWindowEstimate(previous, current, elapsedRatio) {
  const ratio = Math.min(1, Math.max(0, Number.isFinite(elapsedRatio) ? elapsedRatio : 1));
  return previous * (1 - ratio) + current;
}

/**
 * Increment and evaluate the per-IP counter stored at `key`.
 *
 * The KV binding is eventually consistent, so this is a best-effort edge
 * shield; the account-level Cloudflare Rate Limiting rules in
 * `infrastructure/terraform/modules/cloudflare` remain the global backstop.
 *
 * @returns {Promise<{allowed: boolean, limit: number, remaining: number,
 *   reset: number, retryAfter?: number, degraded?: boolean}>}
 */
export async function checkRateLimit(kv, key, config, now) {
  const limit = config.rateLimitMax;
  const windowSeconds = config.rateLimitWindowSeconds;
  const windowMs = windowSeconds * 1000;
  const windowStart = Math.floor(now / windowMs) * windowMs;
  const reset = (windowStart + windowMs) / 1000;

  const unavailable = () => {
    if (config.rateLimitFailOpen) {
      return { allowed: true, limit, remaining: limit, reset, degraded: true };
    }
    return { allowed: false, limit, remaining: 0, reset, retryAfter: windowSeconds, degraded: true };
  };

  if (!kv || typeof kv.get !== 'function') return unavailable();

  let state = null;
  try {
    state = await kv.get(key, 'json');
  } catch {
    return unavailable();
  }

  if (!state || typeof state.current !== 'number' || state.window !== windowStart) {
    const previous = state && state.window === windowStart - windowMs ? state.current : 0;
    state = { window: windowStart, current: 0, previous };
  }

  const elapsedRatio = (now - windowStart) / windowMs;
  const before = slidingWindowEstimate(state.previous, state.current, elapsedRatio);
  if (before >= limit) {
    const retryAfter = Math.max(1, Math.ceil((windowStart + windowMs - now) / 1000));
    return { allowed: false, limit, remaining: 0, reset, retryAfter };
  }

  state.current += 1;
  try {
    await kv.put(key, JSON.stringify(state), { expirationTtl: Math.max(60, windowSeconds * 2) });
  } catch {
    // A KV write failure must not take the API down; the counter simply
    // under-counts until the next successful write.
  }

  const after = slidingWindowEstimate(state.previous, state.current, elapsedRatio);
  return { allowed: true, limit, remaining: Math.max(0, Math.floor(limit - after)), reset };
}

/* ------------------------------------------------------------------------- *
 * JWT verification
 * ------------------------------------------------------------------------- */

/** Error carrying a stable machine-readable reason for JWT failures. */
export class JwtError extends Error {
  constructor(reason, message) {
    super(message);
    this.name = 'JwtError';
    this.reason = reason;
  }
}

/** Module-level JWKS cache shared across requests in the same isolate. */
const jwksCache = new Map();

function base64UrlToBytes(segment) {
  const normalized = String(segment).replace(/-/g, '+').replace(/_/g, '/');
  const padded = normalized + '='.repeat((4 - (normalized.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** Decode a base64url JWT segment into its JSON object. */
export function decodeJwtSegment(segment) {
  const json = new TextDecoder().decode(base64UrlToBytes(segment));
  return JSON.parse(json);
}

/** Extract the bearer token from the `Authorization` header, if present. */
export function getBearerToken(request) {
  const header = request.headers.get('authorization');
  if (!header) return null;
  const match = /^Bearer\s+(.+)$/i.exec(header.trim());
  return match ? match[1].trim() : null;
}

/** Glob-ish path matcher: a trailing `*` means prefix, otherwise exact. */
export function isPathRequired(pathname, patterns) {
  return patterns.some((pattern) => {
    if (!pattern) return false;
    if (pattern.endsWith('*')) return pathname.startsWith(pattern.slice(0, -1));
    return pathname === pattern;
  });
}

async function fetchJwks(config, deps) {
  if (!config.jwtJwksUrl) {
    throw new JwtError('jwt_not_configured', 'JWT_JWKS_URL is required to verify asymmetric tokens.');
  }
  const cache = deps.jwksCache || jwksCache;
  const now = deps.now ?? Date.now();
  const cached = cache.get(config.jwtJwksUrl);
  if (cached && cached.expiresAt > now) return cached.keys;

  const fetchImpl = deps.fetch || globalThis.fetch;
  let response;
  try {
    response = await fetchImpl(new Request(config.jwtJwksUrl, { headers: { accept: 'application/json' } }));
  } catch (error) {
    throw new JwtError('jwks_unavailable', `JWKS fetch failed: ${error.message}`);
  }
  if (!response.ok) {
    throw new JwtError('jwks_unavailable', `JWKS endpoint answered ${response.status}.`);
  }
  const body = await response.json();
  const keys = Array.isArray(body && body.keys) ? body.keys : [];
  if (keys.length === 0) {
    throw new JwtError('jwks_unavailable', 'JWKS document contains no signing keys.');
  }
  cache.set(config.jwtJwksUrl, { keys, expiresAt: now + config.jwksTtlSeconds * 1000 });
  return keys;
}

function selectJwk(keys, header, spec) {
  const matches = keys.filter((key) => {
    if (!key || typeof key !== 'object') return false;
    if (spec.kty && key.kty !== spec.kty) return false;
    if (header.kid && key.kid !== header.kid) return false;
    if (key.use && key.use !== 'sig') return false;
    if (key.alg && String(key.alg).toUpperCase() !== String(header.alg).toUpperCase()) return false;
    return true;
  });
  return matches.length > 0 ? matches[0] : null;
}

/**
 * Validate registered claims (`exp`, `nbf`, `iss`, `aud`) with a clock skew
 * allowance. Throws a `JwtError` on the first failure.
 */
export function validateJwtClaims(payload, config, nowMs) {
  const nowSeconds = Math.floor(nowMs / 1000);
  const skew = config.jwtClockSkewSeconds;

  if (config.jwtRequireExp && typeof payload.exp !== 'number') {
    throw new JwtError('invalid_claims', 'JWT is missing the required exp claim.');
  }
  if (typeof payload.exp === 'number' && payload.exp + skew < nowSeconds) {
    throw new JwtError('token_expired', 'JWT has expired.');
  }
  if (typeof payload.nbf === 'number' && payload.nbf - skew > nowSeconds) {
    throw new JwtError('token_not_yet_valid', 'JWT is not valid yet.');
  }
  if (config.jwtIssuer && payload.iss !== config.jwtIssuer) {
    throw new JwtError('invalid_issuer', 'JWT issuer does not match the configured issuer.');
  }
  if (config.jwtAudience) {
    const audiences = Array.isArray(payload.aud) ? payload.aud : [payload.aud];
    if (!audiences.includes(config.jwtAudience)) {
      throw new JwtError('invalid_audience', 'JWT audience does not match the configured audience.');
    }
  }
  return true;
}

/**
 * Verify a compact JWS token's signature and claims. WebCrypto only — no
 * third-party dependency — with symmetric keys from `JWT_SECRET` and
 * asymmetric keys resolved from `JWT_JWKS_URL`.
 *
 * @returns {Promise<object>} the verified payload.
 * @throws {JwtError}
 */
export async function verifyJwt(token, config, deps = {}) {
  const parts = String(token).split('.');
  if (parts.length !== 3 || parts.some((part) => !part)) {
    throw new JwtError('malformed_token', 'JWT must have three non-empty dot-separated segments.');
  }
  const [headerPart, payloadPart, signaturePart] = parts;

  let header;
  let payload;
  try {
    header = decodeJwtSegment(headerPart);
    payload = decodeJwtSegment(payloadPart);
  } catch {
    throw new JwtError('malformed_token', 'JWT header or payload is not valid base64url-encoded JSON.');
  }

  if (typeof header.alg !== 'string' || header.alg.toLowerCase() === 'none') {
    throw new JwtError('insecure_algorithm', 'Unsigned JWTs (alg "none") are never accepted.');
  }
  const algorithm = header.alg.toUpperCase();
  if (!config.jwtAlgorithms.includes(algorithm)) {
    throw new JwtError('unsupported_algorithm', `JWT algorithm ${header.alg} is not in the allowed set.`);
  }
  const spec = JWT_ALGORITHMS[algorithm];
  if (!spec) {
    throw new JwtError('unsupported_algorithm', `JWT algorithm ${header.alg} is not supported.`);
  }

  const cryptoImpl = deps.crypto || globalThis.crypto;
  const encoding = new TextEncoder();
  const signingInput = encoding.encode(`${headerPart}.${payloadPart}`);
  const signature = base64UrlToBytes(signaturePart);

  let key;
  if (spec.family === 'hmac') {
    if (!config.jwtSecret) {
      throw new JwtError('jwt_not_configured', 'JWT_SECRET must be set to verify HS* tokens.');
    }
    key = await cryptoImpl.subtle.importKey('raw', encoding.encode(config.jwtSecret), spec.importParams, false, [
      'verify',
    ]);
  } else {
    const keys = await fetchJwks(config, deps);
    const jwk = selectJwk(keys, header, spec);
    if (!jwk) {
      throw new JwtError('unknown_key', header.kid ? `No JWKS key matches kid "${header.kid}".` : 'No JWKS key matches the token algorithm.');
    }
    key = await cryptoImpl.subtle.importKey('jwk', jwk, spec.importParams, false, ['verify']);
  }

  let verified = false;
  try {
    verified = await cryptoImpl.subtle.verify(spec.verifyParams, key, signature, signingInput);
  } catch {
    verified = false;
  }
  if (!verified) {
    throw new JwtError('invalid_signature', 'JWT signature verification failed.');
  }

  validateJwtClaims(payload, config, deps.now ?? Date.now());
  return payload;
}

/* ------------------------------------------------------------------------- *
 * Edge caching (Cache API + stale-while-revalidate)
 * ------------------------------------------------------------------------- */

/** Resolve the Cache API implementation, preferring an injected/bound cache. */
export function getCacheStorage(env = {}, deps = {}) {
  if (deps.cache !== undefined) return deps.cache;
  if (env && env.EDGE_CACHE && typeof env.EDGE_CACHE.match === 'function') return env.EDGE_CACHE;
  const globalCaches = globalThis.caches;
  if (globalCaches && globalCaches.default && typeof globalCaches.default.match === 'function') {
    return globalCaches.default;
  }
  return null;
}

/** Only unauthenticated `GET`s to the configured static paths are cacheable. */
export function isCacheableRequest(request, config) {
  if ((request.method || 'GET').toUpperCase() !== 'GET') return false;
  if (request.headers.has('authorization')) return false;
  const pathname = new URL(request.url).pathname;
  return config.cachePaths.includes(pathname);
}

function buildCacheKey(request) {
  const url = new URL(request.url);
  return new Request(`${url.origin}${url.pathname}${url.search}`, { method: 'GET' });
}

function isCacheableOriginResponse(response, config) {
  if (response.status !== 200) return false;
  const cacheControl = response.headers.get('cache-control') || '';
  if (/no-store/i.test(cacheControl)) return false;
  const contentLength = Number(response.headers.get('content-length') || 0);
  if (contentLength && contentLength > config.cacheMaxBytes) return false;
  return true;
}

async function bufferResponse(response) {
  const body = await response.arrayBuffer();
  const headers = new Headers(response.headers);
  return {
    body,
    headers,
    response: new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers,
    }),
  };
}

function withCacheMarker(response, marker) {
  const headers = new Headers(response.headers);
  headers.set('x-edge-cache', marker);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

async function materializeCached(cached, marker, ageSeconds) {
  const body = await cached.arrayBuffer();
  const headers = new Headers(cached.headers);
  headers.delete('x-edge-stored-at');
  headers.set('x-edge-cache', marker);
  if (Number.isFinite(ageSeconds)) headers.set('age', String(Math.max(0, Math.floor(ageSeconds))));
  return new Response(body, {
    status: cached.status,
    statusText: cached.statusText,
    headers,
  });
}

function maybeNotModified(request, response) {
  const etag = response.headers.get('etag');
  const ifNoneMatch = request.headers.get('if-none-match');
  if (!etag || !ifNoneMatch) return response;
  const candidates = ifNoneMatch.split(',').map((value) => value.trim());
  if (!candidates.includes('*') && !candidates.includes(etag)) return response;
  const headers = new Headers();
  headers.set('etag', etag);
  headers.set('x-edge-cache', 'HIT');
  const cacheControl = response.headers.get('cache-control');
  if (cacheControl) headers.set('cache-control', cacheControl);
  return new Response(null, { status: 304, headers });
}

async function fetchAndStore(request, env, config, deps, cache, cacheKey) {
  const originResponse = await fetchFromOrigin(request, env, config, deps);
  if (!cache || !isCacheableOriginResponse(originResponse, config)) return originResponse;

  let buffered;
  try {
    buffered = await bufferResponse(originResponse.clone());
  } catch {
    return originResponse;
  }

  const cacheHeaders = new Headers(buffered.headers);
  cacheHeaders.set('x-edge-stored-at', String(deps.now));
  try {
    await cache.put(
      cacheKey,
      new Response(buffered.body, {
        status: originResponse.status,
        statusText: originResponse.statusText,
        headers: cacheHeaders,
      })
    );
  } catch {
    // Cache writes are best effort; a miss only costs an extra origin call.
  }
  return buffered.response;
}

async function revalidateCache(request, env, config, deps, cache, cacheKey) {
  try {
    await fetchAndStore(request, env, config, deps, cache, cacheKey);
  } catch (error) {
    if (deps.logger && typeof deps.logger.warn === 'function') {
      deps.logger.warn(`[edge-shield] stale-while-revalidate failed: ${error.message}`);
    }
  }
}

async function handleCacheableRequest(request, env, ctx, config, deps) {
  const cache = getCacheStorage(env, deps);
  const cacheKey = buildCacheKey(request);
  const freshSeconds = config.cacheTtlSeconds;
  const staleSeconds = config.cacheTtlSeconds + config.cacheStaleSeconds;

  let cached = null;
  if (cache) {
    try {
      cached = await cache.match(cacheKey);
    } catch {
      cached = null;
    }
  }

  if (cached) {
    const storedAt = Number(cached.headers.get('x-edge-stored-at')) || 0;
    const ageSeconds = storedAt > 0 ? Math.max(0, (deps.now - storedAt) / 1000) : Number.POSITIVE_INFINITY;

    if (ageSeconds <= freshSeconds) {
      return maybeNotModified(request, await materializeCached(cached, 'HIT', ageSeconds));
    }
    if (ageSeconds <= staleSeconds) {
      const revalidation = revalidateCache(request, env, config, deps, cache, cacheKey);
      if (ctx && typeof ctx.waitUntil === 'function') ctx.waitUntil(revalidation);
      else await revalidation;
      return materializeCached(cached, 'STALE', ageSeconds);
    }
  }

  return withCacheMarker(await fetchAndStore(request, env, config, deps, cache, cacheKey), 'MISS');
}

/* ------------------------------------------------------------------------- *
 * Origin proxying
 * ------------------------------------------------------------------------- */

async function fetchFromOrigin(request, env, config, deps) {
  if (!config.originUrl) {
    return jsonResponse(500, {
      error: 'edge_misconfigured',
      message: 'ORIGIN_URL is not configured for the edge shield worker.',
    });
  }

  const url = new URL(request.url);
  let target;
  try {
    target = new URL(`${url.pathname}${url.search}`, config.originUrl);
  } catch {
    return jsonResponse(500, {
      error: 'edge_misconfigured',
      message: `ORIGIN_URL "${config.originUrl}" is not a valid absolute URL.`,
    });
  }

  const clientIp = resolveClientIp(request);
  const init = {
    method: request.method,
    headers: buildOriginHeaders(request, config, clientIp),
    redirect: 'manual',
  };
  const method = (request.method || 'GET').toUpperCase();
  if (method !== 'GET' && method !== 'HEAD' && request.body) {
    init.body = request.body;
  }

  const fetchImpl = deps.fetch || globalThis.fetch;
  try {
    return await fetchImpl(new Request(target.toString(), init));
  } catch (error) {
    return jsonResponse(502, {
      error: 'bad_gateway',
      message: `Origin request failed: ${error.message}`,
    });
  }
}

/* ------------------------------------------------------------------------- *
 * Request handler
 * ------------------------------------------------------------------------- */

/**
 * Core request pipeline. Exported (and dependency-injectable) so the whole
 * flow can be exercised by `node --test` without a Workers runtime.
 */
export async function handleRequest(request, env = {}, ctx = {}, deps = {}) {
  const config = resolveConfig(env);
  const edgeDeps = { ...deps, now: deps.now ?? Date.now() };
  const requestId =
    deps.requestId ||
    (globalThis.crypto && typeof globalThis.crypto.randomUUID === 'function'
      ? globalThis.crypto.randomUUID()
      : `req-${edgeDeps.now}`);

  const respond = (response, extraHeaders = {}) =>
    withHeaders(response, {
      'x-request-id': requestId,
      'x-tessera-edge': 'tessera-edge-shield',
      ...extraHeaders,
    });

  // 1. Oversized URLs are never legitimate and waste parsing budget.
  if (request.url.length > config.maxUrlLength) {
    return respond(
      jsonResponse(414, {
        error: 'uri_too_long',
        message: `Request URL exceeds the ${config.maxUrlLength}-character edge limit.`,
      })
    );
  }

  // 2. Method + header validation.
  const headerCheck = validateHeaders(request, config);
  if (!headerCheck.ok) {
    return respond(jsonResponse(headerCheck.status, { error: headerCheck.error, message: headerCheck.message }));
  }

  const url = new URL(request.url);

  // 3. Injection and scanner heuristics.
  if (config.botProtection) {
    const injection = detectInjection(url.pathname) || detectInjection(url.search);
    if (injection) {
      return respond(
        jsonResponse(403, {
          error: 'blocked_request',
          message: `Request blocked by the edge shield (${injection}).`,
        })
      );
    }
    if (isBlockedUserAgent(request.headers.get('user-agent'), config)) {
      return respond(
        jsonResponse(403, {
          error: 'blocked_bot',
          message: 'Automated scanner traffic is not permitted.',
        })
      );
    }
  }

  // 4. JWT verification: verify whenever a bearer token is present, and
  //    require one for the configured protected paths.
  const token = getBearerToken(request);
  const tokenRequired = isPathRequired(url.pathname, config.jwtRequiredPaths);
  if (token) {
    try {
      await verifyJwt(token, config, edgeDeps);
    } catch (error) {
      const status = error instanceof JwtError ? 401 : 500;
      return respond(
        jsonResponse(status, {
          error: error instanceof JwtError ? error.reason : 'jwt_verification_failed',
          message: error.message,
        })
      );
    }
  } else if (tokenRequired) {
    return respond(
      jsonResponse(401, {
        error: 'missing_token',
        message: `A bearer token is required for ${url.pathname}.`,
      })
    );
  }

  // 5. Per-IP sliding-window rate limit.
  const clientIp = resolveClientIp(request);
  const rate = await checkRateLimit(env.RATE_LIMIT, `rl:${clientIp}`, config, edgeDeps.now);
  const rateHeaders = {
    'X-RateLimit-Limit': String(rate.limit),
    'X-RateLimit-Remaining': String(rate.remaining),
    'X-RateLimit-Reset': String(Math.floor(rate.reset)),
  };
  if (!rate.allowed) {
    return respond(
      jsonResponse(
        429,
        {
          error: 'rate_limited',
          message: `Too many requests from this address; retry after ${rate.retryAfter} seconds.`,
        },
        { 'Retry-After': String(rate.retryAfter) }
      ),
      rateHeaders
    );
  }

  // 6. Cache-first serving for the static read endpoints.
  if (isCacheableRequest(request, config)) {
    return respond(await handleCacheableRequest(request, env, ctx, config, edgeDeps), rateHeaders);
  }

  // 7. Proxy everything else.
  return respond(await fetchFromOrigin(request, env, config, edgeDeps), rateHeaders);
}

export default {
  /**
   * Cloudflare Workers module entry point.
   *
   * @param {Request} request
   * @param {Record<string, unknown>} env bindings (`RATE_LIMIT`, `ORIGIN_URL`, secrets…)
   * @param {{waitUntil?: (promise: Promise<unknown>) => void}} ctx
   */
  async fetch(request, env, ctx) {
    return handleRequest(request, env, ctx);
  },
};
