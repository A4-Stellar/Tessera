/**
 * Lightweight SEP-24 smoke test.
 *
 * Uses only Node built-ins (node:test + fetch). It skips automatically when
 * SEP24_BASE_URL is not set, so a plain `node --test` run stays green.
 *
 *   SEP24_BASE_URL=http://localhost:8080 node --test tests/sep24/smoke.test.mjs
 */

import test from "node:test";
import assert from "node:assert/strict";

const BASE_URL = process.env.SEP24_BASE_URL;
const ACCOUNT =
  process.env.SEP24_TEST_ACCOUNT ??
  "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA";

const smokeTest = BASE_URL ? test : test.skip;

async function request(path, { method = "GET", body, headers = {} } = {}) {
  const response = await fetch(`${BASE_URL.replace(/\/$/, "")}${path}`, {
    method,
    headers: body ? { "content-type": "application/json", ...headers } : headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // Plain-text responses (stellar.toml) are returned as-is.
  }
  return { status: response.status, contentType: response.headers.get("content-type") ?? "", text, json };
}

async function startDeposit(lang = "en") {
  const response = await request("/sep24/transactions/deposit/interactive", {
    method: "POST",
    body: {
      account: ACCOUNT,
      asset_code: process.env.SEP24_TEST_ASSET ?? "USDC",
      amount: "10",
      lang,
    },
  });
  assert.equal(response.status, 200, `deposit failed: ${response.text}`);
  assert.equal(response.json.type, "interactive");
  return response.json;
}

smokeTest("stellar.toml advertises the required SEP-24 fields", async () => {
  const response = await request("/.well-known/stellar.toml");
  assert.equal(response.status, 200, response.text);
  assert.match(response.contentType, /^text\/plain/);
  for (const field of [
    "SIGNING_KEY=",
    "TRANSFER_SERVER_SEP0024=",
    "KYC_SERVER=",
    "NETWORK_PASSPHRASE=",
    "WEB_AUTH_ENDPOINT=",
  ]) {
    assert.ok(response.text.includes(field), `stellar.toml missing ${field}`);
  }
});

smokeTest("deposit creates an interactive URL and a pending transaction", async () => {
  const created = await startDeposit("en");
  assert.ok(created.id, "missing transaction id");
  assert.ok(created.token, "missing one-time token");
  assert.ok(created.url.includes(`transaction_id=${created.id}`));
  assert.ok(created.url.includes(`token=${created.token}`));

  const lookup = await request(
    `/sep24/transaction?id=${encodeURIComponent(created.id)}&token=${encodeURIComponent(created.token)}`,
  );
  assert.equal(lookup.status, 200, lookup.text);
  assert.equal(lookup.json.transaction.status, "pending_user_transfer_start");
  assert.equal(lookup.json.transaction.amount_in, "10");
});

smokeTest("KYC, external confirmation and completion update the status", async () => {
  const created = await startDeposit("en");
  const id = created.id;

  const kyc = await request(`/sep24/transactions/${encodeURIComponent(id)}/kyc`, {
    method: "POST",
    body: {
      first_name: "Ada",
      last_name: "Lovelace",
      dob: "1815-12-10",
      country: "GB",
      id_type: "passport",
      id_number: "P1234567",
      tax_id: "GB123456789",
    },
  });
  assert.equal(kyc.status, 200, kyc.text);
  assert.ok(
    ["pending", "needs_review"].includes(kyc.json.transaction.kyc_status),
    `unexpected kyc_status: ${kyc.json.transaction.kyc_status}`,
  );

  const confirmed = await request(`/sep24/transactions/${encodeURIComponent(id)}/confirm_external`, {
    method: "POST",
  });
  assert.equal(confirmed.status, 200, confirmed.text);
  assert.equal(confirmed.json.transaction.status, "pending_external");

  const done = await request(`/sep24/transactions/${encodeURIComponent(id)}/complete`, {
    method: "POST",
  });
  assert.equal(done.status, 200, done.text);
  assert.equal(done.json.transaction.status, "completed");
  assert.equal(done.json.transaction.amount_out, "10");
});

smokeTest("list by account and reject a bad token", async () => {
  const created = await startDeposit("en");
  const listed = await request(`/sep24/transactions?account=${encodeURIComponent(ACCOUNT)}`);
  assert.equal(listed.status, 200, listed.text);
  assert.ok(
    listed.json.transactions.some((tx) => tx.id === created.id),
    "new deposit not present in account listing",
  );

  const bad = await request(
    `/sep24/transaction?id=${encodeURIComponent(created.id)}&token=not-the-token`,
  );
  assert.equal(bad.status, 401, bad.text);
});

smokeTest("re-verification admin endpoints respond", async () => {
  const due = await request("/sep24/admin/reverify/due");
  assert.equal(due.status, 200, due.text);
  assert.ok(Array.isArray(due.json.transactions));

  const run = await request("/sep24/admin/reverify", { method: "POST" });
  assert.equal(run.status, 200, run.text);
  assert.equal(typeof run.json.marked, "number");
});

smokeTest("malformed deposit is rejected", async () => {
  const bad = await request("/sep24/transactions/deposit/interactive", {
    method: "POST",
    body: { account: "not-a-stellar-key", asset_code: "USDC" },
  });
  assert.equal(bad.status, 400, bad.text);
});
