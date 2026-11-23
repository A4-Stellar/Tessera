import { TesseraClient } from '@tessera/sdk';
import { describe, it, expect } from 'vitest';

const API = process.env.NEXT_PUBLIC_API_BASE_URL ?? "http://localhost:8080";
const client = new TesseraClient(API);

describe('SDK TypeScript Examples - integration', () => {
  it('executes TypeScript example from integration block 1', async () => {
    // Source: docs/content/en/docs/integration.mdx:36
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

  it('executes TypeScript example from integration block 2', async () => {
    // Source: docs/content/en/docs/integration.mdx:122
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

  it('executes TypeScript example from integration block 3', async () => {
    // Source: docs/content/en/docs/integration.mdx:152
    import { Address, nativeToScVal } from "@stellar/stellar-sdk";
    
    const COMPLIANCE = "CBUERYDM7DXTZLLKDBRJKUBPFJ7M4OSUN4T7XKUARU345RLXNAIQD2IU";
    
    const op = new Contract(COMPLIANCE).call(
      "is_allowed",
      new Address(investor).toScVal(),
    );
    // build → simulate → scValToNative(retval) === boolean
  });

  it('executes TypeScript example from integration block 4', async () => {
    // Source: docs/content/en/docs/integration.mdx:169
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
});
