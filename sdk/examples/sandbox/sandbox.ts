// TypeScript sandbox example. Uses the production SDK unchanged; only the
// tenant header differs.
import { TesseraClient } from "../../typescript/src/index";

const API = process.env.TESSERA_SANDBOX_URL ?? "http://localhost:8081";
const TENANT = process.env.X_SANDBOX_TENANT_ID ?? "acme-1";

async function main(): Promise<void> {
  const client = new TesseraClient(API, {
    defaultHeaders: { "X-Sandbox-Tenant-ID": TENANT },
  });

  // Identical call shape to production.
  const stats = await client.getStats();
  const assets = await client.getAssets({ limit: 10 });
  console.log(`tenant=${TENANT} assets=${stats.totalAssets} first=${assets[0]?.id}`);

  // Reset is sandbox-only; it wipes and re-seeds this tenant in < 5 s.
  const response = await fetch(`${API}/sandbox/reset`, {
    method: "POST",
    headers: { "X-Sandbox-Tenant-ID": TENANT },
  });
  console.log("reset", response.status, await response.json());
}

void main();
