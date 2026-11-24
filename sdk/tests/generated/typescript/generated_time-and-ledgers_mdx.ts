import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - time-and-ledgers_mdx', () => {
  it('executes TypeScript example from time-and-ledgers_mdx block 1', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\time-and-ledgers.mdx:36
    // Add a user to the allowlist with no expiry
    const op = compliance.call(
      "add_to_allowlist",
      admin,
      userAddress,
      "US",
      0  // expires_at = 0 means never expires
    );
  });

  it('executes TypeScript example from time-and-ledgers_mdx block 2', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\time-and-ledgers.mdx:49
    import { Horizon } from "@stellar/stellar-sdk";
    
    const server = new Horizon.Server("https://horizon.stellar.org");
    const ledgers = await server.ledgers().limit(1).order("desc").call();
    const now = ledgers.records[0].sequence;
    
    // Approve for 1 year (~6.3 million ledgers at ~5s per ledger)
    const expiresAt = now + 6_300_000;
    
    const op = compliance.call(
      "add_to_allowlist",
      admin,
      userAddress,
      "US",
      expiresAt
    );
  });

  it('executes TypeScript example from time-and-ledgers_mdx block 3', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\time-and-ledgers.mdx:129
    const server = new Horizon.Server("https://horizon.stellar.org");
    const ledger = await server.ledgers().ledger(3502885).call();
    
    console.log(ledger.closed_at);  // "2026-05-15T10:30:00Z"
  });
});
