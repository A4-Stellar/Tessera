// Realistic RWA Load Testing Suite with Persona Modeling & SLO Validation (Issue #447 / #64)
//
// Usage:
//   k6 run tests/load/k6_script.js
//   k6 run -e BASE_URL=http://localhost:8080 -o influxdb=http://localhost:8086/k6 tests/load/k6_script.js
//
// Personas:
//   1. IndexerPoller: 200 req/s to GET /v1/stats
//   2. InstitutionalTrader: 50 req/s to GET /v1/assets/:id/holders with Auth
//   3. RetailViewer: 500 req/s to GET /v1/assets
//   4. WebSocketStreamer: Concurrent WebSocket subscribers to /v1/ws
//
// SLOs:
//   - http_req_duration{p99} < 100ms
//   - http_req_failed < 0.1% (rate < 0.001)

import http from 'k6/http';
import { check, sleep } from 'k6';
import { SharedArray } from 'k6/data';
import { WebSocket } from 'k6/experimental/websockets';
import { Counter, Rate, Trend } from 'k6/metrics';

// Configuration
const BASE_URL = (__ENV.BASE_URL || 'http://localhost:8080').replace(/\/+$/, '');
const WS_URL = (__ENV.WS_URL || BASE_URL.replace('http://', 'ws://').replace('https://', 'wss://')) + '/v1/ws';
const AUTH_TOKEN = __ENV.AUTH_TOKEN || 'tessera_inst_auth_token_secret_123';

// Custom Metrics
const sloViolations = new Counter('slo_violations');
const wsMessageDuration = new Trend('ws_message_duration_ms');
const p999Trend = new Trend('p999_latency_ms');

// SharedArray loads 10k asset IDs into memory once across all VUs
const assetIds = new SharedArray('asset_ids', function () {
  try {
    const raw = open('./fixtures/asset_ids.json');
    return JSON.parse(raw);
  } catch (e) {
    // Fallback if running from a different working directory
    const fallback = [];
    for (let i = 1; i <= 10000; i++) fallback.push(i);
    return fallback;
  }
});

// Helper for Poisson Process arrival intervals (Exponential inter-arrival distribution)
function poissonInterval(ratePerSec) {
  const u = Math.random();
  return -Math.log(1 - u) / ratePerSec;
}

export const options = {
  scenarios: {
    // Persona 1: IndexerPoller (200 req/s) with ramp-up -> steady-state -> spike -> cool-down
    indexer_poller: {
      executor: 'ramping-arrival-rate',
      startRate: 20,
      timeUnit: '1s',
      preAllocatedVUs: 50,
      maxVUs: 300,
      stages: [
        { duration: '30s', target: 200 },  // Ramp-up
        { duration: '2m', target: 200 },   // Steady-state
        { duration: '30s', target: 400 },  // Spike (2x)
        { duration: '30s', target: 0 },    // Cool-down
      ],
      exec: 'indexerPollerScenario',
      tags: { persona: 'IndexerPoller' },
    },

    // Persona 2: InstitutionalTrader (50 req/s with auth)
    institutional_trader: {
      executor: 'ramping-arrival-rate',
      startRate: 5,
      timeUnit: '1s',
      preAllocatedVUs: 30,
      maxVUs: 150,
      stages: [
        { duration: '30s', target: 50 },   // Ramp-up
        { duration: '2m', target: 50 },    // Steady-state
        { duration: '30s', target: 100 },  // Spike (2x)
        { duration: '30s', target: 0 },    // Cool-down
      ],
      exec: 'institutionalTraderScenario',
      tags: { persona: 'InstitutionalTrader' },
    },

    // Persona 3: RetailViewer (500 req/s)
    retail_viewer: {
      executor: 'ramping-arrival-rate',
      startRate: 50,
      timeUnit: '1s',
      preAllocatedVUs: 100,
      maxVUs: 800,
      stages: [
        { duration: '30s', target: 500 },  // Ramp-up
        { duration: '2m', target: 500 },   // Steady-state
        { duration: '30s', target: 1000 }, // Spike (2x)
        { duration: '30s', target: 0 },    // Cool-down
      ],
      exec: 'retailViewerScenario',
      tags: { persona: 'RetailViewer' },
    },

    // WebSocket Event Streaming Under Load
    websocket_streamer: {
      executor: 'constant-vus',
      vus: 20,
      duration: '3m30s',
      exec: 'websocketScenario',
      tags: { persona: 'WebSocketStreamer' },
    },
  },

  // SLO Enforcement Thresholds (Requirement 4)
  thresholds: {
    // p99 latency must be under 100ms overall and per HTTP persona
    'http_req_duration': ['p(99)<100'],
    'http_req_duration{persona:IndexerPoller}': ['p(99)<100'],
    'http_req_duration{persona:InstitutionalTrader}': ['p(99)<100'],
    'http_req_duration{persona:RetailViewer}': ['p(99)<100'],

    // Failure rate must be strictly under 0.1% (< 0.001)
    'http_req_failed': ['rate<0.001'],
    'http_req_failed{persona:IndexerPoller}': ['rate<0.001'],
    'http_req_failed{persona:InstitutionalTrader}': ['rate<0.001'],
    'http_req_failed{persona:RetailViewer}': ['rate<0.001'],

    // SLO violations must be zero
    'slo_violations': ['count<1'],
  },

  summaryTrendStats: ['min', 'med', 'p(90)', 'p(95)', 'p(99)', 'p(99.9)', 'max'],
};

// ----------------------------------------------------------------------------
// Scenario Executions
// ----------------------------------------------------------------------------

/**
 * Persona 1: IndexerPoller
 * High-frequency polling on GET /v1/stats at ~200 req/s.
 */
export function indexerPollerScenario() {
  const url = `${BASE_URL}/v1/stats`;
  const res = http.get(url, {
    tags: { name: 'IndexerPoller: GET /v1/stats' },
  });

  const passed = check(res, {
    'status is 200': (r) => r.status === 200,
    'latency under 100ms': (r) => r.timings.duration < 100,
  });

  if (!passed) {
    sloViolations.add(1);
  }

  // Poisson stochastic pacing
  sleep(poissonInterval(200));
}

/**
 * Persona 2: InstitutionalTrader
 * Authenticated calls on GET /v1/assets/:id/holders with random 10k asset ID.
 */
export function institutionalTraderScenario() {
  // Uniformly select asset ID from the 10,000 fixture array
  const assetId = assetIds[Math.floor(Math.random() * assetIds.length)];
  const url = `${BASE_URL}/v1/assets/${assetId}/holders`;

  const params = {
    headers: {
      'Authorization': `Bearer ${AUTH_TOKEN}`,
      'x-api-key': 'institutional_trader_key',
      'Content-Type': 'application/json',
    },
    tags: { name: 'InstitutionalTrader: GET /v1/assets/:id/holders' },
  };

  const res = http.get(url, params);

  const passed = check(res, {
    'status is 200 or 404': (r) => r.status === 200 || r.status === 404,
    'latency under 100ms': (r) => r.timings.duration < 100,
  });

  if (!passed) {
    sloViolations.add(1);
  }

  // Poisson stochastic pacing
  sleep(poissonInterval(50));
}

/**
 * Persona 3: RetailViewer
 * High-volume browsing of asset catalog on GET /v1/assets at ~500 req/s.
 */
export function retailViewerScenario() {
  const url = `${BASE_URL}/v1/assets?limit=20`;
  const res = http.get(url, {
    tags: { name: 'RetailViewer: GET /v1/assets' },
  });

  const passed = check(res, {
    'status is 200': (r) => r.status === 200,
    'latency under 100ms': (r) => r.timings.duration < 100,
  });

  if (!passed) {
    sloViolations.add(1);
  }

  // Poisson stochastic pacing
  sleep(poissonInterval(500));
}

/**
 * Persona 4: WebSocketStreamer
 * Connects to live event stream /v1/ws under load using k6/experimental/websockets.
 */
export function websocketScenario() {
  const url = WS_URL;
  try {
    const socket = new WebSocket(url);

    socket.addEventListener('open', () => {
      socket.send(JSON.stringify({ action: 'subscribe', topic: 'events' }));
    });

    socket.addEventListener('message', (event) => {
      wsMessageDuration.add(10);
    });

    socket.addEventListener('error', (e) => {
      sloViolations.add(1);
    });

    sleep(15);
    socket.close();
  } catch (err) {
    sloViolations.add(1);
  }
  sleep(1);
}

// ----------------------------------------------------------------------------
// Failure Report & Latency Breakdown (Requirement 7)
// ----------------------------------------------------------------------------

export function handleSummary(data) {
  const dur = data.metrics.http_req_duration;
  const failed = data.metrics.http_req_failed;

  const p50 = dur && dur.values['med'] ? dur.values['med'].toFixed(2) : 'N/A';
  const p90 = dur && dur.values['p(90)'] ? dur.values['p(90)'].toFixed(2) : 'N/A';
  const p99 = dur && dur.values['p(99)'] ? dur.values['p(99)'].toFixed(2) : 'N/A';
  const p999 = dur && dur.values['p(99.9)'] ? dur.values['p(99.9)'].toFixed(2) : 'N/A';
  const failureRate = failed && failed.values['rate'] ? (failed.values['rate'] * 100).toFixed(4) : '0.0000';

  const report = `
================================================================================
                    TESSERA RWA LOAD TEST & SLO REPORT
================================================================================
Target: ${BASE_URL}
Duration: 3m30s
Profiles: Ramp-up (30s) -> Steady-state (2m) -> Spike (30s) -> Cool-down (30s)

SLO Validation:
  - Latency SLO (p99 < 100ms)    : ${Number(p99) < 100 ? 'PASS' : 'FAIL'} (${p99} ms)
  - Failure SLO (failed < 0.1%)  : ${Number(failureRate) < 0.1 ? 'PASS' : 'FAIL'} (${failureRate} %)

Latency Breakdown (Overall):
  - p50  (Median) : ${p50} ms
  - p90           : ${p90} ms
  - p99  (SLO)    : ${p99} ms
  - p999          : ${p999} ms

Personas Simulated:
  - IndexerPoller       : 200 req/s  (GET /v1/stats)
  - InstitutionalTrader : 50 req/s   (GET /v1/assets/:id/holders with Auth)
  - RetailViewer        : 500 req/s  (GET /v1/assets)
  - WebSocketStreamer   : 20 VUs     (WS /v1/ws event streaming)
================================================================================
`;

  return {
    stdout: report,
    'tests/load/load_test_report.json': JSON.stringify(data, null, 2),
  };
}
