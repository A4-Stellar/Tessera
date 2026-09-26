import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import test from "node:test";
import worker, { verifyJwt } from "../worker.js";

const secret = "a-test-secret-that-is-long-enough";

function base64url(value) {
  return Buffer.from(value).toString("base64url");
}

function makeJwt(claims = { exp: 4_000_000_000, iss: "tessera-api", aud: "tessera-api" }) {
  const header = base64url(JSON.stringify({ alg: "HS256", typ: "JWT" }));
  const payload = base64url(JSON.stringify(claims));
  const signingInput = `${header}.${payload}`;
  const signature = createHmac("sha256", secret).update(signingInput).digest("base64url");
  return `${signingInput}.${signature}`;
}

class MemoryCache {
  entries = new Map();
  async match(request) { return this.entries.get(request.url)?.clone(); }
  async put(request, response) { this.entries.set(request.url, response.clone()); }
}

function setup({ response = new Response('{"ok":true}', { headers: { "content-type": "application/json" } }), limited = false } = {}) {
  const cache = new MemoryCache();
  globalThis.caches = { default: cache };
  globalThis.fetch = async () => response.clone();
  const context = { tasks: [], waitUntil(task) { this.tasks.push(task); } };
  const env = {
    API_ORIGIN: "https://origin.example.com",
    ALLOWED_HOSTS: "api.tessera.xyz",
    JWT_SECRET: secret,
    JWT_ISSUER: "tessera-api",
    JWT_AUDIENCE: "tessera-api",
    IP_RATE_LIMIT: { limit: async () => ({ success: !limited }) },
  };
  return { cache, context, env };
}

function request(path = "/v1/assets", headers = {}) {
  return new Request(`https://api.tessera.xyz${path}`, {
    headers: { "user-agent": "Tessera-client/1.0", "cf-connecting-ip": "192.0.2.10", ...headers },
  });
}

test("accepts a valid HS256 token and enforces issuer and audience", async () => {
  assert.equal(await verifyJwt(makeJwt(), secret, 1_800_000_000, "tessera-api", "tessera-api"), true);
  assert.equal(await verifyJwt(makeJwt(), secret, 1_800_000_000, "wrong-issuer", "tessera-api"), false);
  assert.equal(await verifyJwt(makeJwt({ exp: 1 }), secret, 2), false);
});

test("rejects malformed and tampered JWT signatures", async () => {
  const token = makeJwt();
  assert.equal(await verifyJwt("not-a-token", secret), false);
  assert.equal(await verifyJwt(`${token.slice(0, -1)}x`, secret, 1_800_000_000), false);
});

test("blocks scanner user agents and SQL injection requests", async () => {
  const { env, context } = setup();
  assert.equal((await worker.fetch(request("/v1/assets", { "user-agent": "sqlmap/1.8" }), env, context)).status, 403);
  assert.equal((await worker.fetch(request("/v1/assets?id=1%20UNION%20SELECT%20password%20FROM%20users"), env, context)).status, 400);
});

test("bounds streamed request bodies before inspection", async () => {
  const { env, context } = setup();
  const oversized = new Request("https://api.tessera.xyz/v1/audit/entries", {
    method: "POST",
    headers: { "user-agent": "Tessera-client/1.0", "cf-connecting-ip": "192.0.2.10" },
    body: new Uint8Array(1_048_577),
  });
  assert.equal((await worker.fetch(oversized, env, context)).status, 413);
});

test("applies the IP rate limit and rejects invalid bearer tokens", async () => {
  const limited = setup({ limited: true });
  assert.equal((await worker.fetch(request(), limited.env, limited.context)).status, 429);

  const invalid = setup();
  assert.equal((await worker.fetch(request("/v1/assets", { authorization: "Bearer invalid" }), invalid.env, invalid.context)).status, 401);
});

test("proxies public API requests and caches collection responses", async () => {
  const state = setup();
  const response = await worker.fetch(request(), state.env, state.context);
  assert.equal(response.status, 200);
  assert.match(response.headers.get("cache-control"), /stale-while-revalidate=60/);
  assert.equal(state.cache.entries.size, 1);
  assert.match(state.cache.entries.get(request().url).headers.get("cache-control"), /max-age=300/);
  assert.equal((await worker.fetch(request(), state.env, state.context)).status, 200);
});

test("serves stale cache entries while scheduling a refresh", async () => {
  const state = setup();
  const req = request();
  await worker.fetch(req, state.env, state.context);
  const cached = state.cache.entries.get(req.url);
  const oldHeaders = new Headers(cached.headers);
  oldHeaders.set("x-edge-fetched-at", String(Date.now() - 45_000));
  state.cache.entries.set(req.url, new Response('{"version":"stale"}', { status: 200, headers: oldHeaders }));
  globalThis.fetch = async () => new Response('{"version":"fresh"}', { headers: { "content-type": "application/json" } });

  const stale = await worker.fetch(req, state.env, state.context);
  assert.equal(await stale.text(), '{"version":"stale"}');
  assert.match(stale.headers.get("cache-control"), /stale-while-revalidate=60/);
  assert.equal(state.context.tasks.length, 1);
  await state.context.tasks[0];
  assert.equal(await (await state.cache.match(req)).text(), '{"version":"fresh"}');
});

test("does not cache authenticated or cookie-bearing requests", async () => {
  const state = setup();
  await worker.fetch(request("/v1/assets", { authorization: `Bearer ${makeJwt()}` }), state.env, state.context);
  await worker.fetch(request("/v1/assets", { cookie: "session=abc" }), state.env, state.context);
  assert.equal(state.cache.entries.size, 0);
});
