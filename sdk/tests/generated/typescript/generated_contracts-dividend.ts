import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - contracts-dividend', () => {
  it('executes TypeScript example from contracts-dividend block 1', async () => {
    // Source: docs/content/en/docs/contracts-dividend.mdx:97
    const dividend = new Contract("CAR4XY3C…2SYX");
    // issuer funds a 10,000-unit distribution in the payment token
    const create = dividend.call(
      "create_distribution",
      new Address(admin).toScVal(),
      new Address(assetToken).toScVal(),
      new Address(paymentToken).toScVal(),
      nativeToScVal(10_000_0000000n, { type: "i128" }), // 10,000 @ 7 decimals
    );
    
    // a holder later claims their share
    const claim = dividend.call(
      "claim",
      nativeToScVal(1n, { type: "u64" }),
      new Address(holder).toScVal(),
    );
  });
});
