import { xdr } from "@stellar/stellar-sdk";

/** Per-invocation resources Soroban meters and caps per transaction. */
export type ResourceKey =
  | "instructions"
  | "memBytes"
  | "diskReadEntries"
  | "writeEntries"
  | "diskReadBytes"
  | "writeBytes"
  | "eventsBytes";

export type ResourceUsage = Record<ResourceKey, number>;

export interface ResourceDefinition {
  key: ResourceKey;
  label: string;
  unit: "instructions" | "bytes" | "entries";
  /** Network config setting that caps this resource per transaction. */
  limitSetting: string;
}

export const RESOURCE_DEFINITIONS: readonly ResourceDefinition[] = [
  { key: "instructions", label: "CPU instructions", unit: "instructions", limitSetting: "tx_max_instructions" },
  { key: "memBytes", label: "Memory", unit: "bytes", limitSetting: "tx_memory_limit" },
  { key: "diskReadEntries", label: "Disk read entries", unit: "entries", limitSetting: "tx_max_disk_read_entries" },
  { key: "diskReadBytes", label: "Disk read bytes", unit: "bytes", limitSetting: "tx_max_disk_read_bytes" },
  { key: "writeEntries", label: "Write entries", unit: "entries", limitSetting: "tx_max_write_ledger_entries" },
  { key: "writeBytes", label: "Write bytes", unit: "bytes", limitSetting: "tx_max_write_bytes" },
  { key: "eventsBytes", label: "Events size", unit: "bytes", limitSetting: "tx_max_contract_events_size_bytes" },
];

export interface NetworkLimitsSnapshot {
  network: string;
  protocolVersion: number | null;
  ledger: number;
  /** ISO date the values were read, or `null` when read live. */
  capturedAt: string | null;
  /** Per-transaction maxima. */
  limits: ResourceUsage;
  contractMaxSizeBytes: number;
}

/**
 * Mainnet per-transaction limits read from the network's `ConfigSetting`
 * ledger entries (via Soroban RPC `getLedgerEntries`) at ledger 64971567,
 * protocol 29. Validators can vote these values up or down, so components
 * that accept an `rpcUrl` refresh them live.
 */
export const SOROBAN_MAINNET_LIMITS: NetworkLimitsSnapshot = {
  network: "Mainnet",
  protocolVersion: 29,
  ledger: 64971567,
  capturedAt: "2026-10-16",
  limits: {
    instructions: 400_000_000,
    memBytes: 41_943_040,
    diskReadEntries: 200,
    diskReadBytes: 200_000,
    writeEntries: 200,
    writeBytes: 132_096,
    eventsBytes: 16_384,
  },
  contractMaxSizeBytes: 131_072,
};

export interface MethodMeasurement {
  contract: string;
  method: string;
  usage: ResourceUsage;
}

export interface ReleaseMeasurement {
  /** Short commit hash the contracts were built from. */
  id: string;
  label: string;
  date: string;
  methods: MethodMeasurement[];
}

function usage(
  instructions: number,
  memBytes: number,
  writeEntries: number,
  writeBytes: number,
  eventsBytes: number
): ResourceUsage {
  // Live Soroban state is served from memory since protocol 23, so these
  // invocations read nothing from disk.
  return { instructions, memBytes, diskReadEntries: 0, diskReadBytes: 0, writeEntries, writeBytes, eventsBytes };
}

/**
 * Resources metered for Tessera's core contracts at three source revisions.
 * Each revision's release WASM (`wasm32v1-none`, rustc 1.98.0, soroban-sdk
 * 25.3.2) was executed in the soroban-sdk test host and read back with
 * `env.cost_estimate().resources()`. Authorization was mocked, so ed25519
 * signature verification is not included.
 */
export const TESSERA_RESOURCE_RELEASES: readonly ReleaseMeasurement[] = [
  {
    id: "f07a850c",
    label: "Contracts scaffold",
    date: "2026-09-24",
    methods: [
      { contract: "compliance", method: "add_to_allowlist", usage: usage(604_549, 1_241_462, 3, 748, 140) },
      { contract: "compliance", method: "is_allowed", usage: usage(485_200, 1_233_366, 0, 0, 0) },
      { contract: "asset-token", method: "mint", usage: usage(1_172_790, 2_481_292, 3, 852, 124) },
      { contract: "asset-token", method: "transfer", usage: usage(1_632_978, 3_713_957, 3, 360, 168) },
      { contract: "registry", method: "register_asset", usage: usage(625_398, 1_251_080, 3, 760, 120) },
      { contract: "registry", method: "get_all_assets", usage: usage(509_767, 1_236_802, 0, 0, 0) },
    ],
  },
  {
    id: "b703efa8",
    label: "Holding-period lockups (#10)",
    date: "2026-09-24",
    methods: [
      { contract: "compliance", method: "add_to_allowlist", usage: usage(636_899, 1_246_059, 3, 788, 140) },
      { contract: "compliance", method: "is_allowed", usage: usage(508_351, 1_237_470, 0, 0, 0) },
      { contract: "asset-token", method: "mint", usage: usage(1_216_543, 2_487_727, 3, 852, 124) },
      { contract: "asset-token", method: "transfer", usage: usage(1_733_066, 3_727_173, 3, 360, 168) },
      { contract: "registry", method: "register_asset", usage: usage(626_298, 1_251_488, 3, 760, 120) },
      { contract: "registry", method: "get_all_assets", usage: usage(510_083, 1_236_870, 0, 0, 0) },
    ],
  },
  {
    id: "0a050f9a",
    label: "Compliance attestations & hooks",
    date: "2026-09-26",
    methods: [
      { contract: "compliance", method: "add_to_allowlist", usage: usage(743_686, 1_262_933, 3, 788, 140) },
      { contract: "compliance", method: "is_allowed", usage: usage(621_113, 1_254_607, 0, 0, 0) },
      { contract: "asset-token", method: "mint", usage: usage(1_528_176, 2_520_719, 4, 988, 124) },
      { contract: "asset-token", method: "transfer", usage: usage(2_304_134, 3_783_878, 5, 632, 168) },
      { contract: "registry", method: "register_asset", usage: usage(627_836, 1_252_712, 3, 760, 120) },
      { contract: "registry", method: "get_all_assets", usage: usage(510_399, 1_237_074, 0, 0, 0) },
    ],
  },
];

export type Severity = "ok" | "warning" | "critical";

export interface SeverityThresholds {
  /** Utilization (0–1) at which a resource is flagged as nearing its limit. */
  warning: number;
  /** Utilization (0–1) at which a resource is flagged as critical. */
  critical: number;
}

export const DEFAULT_THRESHOLDS: SeverityThresholds = { warning: 0.75, critical: 0.9 };

export interface ResourceUtilization {
  key: ResourceKey;
  used: number;
  limit: number;
  /** `used / limit`; may exceed 1 when a method would fail the limit. */
  fraction: number;
  severity: Severity;
}

export function classifyUtilization(fraction: number, thresholds: SeverityThresholds): Severity {
  if (fraction >= thresholds.critical) return "critical";
  if (fraction >= thresholds.warning) return "warning";
  return "ok";
}

/** Utilization of every resource for one method, highest first: O(r log r). */
export function rankUtilization(
  used: ResourceUsage,
  limits: ResourceUsage,
  thresholds: SeverityThresholds = DEFAULT_THRESHOLDS
): ResourceUtilization[] {
  return RESOURCE_DEFINITIONS.map(({ key }) => {
    const limit = limits[key];
    const fraction = limit > 0 ? used[key] / limit : 0;
    return { key, used: used[key], limit, fraction, severity: classifyUtilization(fraction, thresholds) };
  }).sort((a, b) => b.fraction - a.fraction);
}

/** Integer count of the method that fits in one transaction under every limit. */
export function callsPerTransaction(used: ResourceUsage, limits: ResourceUsage): number {
  return RESOURCE_DEFINITIONS.reduce((fits, { key }) => {
    if (used[key] <= 0) return fits;
    return Math.min(fits, Math.floor(limits[key] / used[key]));
  }, Number.POSITIVE_INFINITY);
}

const CONFIG_IDS = [
  xdr.ConfigSettingId.configSettingContractMaxSizeBytes,
  xdr.ConfigSettingId.configSettingContractComputeV0,
  xdr.ConfigSettingId.configSettingContractLedgerCostV0,
  xdr.ConfigSettingId.configSettingContractEventsV0,
];

async function rpcCall<T>(rpcUrl: string, method: string, params: unknown, signal?: AbortSignal): Promise<T> {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
    signal,
  });
  if (!response.ok) throw new Error(`Soroban RPC ${method} failed with HTTP ${response.status}`);
  const body = (await response.json()) as { result?: T; error?: { message?: string } };
  if (!body.result) throw new Error(body.error?.message ?? `Soroban RPC ${method} returned no result`);
  return body.result;
}

/**
 * Reads the current per-transaction limits from a Soroban RPC endpoint with
 * one `getLedgerEntries` call for the four relevant `ConfigSetting` entries.
 */
export async function fetchNetworkLimits(
  rpcUrl: string,
  network: string,
  signal?: AbortSignal
): Promise<NetworkLimitsSnapshot> {
  const keys = CONFIG_IDS.map((configSettingId) =>
    xdr.LedgerKey.configSetting(new xdr.LedgerKeyConfigSetting({ configSettingId })).toXDR("base64")
  );
  const [entries, latest] = await Promise.all([
    rpcCall<{ entries?: { xdr: string }[]; latestLedger: number }>(rpcUrl, "getLedgerEntries", { keys }, signal),
    rpcCall<{ protocolVersion: number }>(rpcUrl, "getLatestLedger", {}, signal),
  ]);

  const snapshot: NetworkLimitsSnapshot = {
    network,
    protocolVersion: latest.protocolVersion,
    ledger: entries.latestLedger,
    capturedAt: null,
    limits: { ...SOROBAN_MAINNET_LIMITS.limits },
    contractMaxSizeBytes: SOROBAN_MAINNET_LIMITS.contractMaxSizeBytes,
  };
  const found = new Set<string>();
  for (const entry of entries.entries ?? []) {
    const setting = xdr.LedgerEntryData.fromXDR(entry.xdr, "base64");
    if (setting.type !== "configSetting") continue;
    const config = setting.configSetting;
    found.add(config.type);
    switch (config.type) {
      case "configSettingContractMaxSizeBytes":
        snapshot.contractMaxSizeBytes = config.contractMaxSizeBytes;
        break;
      case "configSettingContractComputeV0":
        snapshot.limits.instructions = Number(config.contractCompute.txMaxInstructions);
        snapshot.limits.memBytes = config.contractCompute.txMemoryLimit;
        break;
      case "configSettingContractLedgerCostV0":
        snapshot.limits.diskReadEntries = config.contractLedgerCost.txMaxDiskReadEntries;
        snapshot.limits.diskReadBytes = config.contractLedgerCost.txMaxDiskReadBytes;
        snapshot.limits.writeEntries = config.contractLedgerCost.txMaxWriteLedgerEntries;
        snapshot.limits.writeBytes = config.contractLedgerCost.txMaxWriteBytes;
        break;
      case "configSettingContractEventsV0":
        snapshot.limits.eventsBytes = config.contractEvents.txMaxContractEventsSizeBytes;
        break;
    }
  }
  if (found.size !== CONFIG_IDS.length) {
    throw new Error("Soroban RPC did not return every network config setting");
  }
  return snapshot;
}
