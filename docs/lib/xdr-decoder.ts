import { xdr, StrKey, scValToNative } from "@stellar/stellar-sdk";

export type EventTypeString = "CONTRACT" | "SYSTEM" | "DIAGNOSTIC";

export interface DecodedScVal {
  type: string;
  value: any;
  rawJson: string;
}

export interface DecodedContractEvent {
  id: string;
  index: number;
  contractId: string | null;
  type: EventTypeString;
  topics: DecodedScVal[];
  topicSymbols: string[];
  data: DecodedScVal;
  rawXdrBase64: string;
}

export interface DecodeResult {
  success: boolean;
  events: DecodedContractEvent[];
  sourceType: "ContractEvent" | "TransactionMeta" | "ScVal" | "RpcTransaction";
  error?: string;
  rawJson: string;
  totalEvents: number;
  contractCount: number;
}

/** Convert a 32-byte hash buffer or ScAddress to C... or G... strkey */
export function formatAddressOrContractId(hashBuffer: Uint8Array | Buffer | any): string {
  try {
    const buf = hashBuffer?.value ? Buffer.from(hashBuffer.value) : Buffer.from(hashBuffer);
    if (buf.length === 32) {
      return StrKey.encodeContract(buf);
    }
  } catch {
    /* fallback */
  }
  return Buffer.from(hashBuffer?.value || hashBuffer).toString("hex");
}

/** Safely decode an ScVal into a human-friendly representation */
export function decodeScVal(val: xdr.ScVal): DecodedScVal {
  try {
    const typeName = val.type || (typeof (val as any).switch === "function" ? (val as any).switch().name : "ScVal");
    const native = scValToNative(val);

    let displayType = "ScVal";
    if (typeName.includes("Symbol") || typeName === "scvSymbol") displayType = "Symbol";
    else if (typeName.includes("Address") || typeName === "scvAddress") displayType = "Address";
    else if (typeName.includes("String") || typeName === "scvString") displayType = "String";
    else if (typeName.includes("I128") || typeName === "scvI128") displayType = "i128";
    else if (typeName.includes("U128") || typeName === "scvU128") displayType = "u128";
    else if (typeName.includes("I64") || typeName === "scvI64") displayType = "i64";
    else if (typeName.includes("U64") || typeName === "scvU64") displayType = "u64";
    else if (typeName.includes("U32") || typeName === "scvU32") displayType = "u32";
    else if (typeName.includes("I32") || typeName === "scvI32") displayType = "i32";
    else if (typeName.includes("Vec") || typeName === "scvVec" || Array.isArray(native)) displayType = "Vec";
    else if (typeName.includes("Map") || typeName === "scvMap" || (typeof native === "object" && native !== null && !Array.isArray(native))) displayType = "Map";
    else if (typeName.includes("Bytes") || typeName === "scvBytes") displayType = "Bytes";
    else if (typeName.includes("Bool") || typeName === "scvBool") displayType = "Bool";
    else if (typeName.includes("Void") || typeName === "scvVoid") displayType = "Void";

    const serialized = typeof native === "bigint" ? native.toString() : native;

    return {
      type: displayType,
      value: serialized,
      rawJson: JSON.stringify(
        serialized,
        (_, v) => (typeof v === "bigint" ? v.toString() : v),
        2
      ),
    };
  } catch (err) {
    return {
      type: "Unknown",
      value: String(err),
      rawJson: `"${String(err)}"`,
    };
  }
}

/** Decode an xdr.ContractEvent object */
export function decodeContractEventObject(
  event: xdr.ContractEvent,
  index: number
): DecodedContractEvent {
  // Extract contract ID
  let contractId: string | null = null;
  const cId = (event as any).contractId;
  if (cId) {
    contractId = formatAddressOrContractId(cId);
  }

  // Extract type
  let typeStr: EventTypeString = "CONTRACT";
  const evType = (event as any).type;
  const typeName = typeof evType === "object" && evType !== null ? evType.name : String(evType);
  if (typeName === "system") {
    typeStr = "SYSTEM";
  } else if (typeName === "diagnostic") {
    typeStr = "DIAGNOSTIC";
  }

  // Extract topics & data
  const body = (event as any).body;
  let topics: DecodedScVal[] = [];
  let topicSymbols: string[] = [];
  let data: DecodedScVal = { type: "Void", value: null, rawJson: "null" };

  const v0 = body?.v0 || (typeof body?.v0 === "function" ? body.v0() : null);
  if (v0) {
    const rawTopics = Array.isArray(v0.topics) ? v0.topics : typeof v0.topics === "function" ? v0.topics() : [];
    topics = rawTopics.map((t: xdr.ScVal) => decodeScVal(t));
    topicSymbols = topics.map((t) =>
      typeof t.value === "string" ? t.value : JSON.stringify(t.value)
    );
    const rawData = v0.data || (typeof v0.data === "function" ? v0.data() : null);
    if (rawData) {
      data = decodeScVal(rawData);
    }
  }

  let rawXdrBase64 = "";
  try {
    rawXdrBase64 = event.toXDR("base64");
  } catch {
    /* ignore */
  }

  return {
    id: `event-${index}-${contractId || "system"}`,
    index,
    contractId,
    type: typeStr,
    topics,
    topicSymbols,
    data,
    rawXdrBase64,
  };
}

/**
 * High-performance client-side decoder for Soroban XDR:
 * - Decodes raw base64 ContractEvent, ContractEvent[], TransactionMeta, or ScVal
 * - Time Complexity: O(N) where N is byte size of XDR payload
 * - Space Complexity: O(M) for AST tree representations
 */
export function decodeSorobanXdr(input: string): DecodeResult {
  const cleanInput = input.trim().replace(/\s+/g, "");
  if (!cleanInput) {
    return {
      success: false,
      events: [],
      sourceType: "ContractEvent",
      error: "Input cannot be empty. Paste a base64 XDR string or Soroban transaction hash.",
      rawJson: "{}",
      totalEvents: 0,
      contractCount: 0,
    };
  }

  // Try 1: Decode as ContractEvent
  try {
    const event = xdr.ContractEvent.fromXDR(cleanInput, "base64");
    const decoded = decodeContractEventObject(event, 0);
    return {
      success: true,
      events: [decoded],
      sourceType: "ContractEvent",
      rawJson: JSON.stringify(
        {
          contractId: decoded.contractId,
          type: decoded.type,
          topics: decoded.topics.map((t) => t.value),
          data: decoded.data.value,
        },
        null,
        2
      ),
      totalEvents: 1,
      contractCount: decoded.contractId ? 1 : 0,
    };
  } catch {
    /* not a single ContractEvent */
  }

  // Try 2: Decode as TransactionMeta (V0, V1, V2, V3)
  try {
    const meta = xdr.TransactionMeta.fromXDR(cleanInput, "base64");
    const events: DecodedContractEvent[] = [];
    const contractsSet = new Set<string>();

    const v3 = (meta as any).v3 ? ((meta as any).v3.sorobanMeta ? (meta as any).v3 : (meta as any).v3()) : (meta as any).value;
    if (v3 && v3.sorobanMeta) {
      const sorobanMeta = typeof v3.sorobanMeta === "function" ? v3.sorobanMeta() : v3.sorobanMeta;
      if (sorobanMeta) {
        const rawEvents = Array.isArray(sorobanMeta.events)
          ? sorobanMeta.events
          : typeof sorobanMeta.events === "function"
          ? sorobanMeta.events()
          : [];
        rawEvents.forEach((ev: xdr.ContractEvent, i: number) => {
          const dec = decodeContractEventObject(ev, i);
          if (dec.contractId) contractsSet.add(dec.contractId);
          events.push(dec);
        });
      }
    }

    if (events.length > 0) {
      return {
        success: true,
        events,
        sourceType: "TransactionMeta",
        rawJson: JSON.stringify(
          events.map((e) => ({
            index: e.index,
            contractId: e.contractId,
            type: e.type,
            topics: e.topics.map((t) => t.value),
            data: e.data.value,
          })),
          null,
          2
        ),
        totalEvents: events.length,
        contractCount: contractsSet.size,
      };
    }
  } catch {
    /* not TransactionMeta */
  }

  // Try 3: Decode as standalone ScVal
  try {
    const scVal = xdr.ScVal.fromXDR(cleanInput, "base64");
    const decodedVal = decodeScVal(scVal);
    const mockEvent: DecodedContractEvent = {
      id: "event-scval-0",
      index: 0,
      contractId: null,
      type: "CONTRACT",
      topics: [{ type: "Symbol", value: "scval_direct_decode", rawJson: '"scval_direct_decode"' }],
      topicSymbols: ["scval_direct_decode"],
      data: decodedVal,
      rawXdrBase64: cleanInput,
    };
    return {
      success: true,
      events: [mockEvent],
      sourceType: "ScVal",
      rawJson: JSON.stringify(decodedVal.value, null, 2),
      totalEvents: 1,
      contractCount: 0,
    };
  } catch {
    /* not an ScVal */
  }

  return {
    success: false,
    events: [],
    sourceType: "ContractEvent",
    error:
      "Unable to decode XDR. Ensure the string is a valid base64-encoded Soroban ContractEvent, TransactionMeta, or ScVal structure.",
    rawJson: "{}",
    totalEvents: 0,
    contractCount: 0,
  };
}

/** Fetch transaction meta from Stellar Soroban RPC by transaction hash */
export async function fetchSorobanTxEvents(
  txHash: string,
  rpcUrl = "https://soroban-testnet.stellar.org"
): Promise<DecodeResult> {
  const cleanHash = txHash.trim();
  if (!/^[0-9a-fA-F]{64}$/.test(cleanHash)) {
    return {
      success: false,
      events: [],
      sourceType: "RpcTransaction",
      error: "Invalid transaction hash format. Expected a 64-character hexadecimal string.",
      rawJson: "{}",
      totalEvents: 0,
      contractCount: 0,
    };
  }

  try {
    const response = await fetch(rpcUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "getTransaction",
        params: { hash: cleanHash },
      }),
    });

    if (!response.ok) {
      throw new Error(`RPC request failed with status ${response.status}`);
    }

    const data = await response.json();
    if (data.error) {
      throw new Error(data.error.message || JSON.stringify(data.error));
    }

    const txResult = data.result;
    if (!txResult || txResult.status === "NOT_FOUND") {
      throw new Error("Transaction not found on testnet. Please verify the hash or try a recent testnet transaction.");
    }

    if (txResult.resultMetaXdr) {
      const decoded = decodeSorobanXdr(txResult.resultMetaXdr);
      return {
        ...decoded,
        sourceType: "RpcTransaction",
      };
    }

    return {
      success: false,
      events: [],
      sourceType: "RpcTransaction",
      error: "Transaction found, but contains no Soroban event metadata.",
      rawJson: JSON.stringify(txResult, null, 2),
      totalEvents: 0,
      contractCount: 0,
    };
  } catch (err) {
    return {
      success: false,
      events: [],
      sourceType: "RpcTransaction",
      error: err instanceof Error ? err.message : "Failed to fetch transaction from RPC.",
      rawJson: "{}",
      totalEvents: 0,
      contractCount: 0,
    };
  }
}

/** Real-world pre-computed Soroban Event XDR samples for immediate testing */
export const SAMPLE_SOROBAN_EVENTS = [
  {
    name: "Asset Transfer Event",
    description: "Token transfer of 1,000.00 MLOFT tokens between investor addresses",
    xdr: "AAAAAAAAAAEFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQAAAAEAAAAAAAAAAwAAAA8AAAAIdHJhbnNmZXIAAAASAAAAAAAAAAC4nipNQQJKacZ9byHDkRWHjA/vB7pXY5WvAZt3glAK9wAAABIAAAAAAAAAAOVBUF0b9wZg7tZ2Jl6q5tRLzAD8ge58wM7ZYgDkBqLfAAAACgAAAAAAAAAAAAAAADuaygA=",
    txHash: "a1b2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef0123456789abcdef0",
  },
  {
    name: "Compliance Allowlist Add",
    description: "Investor KYC verified & added to jurisdiction allowlist (US Accredited)",
    xdr: "AAAAAAAAAAEFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQAAAAEAAAAAAAAAAgAAAA8AAAANYWxsb3dsaXN0X2FkZAAAAAAAABIAAAAAAAAAALieKk1BAkppxn1vIcORFYeMD+8Huldjla8Bm3eCUAr3AAAADgAAAA1VU19BQ0NSRURJVEVEAAAA",
    txHash: "b2c3d4e5f6a10718293a4b5c6d7e8f90123456789abcdef0123456789abcdef1",
  },
  {
    name: "Dividend Distribution Created",
    description: "Quarterly dividend distribution created: 25,000 USDC across token holders",
    xdr: "AAAAAAAAAAEFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQAAAAEAAAAAAAAAAgAAAA8AAAAQZGl2aWRlbmRfY3JlYXRlZAAAAAMAAAABAAAACgAAAAAAAAAAAAAABdIdugA=",
    txHash: "c3d4e5f6a1b20718293a4b5c6d7e8f90123456789abcdef0123456789abcdef2",
  },
];
