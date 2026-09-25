// Peak-load regression benchmark for the Tessera API (#29).
//
//   k6 run tests/load/k6_script.js
//   k6 run -e BASE_URL=https://api.example.com -e RATE=2000 -e DURATION=2m tests/load/k6_script.js
//
// Drives a constant 2,000 requests/second (one request per iteration) spread
// evenly across the four hottest read endpoints, and fails (non-zero exit) if
// the response SLA is missed. A constant-arrival-rate executor is used so the
// offered load does not drop when the server slows down — a closed-loop
// (fixed-VU) test would quietly send fewer requests and hide regressions.
//
// The API rate-limits per client IP (RWA_RATE_LIMIT_PER_SECOND /
// RWA_RATE_LIMIT_BURST). Raise those limits on the instance under test, or
// the limiter's 429s will count as errors. See .github/workflows/load-test.yml.

import http from 'k6/http';
import exec from 'k6/execution';

const BASE_URL = (__ENV.BASE_URL || 'http://localhost:8080').replace(/\/+$/, '');
const RATE = Number(__ENV.RATE || 2000);
const DURATION = __ENV.DURATION || '1m';
const PRE_ALLOCATED_VUS = Number(__ENV.PRE_ALLOCATED_VUS || 200);
const MAX_VUS = Number(__ENV.MAX_VUS || 1000);

// Data routes are served under /v1 (api/src/routes/mod.rs).
const ENDPOINTS = [
  { name: 'GET /v1/stats', path: () => '/v1/stats' },
  { name: 'GET /v1/assets', path: () => '/v1/assets' },
  { name: 'GET /v1/assets/:id/holders', path: (id) => `/v1/assets/${id}/holders` },
  { name: 'GET /v1/assets/:id/compliance', path: (id) => `/v1/assets/${id}/compliance` },
];

export const options = {
  scenarios: {
    peak_read_load: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: PRE_ALLOCATED_VUS,
      maxVUs: MAX_VUS,
    },
  },
  thresholds: {
    // Response SLA: p95 latency under 50 ms, error rate under 0.01 %, measured
    // on the load itself (the one-off setup() request is excluded).
    'http_req_duration{scenario:peak_read_load}': ['p(95)<50'],
    'http_req_failed{scenario:peak_read_load}': ['rate<0.0001'],
    // Iterations k6 could not start (not enough VUs) mean the target rate was
    // never actually offered, so the run cannot vouch for 2,000 req/s.
    dropped_iterations: ['count<1'],
  },
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

/** Resolve real asset ids once, so per-asset routes hit indexed data, not 404s. */
export function setup() {
  const res = http.get(`${BASE_URL}/v1/assets`, { tags: { name: 'setup: GET /v1/assets' } });
  if (res.status !== 200) {
    throw new Error(`GET /v1/assets returned ${res.status}; is the API up and indexed?`);
  }
  const ids = res.json().map((asset) => asset.id);
  if (ids.length === 0) {
    throw new Error('No assets are indexed; per-asset endpoints would only return 404.');
  }
  return { ids };
}

/** One request per iteration, rotating endpoints and assets evenly: O(1) per request. */
export default function (data) {
  const i = exec.scenario.iterationInTest;
  const endpoint = ENDPOINTS[i % ENDPOINTS.length];
  const id = data.ids[Math.floor(i / ENDPOINTS.length) % data.ids.length];
  http.get(`${BASE_URL}${endpoint.path(id)}`, { tags: { name: endpoint.name } });
}
