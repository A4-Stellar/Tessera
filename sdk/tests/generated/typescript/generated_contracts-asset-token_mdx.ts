import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - contracts-asset-token_mdx', () => {
  it('executes TypeScript example from contracts-asset-token_mdx block 1', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\contracts-asset-token.mdx:146
    import { Contract, TransactionBuilder, nativeToScVal, Address } from "@stellar/stellar-sdk";
    
    const token = new Contract("CBMCWLSQ…4ECZ");
    const op = token.call(
      "transfer",
      new Address(from).toScVal(),
      new Address(to).toScVal(),
      nativeToScVal(10000n, { type: "i128" }), // 100.00 @ 2 decimals
    );
    // build → simulate → sign with Freighter → submit
  });
});
