import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - contracts-compliance_mdx', () => {
  it('executes TypeScript example from contracts-compliance_mdx block 1', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\contracts-compliance.mdx:118
    const compliance = new Contract("CBUERYDM…D2IU");
    const op = compliance.call(
      "add_to_allowlist",
      new Address(admin).toScVal(),
      new Address(investor).toScVal(),
      nativeToScVal("US", { type: "string" }),
      nativeToScVal(currentLedger + 6_300_000, { type: "u32" }),
    );
  });
});
