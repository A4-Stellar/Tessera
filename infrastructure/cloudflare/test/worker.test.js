import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_CACHE_PATHS,
  JwtError,
  buildOriginHeaders,
  checkRateLimit,
  detectInjection,
  getBearerToken,
  handleRequest,
  isBlockedUserAgent,
  isCacheableRequest,
  isPathRequired,
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
