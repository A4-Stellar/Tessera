import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - contracts-registry', () => {
  it('executes TypeScript example from contracts-registry block 1', async () => {
    // Source: docs/content/en/docs/contracts-registry.mdx:95
    const registry = new Contract("CBX5SMLT…PUR3");
    // register a freshly deployed asset token
    const op = registry.call(
      "register_asset",
      new Address(issuer).toScVal(),
      new Address(tokenContract).toScVal(),
      nativeToScVal("Manhattan Loft", { type: "string" }),
      nativeToScVal("real_estate", { type: "string" }),
      nativeToScVal(500_000_000n, { type: "i128" }), // $5,000,000 in cents
    );
  });
});
