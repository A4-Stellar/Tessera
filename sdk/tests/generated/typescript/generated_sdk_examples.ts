import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - all', () => {
  it('executes TypeScript example from all block 1', async () => {
    const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
    export interface Asset {
      id: number;
      token_contract: string;
      name: string;
      symbol: string;
      asset_type: string;
      valuation_cents: string;
      valuation_usd: number;
      decimals: number;
      total_supply: string;
      holders: number;
      active: boolean;
      paused: boolean;
      compliance_contract: string;
    }
    export async function getAssets(): Promise<Asset[]> {
      const res = await fetch(`${API}/assets`, { cache: "no-store" });
      if (!res.ok) throw new Error(`API ${res.status}`);
      return res.json();
    }
  });

  it('executes TypeScript example from all block 2', async () => {
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
