import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_CACHE_PATHS,
  DEFAULT_CACHE_S_MAXAGE_SECONDS,
  DEFAULT_KNOWN_QUERY_PARAMS,
  INVALIDATED_CONTRACTS_KV_KEY,
  JwtError,
  buildCacheKey,
  buildOriginHeaders,
  checkRateLimit,
  detectInjection,
  emitLogpushRecord,
  extractAssetId,
  getBearerToken,
  getInvalidatedContracts,
  handleRequest,
  isBlockedUserAgent,
  isCacheableRequest,
  isPathRequired,
  isRequestInvalidated,
  normalizeCacheUrl,
  pushInvalidatedContracts,
  resolveClientIp,
  resolveConfig,
  slidingWindowEstimate,
  validateHeaders,
  validateJwtClaims,
  verifyJwt,
} from '../worker.js';

/* ------------------------------------------------------------------------- *
 * Test doubles
 * ------------------------------------------------------------------------- */

class FakeKv {
  constructor() {
    this.store = new Map();
  }

  async get(key, type) {
    if (!this.store.has(key)) return null;
    const value = this.store.get(key);
    return type === 'json' ? JSON.parse(value) : value;
  }

  async put(key, value) {
    this.store.set(key, value);
  }
}

class FakeCache {
  constructor() {
    this.entries = new Map();
  }

  async match(request) {
    const key = typeof request === 'string' ? request : request.url;
    const entry = this.entries.get(key);
    return entry ? entry.clone() : undefined;
  }

  async put(request, response) {
    const key = typeof request === 'string' ? request : request.url;
    this.entries.set(key, response.clone());
  }

  async delete(request) {
    const key = typeof request === 'string' ? request : request.url;
    return this.entries.delete(key);
  }
}

function base64UrlEncodeBytes(bytes) {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function base64UrlEncodeString(value) {
  return base64UrlEncodeBytes(new TextEncoder().encode(value));
}

async function signJwt(payload, { alg, secret, privateKey, header = {} }) {
  const encodedHeader = base64UrlEncodeString(JSON.stringify({ alg, typ: 'JWT', ...header }));
  const encodedPayload = base64UrlEncodeString(JSON.stringify(payload));
  const signingInput = `${encodedHeader}.${encodedPayload}`;
  const data = new TextEncoder().encode(signingInput);

  let signature;
  if (alg.startsWith('HS')) {
    const key = await crypto.subtle.importKey(
      'raw',
      new TextEncoder().encode(secret),
      { name: 'HMAC', hash: `SHA-${alg.slice(2)}` },
      false,
      ['sign']
    );
    signature = await crypto.subtle.sign({ name: 'HMAC' }, key, data);
  } else if (alg.startsWith('RS')) {
    signature = await crypto.subtle.sign({ name: 'RSASSA-PKCS1-v1_5' }, privateKey, data);
  } else if (alg.startsWith('ES')) {
    signature = await crypto.subtle.sign({ name: 'ECDSA', hash: `SHA-${alg.slice(2)}` }, privateKey, data);
  } else {
    throw new Error(`unsupported test algorithm ${alg}`);
  }

  return `${signingInput}.${base64UrlEncodeBytes(new Uint8Array(signature))}`;
}

async function generateRsaKeyPair() {
  const pair = await crypto.subtle.generateKey(
    {
      name: 'RSASSA-PKCS1-v1_5',
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: 'SHA-256',
    },
    true,
    ['sign', 'verify']
  );
  const jwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
  return { pair, jwk: { ...jwk, kid: 'rsa-test-key', alg: 'RS256', use: 'sig' } };
}

async function generateEcKeyPair() {
  const pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
  const jwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
  return { pair, jwk: { ...jwk, kid: 'ec-test-key', alg: 'ES256', use: 'sig' } };
}

function edgeEnv(overrides = {}) {
  return {
    ORIGIN_URL: 'https://origin.tessera.xyz',
    RATE_LIMIT: new FakeKv(),
    CACHE_PATHS: '/stats,/assets,/v1/stats,/v1/assets',
    CACHE_TTL_SECONDS: '30',
    CACHE_STALE_SECONDS: '120',
    ...overrides,
  };
}

function originResponder(handler) {
  const calls = [];
  const fetchImpl = async (request) => {
    calls.push(request.url);
    if (handler) return handler(request);
    return new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { 'content-type': 'application/json', etag: '"snapshot-1"' },
    });
  };
  return { fetchImpl, calls };
}

/* ------------------------------------------------------------------------- *
 * Configuration
 * ------------------------------------------------------------------------- */

describe('resolveConfig', () => {
  test('applies production defaults when nothing is configured', () => {
    const config = resolveConfig({});
    assert.equal(config.rateLimitMax, 300);
    assert.equal(config.rateLimitWindowSeconds, 60);
    assert.deepEqual(config.cachePaths, DEFAULT_CACHE_PATHS);
    assert.ok(config.cachePaths.includes('/stats'));
    assert.ok(config.cachePaths.includes('/assets'));
    assert.equal(config.cacheTtlSeconds, 30);
    assert.equal(config.cacheStaleSeconds, 120);
    assert.equal(config.cacheStaleSeconds > config.cacheTtlSeconds, true);
    assert.deepEqual(config.jwtAlgorithms, ['RS256', 'ES256', 'HS256']);
    assert.equal(config.botProtection, true);
  });

  test('parses string bindings from wrangler vars', () => {
    const config = resolveConfig({
      ORIGIN_URL: 'https://api.internal.tessera.xyz',
      RATE_LIMIT_MAX: '42',
      RATE_LIMIT_WINDOW_SECONDS: '15',
      RATE_LIMIT_FAIL_OPEN: 'false',
      REQUIRED_HEADERS: 'X-API-Version, X-Client',
      CACHE_PATHS: '/stats',
      JWT_ALGORITHMS: 'rs256',
      JWT_REQUIRED_PATHS: '/v1/audit/*,/v1/admin',
    });
    assert.equal(config.originUrl, 'https://api.internal.tessera.xyz');
    assert.equal(config.rateLimitMax, 42);
    assert.equal(config.rateLimitWindowSeconds, 15);
    assert.equal(config.rateLimitFailOpen, false);
    assert.deepEqual(config.requiredHeaders, ['x-api-version', 'x-client']);
    assert.deepEqual(config.cachePaths, ['/stats']);
    assert.deepEqual(config.jwtAlgorithms, ['RS256']);
    assert.deepEqual(config.jwtRequiredPaths, ['/v1/audit/*', '/v1/admin']);
  });
});

/* ------------------------------------------------------------------------- *
 * Header validation
 * ------------------------------------------------------------------------- */

describe('validateHeaders', () => {
  const config = resolveConfig({});

  test('accepts a normal read request', () => {
    const request = new Request('https://edge.tessera.xyz/v1/stats', { headers: { accept: 'application/json' } });
    assert.deepEqual(validateHeaders(request, config), { ok: true });
  });

  test('rejects methods outside the allow-list with 405', () => {
    const request = new Request('https://edge.tessera.xyz/v1/stats', { method: 'PATCH' });
    const result = validateHeaders(request, config);
    assert.equal(result.ok, false);
    assert.equal(result.status, 405);
    assert.equal(result.error, 'method_not_allowed');
  });

  test('rejects edge-bypass headers with 400', () => {
    const request = new Request('https://edge.tessera.xyz/v1/stats', {
      headers: { 'x-http-method-override': 'DELETE' },
    });
    const result = validateHeaders(request, config);
    assert.equal(result.ok, false);
    assert.equal(result.status, 400);
    assert.equal(result.error, 'blocked_header');
  });

  test('rejects requests missing a configured required header', () => {
    const strict = resolveConfig({ REQUIRED_HEADERS: 'x-api-version' });
    const request = new Request('https://edge.tessera.xyz/v1/stats');
    const result = validateHeaders(request, strict);
    assert.equal(result.ok, false);
    assert.equal(result.error, 'missing_header');
  });

  test('rejects header counts above the cap with 431', () => {
    const headers = {};
    for (let i = 0; i < 150; i += 1) headers[`x-tessera-test-${i}`] = 'v';
    const request = new Request('https://edge.tessera.xyz/v1/stats', { headers });
    const result = validateHeaders(request, config);
    assert.equal(result.ok, false);
    assert.equal(result.status, 431);
    assert.equal(result.error, 'too_many_headers');
  });

  test('rejects oversized header values with 431', () => {
    const request = new Request('https://edge.tessera.xyz/v1/stats', {
      headers: { 'x-tessera-blob': 'a'.repeat(9000) },
    });
    const result = validateHeaders(request, config);
    assert.equal(result.ok, false);
    assert.equal(result.status, 431);
    assert.equal(result.error, 'header_too_large');
  });
});

describe('buildOriginHeaders', () => {
  test('forwards only allow-listed headers and rewrites the client address', () => {
    const config = resolveConfig({});
    const request = new Request('https://edge.tessera.xyz/v1/assets', {
      headers: {
        accept: 'application/json',
        'user-agent': 'tessera-tests/1.0',
        'x-tessera-evil': 'drop-me',
        'x-forwarded-for': '10.0.0.1',
        'x-real-ip': '10.0.0.2',
        'cf-connecting-ip': '203.0.113.9',
      },
    });

    const headers = buildOriginHeaders(request, config, resolveClientIp(request));
    assert.equal(headers.get('accept'), 'application/json');
    assert.equal(headers.get('user-agent'), 'tessera-tests/1.0');
    assert.equal(headers.get('x-tessera-evil'), null);
    assert.equal(headers.get('x-forwarded-for'), '203.0.113.9');
    assert.equal(headers.get('x-real-ip'), '203.0.113.9');
    assert.equal(headers.get('x-tessera-edge'), 'tessera-edge-shield');
  });
});

describe('resolveClientIp', () => {
  test('prefers the Cloudflare-observed address', () => {
    const request = new Request('https://edge.tessera.xyz/', {
      headers: { 'cf-connecting-ip': '203.0.113.7', 'x-forwarded-for': '198.51.100.1, 198.51.100.2' },
    });
    assert.equal(resolveClientIp(request), '203.0.113.7');
  });

  test('falls back to the first X-Forwarded-For hop', () => {
    const request = new Request('https://edge.tessera.xyz/', {
      headers: { 'x-forwarded-for': '198.51.100.1, 198.51.100.2' },
    });
    assert.equal(resolveClientIp(request), '198.51.100.1');
  });

  test('returns "unknown" when no address header is present', () => {
    assert.equal(resolveClientIp(new Request('https://edge.tessera.xyz/')), 'unknown');
  });
});

/* ------------------------------------------------------------------------- *
 * Injection + bot heuristics
 * ------------------------------------------------------------------------- */

describe('detectInjection', () => {
  test('flags classic SQL injection payloads', () => {
    assert.equal(detectInjection('?filter=1 OR 1=1'), 'sql_or_tautology');
    assert.equal(detectInjection("?q=1' UNION SELECT password FROM users"), 'sql_union_select');
    assert.equal(detectInjection("?id=1; SELECT * FROM information_schema.tables"), 'sql_statement');
    assert.equal(detectInjection('?id=1%20--'), 'sql_comment');
    assert.equal(detectInjection('?id=1 AND sleep(5)'), 'sql_time_delay');
  });

  test('flags encoded payloads after percent-decoding', () => {
    assert.equal(detectInjection('?q=%27%20OR%201%3D1'), 'sql_or_tautology');
    assert.equal(detectInjection('%2e%2e%2fetc%2fpasswd'), 'path_traversal');
  });

  test('flags path traversal, XSS and shell probes', () => {
    assert.equal(detectInjection('/../../etc/passwd'), 'path_traversal');
    assert.equal(detectInjection('?next=<script>alert(1)</script>'), 'xss_probe');
    assert.equal(detectInjection('?host=x;cat /etc/hosts'), 'shell_injection');
  });

  test('leaves ordinary API queries alone', () => {
    assert.equal(detectInjection(''), null);
    assert.equal(detectInjection('?limit=10&cursor=CAFEBABE'), null);
    assert.equal(detectInjection('/v1/assets/CDLZFC3SYJYDZT7K67VZ75'), null);
    assert.equal(detectInjection('?sort=updated_at&order=desc'), null);
  });
});

describe('isBlockedUserAgent', () => {
  const config = resolveConfig({});

  test('blocks known security scanners', () => {
    assert.equal(isBlockedUserAgent('sqlmap/1.7#stable', config), true);
    assert.equal(isBlockedUserAgent('Mozilla/5.0 (compatible; Nuclei)', config), true);
  });

  test('allows browsers and API clients', () => {
    assert.equal(isBlockedUserAgent('Mozilla/5.0 (Macintosh) AppleWebKit/537.36', config), false);
    assert.equal(isBlockedUserAgent('tessera-synthetic-monitor/1.0', config), false);
    assert.equal(isBlockedUserAgent(undefined, config), false);
  });
});

/* ------------------------------------------------------------------------- *
 * Rate limiting
 * ------------------------------------------------------------------------- */

describe('slidingWindowEstimate', () => {
  test('weights the previous window by the elapsed fraction', () => {
    assert.equal(slidingWindowEstimate(10, 0, 0), 10);
    assert.equal(slidingWindowEstimate(10, 0, 0.5), 5);
    assert.equal(slidingWindowEstimate(10, 0, 1), 0);
    assert.equal(slidingWindowEstimate(10, 4, 0.5), 9);
  });
});

describe('checkRateLimit', () => {
  const config = resolveConfig({ RATE_LIMIT_MAX: '3', RATE_LIMIT_WINDOW_SECONDS: '60' });
  const T0 = 1_700_000_000_000;

  test('allows up to the limit then rejects with a Retry-After', async () => {
    const kv = new FakeKv();
    const first = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0);
    const second = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0 + 1000);
    const third = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0 + 2000);
    const fourth = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0 + 3000);

    assert.equal(first.allowed, true);
    assert.equal(second.allowed, true);
    assert.equal(third.allowed, true);
    assert.equal(fourth.allowed, false);
    assert.equal(fourth.remaining, 0);
    assert.ok(fourth.retryAfter >= 1 && fourth.retryAfter <= 60);
    assert.equal(first.limit, 3);
  });

  test('tracks each client independently', async () => {
    const kv = new FakeKv();
    await checkRateLimit(kv, 'rl:198.51.100.1', config, T0);
    await checkRateLimit(kv, 'rl:198.51.100.1', config, T0);
    const other = await checkRateLimit(kv, 'rl:198.51.100.2', config, T0);
    assert.equal(other.allowed, true);
    assert.equal(other.remaining, 2);
  });

  test('forgets traffic two windows later', async () => {
    const kv = new FakeKv();
    for (let i = 0; i < 3; i += 1) await checkRateLimit(kv, 'rl:203.0.113.9', config, T0);
    const denied = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0);
    assert.equal(denied.allowed, false);

    const afterTwoWindows = await checkRateLimit(kv, 'rl:203.0.113.9', config, T0 + 120_000);
    assert.equal(afterTwoWindows.allowed, true);
    assert.equal(afterTwoWindows.remaining, 2);
  });

  test('fails open when the KV binding is missing', async () => {
    const result = await checkRateLimit(undefined, 'rl:1.2.3.4', config, T0);
    assert.equal(result.allowed, true);
    assert.equal(result.degraded, true);
  });

  test('fails closed when configured to do so', async () => {
    const strict = resolveConfig({ RATE_LIMIT_FAIL_OPEN: 'false' });
    const result = await checkRateLimit(undefined, 'rl:1.2.3.4', strict, T0);
    assert.equal(result.allowed, false);
    assert.equal(result.retryAfter, strict.rateLimitWindowSeconds);
  });
});

/* ------------------------------------------------------------------------- *
 * JWT verification
 * ------------------------------------------------------------------------- */

describe('JWT helpers', () => {
  test('extracts bearer tokens case-insensitively', () => {
    const withToken = new Request('https://edge.tessera.xyz/', { headers: { authorization: 'bearer abc.def.ghi' } });
    assert.equal(getBearerToken(withToken), 'abc.def.ghi');
    assert.equal(getBearerToken(new Request('https://edge.tessera.xyz/')), null);
  });

  test('matches required paths exactly or by prefix', () => {
    assert.equal(isPathRequired('/v1/audit/entries', ['/v1/audit/*']), true);
    assert.equal(isPathRequired('/v1/audit', ['/v1/audit/*']), false);
    assert.equal(isPathRequired('/v1/admin', ['/v1/audit/*', '/v1/admin']), true);
    assert.equal(isPathRequired('/v1/stats', ['/v1/audit/*']), false);
  });

  test('validates exp, nbf, iss and aud claims', () => {
    const config = resolveConfig({
      JWT_ISSUER: 'https://auth.tessera.xyz',
      JWT_AUDIENCE: 'tessera-api',
    });
    const now = 1_700_000_000_000;
    assert.equal(
      validateJwtClaims(
        { exp: Math.floor(now / 1000) + 60, iss: 'https://auth.tessera.xyz', aud: ['tessera-api'] },
        config,
        now
      ),
      true
    );
    assert.throws(() => validateJwtClaims({ iss: config.jwtIssuer, aud: 'tessera-api' }, config, now), /exp/);
    assert.throws(
      () => validateJwtClaims({ exp: Math.floor(now / 1000) - 3600, iss: config.jwtIssuer, aud: 'tessera-api' }, config, now),
      /expired/
    );
    assert.throws(
      () => validateJwtClaims({ exp: Math.floor(now / 1000) + 60, iss: 'https://evil.example', aud: 'tessera-api' }, config, now),
      /issuer/
    );
    assert.throws(
      () => validateJwtClaims({ exp: Math.floor(now / 1000) + 60, iss: config.jwtIssuer, aud: 'other' }, config, now),
      /audience/
    );
  });
});

describe('verifyJwt (HS256)', () => {
  const config = resolveConfig({ JWT_ALGORITHMS: 'HS256', JWT_SECRET: 'super-secret-edge-key' });
  const now = 1_700_000_000_000;

  test('accepts a correctly signed token', async () => {
    const token = await signJwt({ sub: 'scout-1', exp: Math.floor(now / 1000) + 300 }, {
      alg: 'HS256',
      secret: config.jwtSecret,
    });
    const payload = await verifyJwt(token, config, { now });
    assert.equal(payload.sub, 'scout-1');
  });

  test('rejects a token signed with a different secret', async () => {
    const token = await signJwt({ sub: 'scout-1', exp: Math.floor(now / 1000) + 300 }, {
      alg: 'HS256',
      secret: 'not-the-edge-secret',
    });
    await assert.rejects(verifyJwt(token, config, { now }), (error) => {
      assert.ok(error instanceof JwtError);
      assert.equal(error.reason, 'invalid_signature');
      return true;
    });
  });

  test('rejects unsigned ("none") tokens', async () => {
    const header = base64UrlEncodeString(JSON.stringify({ alg: 'none', typ: 'JWT' }));
    const payload = base64UrlEncodeString(JSON.stringify({ sub: 'scout-1', exp: Math.floor(now / 1000) + 300 }));
    await assert.rejects(verifyJwt(`${header}.${payload}.`, config, { now }), (error) => {
      assert.equal(error.reason, 'malformed_token');
      return true;
    });
  });

  test('rejects an algorithm that is not in the allow-list', async () => {
    const token = await signJwt({ exp: Math.floor(now / 1000) + 300 }, { alg: 'HS512', secret: config.jwtSecret });
    await assert.rejects(verifyJwt(token, config, { now }), (error) => {
      assert.equal(error.reason, 'unsupported_algorithm');
      return true;
    });
  });

  test('rejects expired tokens', async () => {
    const token = await signJwt({ sub: 'scout-1', exp: Math.floor(now / 1000) - 3600 }, {
      alg: 'HS256',
      secret: config.jwtSecret,
    });
    await assert.rejects(verifyJwt(token, config, { now }), (error) => {
      assert.equal(error.reason, 'token_expired');
      return true;
    });
  });
});

describe('verifyJwt (asymmetric via JWKS)', () => {
  const now = 1_700_000_000_000;

  test('accepts an RS256 token whose kid exists in the JWKS document', async () => {
    const { pair, jwk } = await generateRsaKeyPair();
    const config = resolveConfig({
      JWT_ALGORITHMS: 'RS256',
      JWT_JWKS_URL: 'https://auth.tessera.xyz/.well-known/jwks.json',
      JWT_ISSUER: 'https://auth.tessera.xyz',
      JWT_AUDIENCE: 'tessera-api',
    });
    const deps = {
      now,
      jwksCache: new Map(),
      fetch: async () =>
        new Response(JSON.stringify({ keys: [jwk] }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    };
    const token = await signJwt(
      { sub: 'scout-2', iss: 'https://auth.tessera.xyz', aud: 'tessera-api', exp: Math.floor(now / 1000) + 300 },
      { alg: 'RS256', privateKey: pair.privateKey, header: { kid: 'rsa-test-key' } }
    );

    const payload = await verifyJwt(token, config, deps);
    assert.equal(payload.sub, 'scout-2');
  });

  test('accepts an ES256 token signed by an EC JWKS key', async () => {
    const { pair, jwk } = await generateEcKeyPair();
    const config = resolveConfig({
      JWT_ALGORITHMS: 'ES256',
      JWT_JWKS_URL: 'https://auth.tessera.xyz/.well-known/jwks.json',
    });
    const deps = {
      now,
      jwksCache: new Map(),
      fetch: async () =>
        new Response(JSON.stringify({ keys: [jwk] }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    };
    const token = await signJwt({ sub: 'scout-3', exp: Math.floor(now / 1000) + 300 }, {
      alg: 'ES256',
      privateKey: pair.privateKey,
      header: { kid: 'ec-test-key' },
    });

    const payload = await verifyJwt(token, config, deps);
    assert.equal(payload.sub, 'scout-3');
  });

  test('rejects a token whose kid is not present in the JWKS', async () => {
    const { pair, jwk } = await generateRsaKeyPair();
    const config = resolveConfig({
      JWT_ALGORITHMS: 'RS256',
      JWT_JWKS_URL: 'https://auth.tessera.xyz/.well-known/jwks.json',
    });
    const deps = {
      now,
      jwksCache: new Map(),
      fetch: async () => new Response(JSON.stringify({ keys: [jwk] }), { status: 200 }),
    };
    const token = await signJwt({ exp: Math.floor(now / 1000) + 300 }, {
      alg: 'RS256',
      privateKey: pair.privateKey,
      header: { kid: 'a-different-key' },
    });

    await assert.rejects(verifyJwt(token, config, deps), (error) => {
      assert.equal(error.reason, 'unknown_key');
      return true;
    });
  });
});

/* ------------------------------------------------------------------------- *
 * Caching
 * ------------------------------------------------------------------------- */

describe('isCacheableRequest', () => {
  const config = resolveConfig({});

  test('accepts unauthenticated GETs on the configured static paths', () => {
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/v1/stats'), config), true);
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/stats'), config), true);
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/assets'), config), true);
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/v1/assets'), config), true);
  });

  test('rejects authenticated requests, non-GETs and other paths', () => {
    const authed = new Request('https://edge.tessera.xyz/v1/stats', { headers: { authorization: 'Bearer x.y.z' } });
    assert.equal(isCacheableRequest(authed, config), false);
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/v1/stats', { method: 'POST' }), config), false);
    assert.equal(isCacheableRequest(new Request('https://edge.tessera.xyz/v1/events'), config), false);
  });
});

describe('edge cache with stale-while-revalidate', () => {
  const now = 1_700_000_000_000;

  test('serves a MISS from origin then a HIT from the edge cache', async () => {
    const cache = new FakeCache();
    const { fetchImpl, calls } = originResponder();
    const deps = { now, cache, fetch: fetchImpl };
    const env = edgeEnv();
    const makeRequest = () => new Request('https://edge.tessera.xyz/v1/stats');

    const miss = await handleRequest(makeRequest(), env, {}, deps);
    assert.equal(miss.status, 200);
    assert.equal(miss.headers.get('x-edge-cache'), 'MISS');
    assert.equal(miss.headers.get('x-edge-stored-at'), null);

    const hit = await handleRequest(makeRequest(), env, {}, deps);
    assert.equal(hit.status, 200);
    assert.equal(hit.headers.get('x-edge-cache'), 'HIT');
    assert.equal(hit.headers.get('age'), '0');
    assert.equal(calls.length, 1, 'origin is contacted once; the second request is an edge hit');
  });

  test('returns a stale copy and revalidates in the background', async () => {
    const cache = new FakeCache();
    let originBody = JSON.stringify({ generation: 2 });
    const originRequests = [];
    const deps = {
      now,
      cache,
      fetch: async (request) => {
        originRequests.push(request.url);
        return new Response(originBody, {
          status: 200,
          headers: { 'content-type': 'application/json', etag: '"snapshot-2"' },
        });
      },
    };
    const env = edgeEnv();

    await cache.put(
      new Request('https://edge.tessera.xyz/v1/stats'),
      new Response(JSON.stringify({ generation: 1 }), {
        status: 200,
        headers: {
          'content-type': 'application/json',
          etag: '"snapshot-1"',
          'x-edge-stored-at': String(now - 45_000), // inside the 30s+120s stale window
        },
      })
    );

    const pending = [];
    const ctx = { waitUntil: (promise) => pending.push(promise) };
    const stale = await handleRequest(new Request('https://edge.tessera.xyz/v1/stats'), env, ctx, deps);

    assert.equal(stale.headers.get('x-edge-cache'), 'STALE');
    assert.equal(await stale.text(), JSON.stringify({ generation: 1 }));

    await Promise.all(pending);
    assert.equal(originRequests.length, 1, 'the stale hit triggered one background revalidation');

    const refreshed = await handleRequest(new Request('https://edge.tessera.xyz/v1/stats'), env, {}, deps);
    assert.equal(refreshed.headers.get('x-edge-cache'), 'HIT');
    assert.equal(await refreshed.text(), JSON.stringify({ generation: 2 }));
  });

  test('answers a conditional request from cache with 304 Not Modified', async () => {
    const cache = new FakeCache();
    const { fetchImpl, calls } = originResponder();
    const deps = { now, cache, fetch: fetchImpl };
    const env = edgeEnv();

    await handleRequest(new Request('https://edge.tessera.xyz/v1/stats'), env, {}, deps);
    const conditional = await handleRequest(
      new Request('https://edge.tessera.xyz/v1/stats', { headers: { 'if-none-match': '"snapshot-1"' } }),
      env,
      {},
      deps
    );

    assert.equal(conditional.status, 304);
    assert.equal(conditional.headers.get('etag'), '"snapshot-1"');
    assert.equal(calls.length, 1);
  });

  test('does not cache a no-store origin response', async () => {
    const cache = new FakeCache();
    let hits = 0;
    const deps = {
      now,
      cache,
      fetch: async () => {
        hits += 1;
        return new Response('{}', { status: 200, headers: { 'cache-control': 'no-store' } });
      },
    };
    const env = edgeEnv();

    await handleRequest(new Request('https://edge.tessera.xyz/v1/stats'), env, {}, deps);
    await handleRequest(new Request('https://edge.tessera.xyz/v1/stats'), env, {}, deps);
    assert.equal(hits, 2, 'a no-store response must never be served from the edge cache');
  });
});

/* ------------------------------------------------------------------------- *
 * Full pipeline
 * ------------------------------------------------------------------------- */

describe('handleRequest pipeline', () => {
  const now = 1_700_000_000_000;

  test('blocks SQL injection in the query string with 403', async () => {
    const { fetchImpl, calls } = originResponder();
    const response = await handleRequest(
      new Request('https://edge.tessera.xyz/v1/assets?filter=1%20OR%201%3D1', { headers: { 'user-agent': 'tests' } }),
      edgeEnv(),
      {},
      { now, cache: new FakeCache(), fetch: fetchImpl }
    );
    assert.equal(response.status, 403);
    const body = await response.json();
    assert.equal(body.error, 'blocked_request');
    assert.equal(calls.length, 0);
  });

  test('blocks scanner user agents with 403', async () => {
    const response = await handleRequest(
      new Request('https://edge.tessera.xyz/v1/stats', { headers: { 'user-agent': 'sqlmap/1.7#stable' } }),
      edgeEnv(),
      {},
      { now, cache: new FakeCache(), fetch: originResponder().fetchImpl }
    );
    assert.equal(response.status, 403);
    assert.equal((await response.json()).error, 'blocked_bot');
  });

  test('returns 405 for a disallowed method', async () => {
    const response = await handleRequest(new Request('https://edge.tessera.xyz/v1/stats', { method: 'PATCH' }), edgeEnv(), {}, { now });
    assert.equal(response.status, 405);
  });

  test('enforces the per-IP rate limit with 429 and rate limit headers', async () => {
    const env = edgeEnv({ RATE_LIMIT_MAX: '2', RATE_LIMIT_WINDOW_SECONDS: '60' });
    const deps = { now, cache: new FakeCache(), fetch: originResponder().fetchImpl };
    const makeRequest = () => new Request('https://edge.tessera.xyz/health');

    const first = await handleRequest(makeRequest(), env, {}, deps);
    const second = await handleRequest(makeRequest(), env, {}, deps);
    const third = await handleRequest(makeRequest(), env, {}, deps);

    assert.equal(first.status, 200);
    assert.equal(first.headers.get('x-ratelimit-limit'), '2');
    assert.equal(second.status, 200);
    assert.equal(third.status, 429);
    assert.equal(third.headers.get('x-ratelimit-remaining'), '0');
    assert.ok(Number(third.headers.get('retry-after')) >= 1);
    const body = await third.json();
    assert.equal(body.error, 'rate_limited');
  });

  test('requires a bearer token on configured protected paths', async () => {
    const env = edgeEnv({ JWT_REQUIRED_PATHS: '/v1/audit/*', JWT_ALGORITHMS: 'HS256', JWT_SECRET: 'secret' });
    const response = await handleRequest(new Request('https://edge.tessera.xyz/v1/audit/entries'), env, {}, { now });
    assert.equal(response.status, 401);
    assert.equal((await response.json()).error, 'missing_token');
  });

  test('rejects an invalid bearer token with 401 and the JWT failure reason', async () => {
    const env = edgeEnv({ JWT_ALGORITHMS: 'HS256', JWT_SECRET: 'secret' });
    const response = await handleRequest(
      new Request('https://edge.tessera.xyz/v1/stats', { headers: { authorization: 'Bearer not.a.jwt' } }),
      env,
      {},
      { now }
    );
    assert.equal(response.status, 401);
    assert.equal((await response.json()).error, 'malformed_token');
  });

  test('proxies other paths to ORIGIN_URL and forwards only allow-listed headers', async () => {
    let seen;
    const env = edgeEnv();
    const deps = {
      now,
      cache: new FakeCache(),
      fetch: async (request) => {
        seen = request;
        return new Response(JSON.stringify({ proxied: true }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      },
    };
    const response = await handleRequest(
      new Request('https://edge.tessera.xyz/v1/events?limit=5', {
        headers: { accept: 'application/json', 'x-tessera-evil': 'nope', 'cf-connecting-ip': '203.0.113.50' },
      }),
      env,
      {},
      deps
    );

    assert.equal(response.status, 200);
    assert.equal(response.headers.get('x-edge-cache'), null);
    assert.equal(seen.url, 'https://origin.tessera.xyz/v1/events?limit=5');
    assert.equal(seen.headers.get('accept'), 'application/json');
    assert.equal(seen.headers.get('x-tessera-evil'), null);
    assert.equal(seen.headers.get('x-forwarded-for'), '203.0.113.50');
  });

  test('reports a misconfigured origin instead of crashing', async () => {
    const env = edgeEnv({ ORIGIN_URL: '' });
    const response = await handleRequest(new Request('https://edge.tessera.xyz/v1/events'), env, {}, { now });
    assert.equal(response.status, 500);
    assert.equal((await response.json()).error, 'edge_misconfigured');
  });

  test('adds a request id and the edge marker to every response', async () => {
    const response = await handleRequest(new Request('https://edge.tessera.xyz/health'), edgeEnv(), {}, { now });
    assert.ok(response.headers.get('x-request-id'));
    assert.equal(response.headers.get('x-tessera-edge'), 'tessera-edge-shield');
  });
});

/* ------------------------------------------------------------------------- *
 * Issue #422: Edge caching s-maxage=10, Soroban KV invalidation,
 * cache-key normalisation, Cf-Cache-Status Logpush, and request coalescing
 * ------------------------------------------------------------------------- */

describe('edge caching for GET /assets, GET /assets/:id, and GET /stats (#422)', () => {
  const now = 1_700_000_000_000;

  test('caches GET /assets, GET /assets/:id, and GET /stats with Cache-Control: s-maxage=10', async () => {
    const config = resolveConfig({});
    assert.equal(config.cacheSMaxAgeSeconds, DEFAULT_CACHE_S_MAXAGE_SECONDS);
    assert.equal(config.cacheSMaxAgeSeconds, 10);
    assert.equal(
      isCacheableRequest(
        new Request('https://edge.tessera.xyz/assets/CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ'),
        config
      ),
      true
    );
    assert.equal(
      isCacheableRequest(new Request('https://edge.tessera.xyz/v1/assets/1'), config),
      true
    );
    assert.equal(
      isCacheableRequest(new Request('https://edge.tessera.xyz/assets/1/holders'), config),
      false,
      'nested sub-resources under /assets/:id/... are not matched by /assets/:id'
    );

    const cache = new FakeCache();
    const { fetchImpl, calls } = originResponder();
    const deps = { now, cache, fetch: fetchImpl };
    const env = edgeEnv();

    for (const path of ['/assets', '/assets/1', '/stats']) {
      const url = `https://edge.tessera.xyz${path}`;
      const miss = await handleRequest(new Request(url), env, {}, deps);
      assert.equal(miss.status, 200);
      assert.equal(miss.headers.get('Cf-Cache-Status'), 'MISS');
      assert.equal(miss.headers.get('x-edge-cache'), 'MISS');
      assert.match(miss.headers.get('cache-control') || '', /\bs-maxage=10\b/);

      const hit = await handleRequest(new Request(url), env, {}, deps);
      assert.equal(hit.status, 200);
      assert.equal(hit.headers.get('Cf-Cache-Status'), 'HIT');
      assert.equal(hit.headers.get('x-edge-cache'), 'HIT');
      assert.match(hit.headers.get('cache-control') || '', /\bs-maxage=10\b/);
    }

    assert.equal(calls.length, 3, 'each distinct endpoint hit origin once and served the second call from cache');
  });

  test('normalises cache keys by stripping unknown query params and sorting known params alphabetically', async () => {
    const config = resolveConfig({});
    assert.ok(DEFAULT_KNOWN_QUERY_PARAMS.includes('limit'));
    assert.ok(DEFAULT_KNOWN_QUERY_PARAMS.includes('sort'));

    const urlA = 'https://edge.tessera.xyz/assets?utm_source=twitter&sort=name&limit=10&fbclid=123&order=asc';
    const urlB = 'https://edge.tessera.xyz/assets?limit=10&order=asc&sort=name&random_param=xyz';

    assert.equal(
      normalizeCacheUrl(urlA, config),
      'https://edge.tessera.xyz/assets?limit=10&order=asc&sort=name'
    );
    assert.equal(normalizeCacheUrl(urlA, config), normalizeCacheUrl(urlB, config));
    assert.equal(buildCacheKey(new Request(urlA), config).url, buildCacheKey(new Request(urlB), config).url);

    const cache = new FakeCache();
    const { fetchImpl, calls } = originResponder();
    const deps = { now, cache, fetch: fetchImpl };
    const env = edgeEnv();

    const first = await handleRequest(new Request(urlA), env, {}, deps);
    assert.equal(first.headers.get('Cf-Cache-Status'), 'MISS');

    const second = await handleRequest(new Request(urlB), env, {}, deps);
    assert.equal(second.headers.get('Cf-Cache-Status'), 'HIT');
    assert.equal(calls.length, 1, 'second request with reordered/unknown query params hit the normalized cache entry');
  });

  test('bypasses cache when Soroban polling pushes invalidated contract IDs to Workers KV', async () => {
    const invalidationKv = new FakeKv();
    const cache = new FakeCache();
    let originCounter = 0;
    const deps = {
      now,
      cache,
      fetch: async () => {
        originCounter += 1;
        return new Response(JSON.stringify({ version: originCounter }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      },
    };
    const env = edgeEnv({ CACHE_INVALIDATION: invalidationKv });

    const contractA = 'CBMCWLSQSWUTLUJFCNBHNBSXMUM3XU7NAQ5TSNERW4HA4ZZBYHLG4ECZ';
    const contractB = 'CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3';
    assert.equal(extractAssetId(`https://edge.tessera.xyz/assets/${contractA}`), contractA);

    // Prime cache for both contractA and contractB
    const missA = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env, {}, deps);
    const missB = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractB}`), env, {}, deps);
    assert.equal(missA.headers.get('Cf-Cache-Status'), 'MISS');
    assert.equal(missB.headers.get('Cf-Cache-Status'), 'MISS');

    // Both are now cached (HIT)
    const hitA = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env, {}, deps);
    assert.equal(hitA.headers.get('Cf-Cache-Status'), 'HIT');

    // Soroban polling cycle pushes contractA to KV invalidation list
    const pushed = await pushInvalidatedContracts(invalidationKv, [contractA], {
      now,
      ledgerSequence: 3514200,
    });
    assert.deepEqual(pushed.contractIds, [contractA]);
    const storedSet = await getInvalidatedContracts(invalidationKv);
    assert.equal(storedSet.has(contractA), true);
    assert.equal(storedSet.has(contractB), false);
    assert.equal(
      await isRequestInvalidated(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env),
      true
    );

    // Request for invalidated contractA bypasses cache and fetches fresh data from origin
    const bypassA = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env, {}, deps);
    assert.equal(bypassA.headers.get('Cf-Cache-Status'), 'BYPASS');
    assert.equal(bypassA.headers.get('x-edge-cache'), 'BYPASS');
    assert.equal(bypassA.headers.get('x-edge-cache-invalidated'), 'true');
    assert.deepEqual(await bypassA.json(), { version: 3 });

    // Uninvalidated contractB still serves from cache (HIT)
    const hitB = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractB}`), env, {}, deps);
    assert.equal(hitB.headers.get('Cf-Cache-Status'), 'HIT');
    assert.deepEqual(await hitB.json(), { version: 2 });

    // Once Soroban polling clears the invalidation list, contractA can be cached again
    await invalidationKv.put(INVALIDATED_CONTRACTS_KV_KEY, JSON.stringify({ contractIds: [] }));
    const reMissA = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env, {}, deps);
    assert.equal(reMissA.headers.get('Cf-Cache-Status'), 'MISS');
    const reHitA = await handleRequest(new Request(`https://edge.tessera.xyz/assets/${contractA}`), env, {}, deps);
    assert.equal(reHitA.headers.get('Cf-Cache-Status'), 'HIT');
  });

  test('logs Cf-Cache-Status in Logpush records for MISS, HIT, BYPASS, and DYNAMIC requests', async () => {
    const invalidationKv = new FakeKv();
    const cache = new FakeCache();
    const logpushRecords = [];
    const { fetchImpl } = originResponder();
    const deps = { now, cache, fetch: fetchImpl, logpush: logpushRecords };
    const env = edgeEnv({ CACHE_INVALIDATION: invalidationKv });

    // 1. MISS
    await handleRequest(new Request('https://edge.tessera.xyz/stats'), env, {}, deps);
    // 2. HIT
    await handleRequest(new Request('https://edge.tessera.xyz/stats'), env, {}, deps);
    // 3. BYPASS
    await pushInvalidatedContracts(invalidationKv, ['asset-42']);
    await handleRequest(new Request('https://edge.tessera.xyz/assets/asset-42'), env, {}, deps);
    // 4. DYNAMIC (non-cacheable endpoint)
    const dynamicRes = await handleRequest(new Request('https://edge.tessera.xyz/v1/events'), env, {}, deps);
    assert.equal(dynamicRes.headers.get('Cf-Cache-Status'), 'DYNAMIC');

    assert.equal(logpushRecords.length, 4);
    assert.deepEqual(
      logpushRecords.map((r) => r['Cf-Cache-Status']),
      ['MISS', 'HIT', 'BYPASS', 'DYNAMIC']
    );
    assert.equal(logpushRecords[2].invalidated, true);
    assert.equal(logpushRecords[2].assetId, 'asset-42');

    const directRecord = emitLogpushRecord({ cfCacheStatus: 'HIT', pathname: '/stats', status: 200 }, env, {}, { now });
    assert.equal(directRecord['Cf-Cache-Status'], 'HIT');
  });

  test('coalesces concurrent cache-miss requests for the same key into a single origin fetch', async () => {
    const cache = new FakeCache();
    const inFlight = new Map();
    let originCalls = 0;
    let releaseOrigin;
    const originGate = new Promise((resolve) => {
      releaseOrigin = resolve;
    });

    const deps = {
      now,
      cache,
      inFlight,
      fetch: async () => {
        originCalls += 1;
        await originGate;
        return new Response(JSON.stringify({ coalescedPayload: true, calls: originCalls }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      },
    };
    const env = edgeEnv();

    // Fire 5 concurrent requests with equivalent normalized cache keys while cache is cold
    const concurrentPromises = [
      handleRequest(new Request('https://edge.tessera.xyz/assets?limit=20&sort=id&utm_medium=cpc'), env, {}, deps),
      handleRequest(new Request('https://edge.tessera.xyz/assets?sort=id&limit=20&utm_source=x'), env, {}, deps),
      handleRequest(new Request('https://edge.tessera.xyz/assets?limit=20&sort=id'), env, {}, deps),
      handleRequest(new Request('https://edge.tessera.xyz/assets?sort=id&limit=20&ref=123'), env, {}, deps),
      handleRequest(new Request('https://edge.tessera.xyz/assets?limit=20&sort=id&fbclid=abc'), env, {}, deps),
    ];

    // Allow microtasks (rate-limit check + cache.match) to reach the in-flight origin fetch
    await new Promise((resolve) => setTimeout(resolve, 15));
    releaseOrigin();

    const responses = await Promise.all(concurrentPromises);
    assert.equal(originCalls, 1, 'only 1 origin fetch must be made for all 5 concurrent cache misses');

    const coalescedFlags = responses.map((res) => res.headers.get('x-edge-coalesced'));
    assert.equal(coalescedFlags.filter((f) => f === 'false').length, 1);
    assert.equal(coalescedFlags.filter((f) => f === 'true').length, 4);

    for (const res of responses) {
      assert.equal(res.status, 200);
      assert.equal(res.headers.get('Cf-Cache-Status'), 'MISS');
      const body = await res.json();
      assert.deepEqual(body, { coalescedPayload: true, calls: 1 });
    }
  });
});
