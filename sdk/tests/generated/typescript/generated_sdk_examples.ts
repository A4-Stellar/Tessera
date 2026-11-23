import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - all', () => {
  it('executes TypeScript example from all block 1', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\api-rate-limits.mdx:98
    const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
    
    const etagCache = new Map<string, { etag: string; data: unknown }>();
    
    export async function fetchWithEtag<T>(path: string): Promise<T> {
      const url = `${API}${path}`;
      const cached = etagCache.get(url);
    
      const headers: HeadersInit = cached
        ? { "If-None-Match": cached.etag }
        : {};
    
      const res = await fetch(url, { headers });
    
      if (res.status === 304 && cached) {
        // Server confirmed nothing changed — reuse the cached body.
        return cached.data as T;
      }
    
      if (!res.ok) throw new Error(`API ${res.status}`);
    
      const data = (await res.json()) as T;
      const etag = res.headers.get("ETag");
      if (etag) etagCache.set(url, { etag, data });
    
      return data;
    }
  });

  it('executes TypeScript example from all block 2', async () => {
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

  it('executes TypeScript example from all block 3', async () => {
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

  it('executes TypeScript example from all block 4', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\contracts-dividend.mdx:97
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

  it('executes TypeScript example from all block 5', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\contracts-registry.mdx:95
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

  it('executes TypeScript example from all block 6', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:36
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
    
    export function displaySupply(a: Asset): string {
      const raw = BigInt(a.total_supply);
      const base = 10n ** BigInt(a.decimals);
      return (Number(raw) / Number(base)).toLocaleString();
    }
  });

  it('executes TypeScript example from all block 7', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:122
    import {
      Contract, TransactionBuilder, Account, BASE_FEE,
      Networks, rpc, scValToNative,
    } from "@stellar/stellar-sdk";
    
    const server = new rpc.Server("https://soroban-testnet.stellar.org");
    const REGISTRY = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3";
    
    export async function totalValueLocked(): Promise<bigint> {
      const contract = new Contract(REGISTRY);
      // Any valid account works as the simulation source.
      const source = new Account(
        "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA", "0",
      );
      const tx = new TransactionBuilder(source, {
        fee: BASE_FEE, networkPassphrase: Networks.TESTNET,
      })
        .addOperation(contract.call("total_value_locked"))
        .setTimeout(30)
        .build();
    
      const sim = await server.simulateTransaction(tx);
      if (rpc.Api.isSimulationError(sim)) throw new Error(sim.error);
      return scValToNative(sim.result!.retval) as bigint; // USD cents
    }
  });

  it('executes TypeScript example from all block 8', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:152
    import { Address, nativeToScVal } from "@stellar/stellar-sdk";
    
    const COMPLIANCE = "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU";
    
    const op = new Contract(COMPLIANCE).call(
      "is_allowed",
      new Address(investor).toScVal(),
    );
    // build → simulate → scValToNative(retval) === boolean
  });

  it('executes TypeScript example from all block 9', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\integration.mdx:169
    import {
      Contract, TransactionBuilder, Networks, BASE_FEE, rpc, Address, nativeToScVal,
    } from "@stellar/stellar-sdk";
    import { signTransaction } from "@stellar/freighter-api";
    
    const server = new rpc.Server("https://soroban-testnet.stellar.org");
    
    export async function transfer(
      token: string, from: string, to: string, amount: bigint,
    ): Promise<string> {
      const account = await server.getAccount(from);
      const op = new Contract(token).call(
        "transfer",
        new Address(from).toScVal(),
        new Address(to).toScVal(),
        nativeToScVal(amount, { type: "i128" }),
      );
      const built = new TransactionBuilder(account, {
        fee: BASE_FEE, networkPassphrase: Networks.TESTNET,
      }).addOperation(op).setTimeout(60).build();
    
      // Simulate + assemble to attach the Soroban footprint and resource fees.
      const sim = await server.simulateTransaction(built);
      if (rpc.Api.isSimulationError(sim)) throw new Error(sim.error);
      const prepared = rpc.assembleTransaction(built, sim).build();
    
      const { signedTxXdr } = await signTransaction(prepared.toXDR(), {
        networkPassphrase: Networks.TESTNET, address: from,
      });
      const signed = TransactionBuilder.fromXDR(signedTxXdr, Networks.TESTNET);
      const sent = await server.sendTransaction(signed);
      return sent.hash; // then poll server.getTransaction(hash)
    }
  });

  it('executes TypeScript example from all block 10', async () => {
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

  it('executes TypeScript example from all block 11', async () => {
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

  it('executes TypeScript example from all block 12', async () => {
    // Source: C:\Users\hp\.gemini\antigravity\scratch\Tessera\docs\content\en\docs\time-and-ledgers.mdx:129
    const server = new Horizon.Server("https://horizon.stellar.org");
    const ledger = await server.ledgers().ledger(3502885).call();
    
    console.log(ledger.closed_at);  // "2026-05-15T10:30:00Z"
  });
});
