import { contract, xdr } from "@stellar/stellar-sdk";
import {
  BinaryReader,
  bytesToString,
  WasmDisassembler,
  type IExportEntry,
  type IImportEntry,
  type ISectionInformation,
  type ITypeEntry,
  type Type,
} from "wasmparser";
import { resolveHostFunction, SOROBAN_HOST_MODULES } from "@/lib/soroban-host-functions";
import { SOROBAN_MAINNET_LIMITS } from "@/lib/soroban-resources";

// wasmparser declares its enums as ambient `const enum`s, which `isolatedModules`
// cannot reference, so the WebAssembly binary-format values used here are
// mirrored locally (https://webassembly.github.io/spec/core/binary/).
const ReaderState = {
  ERROR: -1,
  BEGIN_SECTION: 3,
  SECTION_RAW_DATA: 7,
  TYPE_SECTION_ENTRY: 11,
  IMPORT_SECTION_ENTRY: 12,
  FUNCTION_SECTION_ENTRY: 13,
  EXPORT_SECTION_ENTRY: 17,
} as const;
const CUSTOM_SECTION_ID = 0;
/** Type, import, function and export sections: everything the interface view reads. */
const INTERFACE_SECTION_IDS = new Set([1, 2, 3, 7]);
const EXTERNAL_KINDS = ["function", "table", "memory", "global", "tag"] as const;
const VALUE_TYPES: Readonly<Record<number, string>> = {
  [-1]: "i32",
  [-2]: "i64",
  [-3]: "f32",
  [-4]: "f64",
  [-5]: "v128",
  [-16]: "funcref",
  [-17]: "externref",
};

const SECTION_ID_NAMES: Readonly<Record<number, string>> = {
  0: "custom",
  1: "type",
  2: "import",
  3: "function",
  4: "table",
  5: "memory",
  6: "global",
  7: "export",
  8: "start",
  9: "element",
  10: "code",
  11: "data",
  12: "datacount",
};

/** `\0asm` followed by binary-format version 1. */
const WASM_HEADER = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/** Custom sections the Soroban SDK embeds in every contract. */
export const SOROBAN_CUSTOM_SECTIONS = {
  spec: "contractspecv0",
  meta: "contractmetav0",
  envMeta: "contractenvmetav0",
  envMetaLegacy: "contractenvmeta",
  budget: "soroban_budget",
} as const;

type SorobanSectionName = (typeof SOROBAN_CUSTOM_SECTIONS)[keyof typeof SOROBAN_CUSTOM_SECTIONS];
const SOROBAN_SECTION_NAMES = new Set<string>(Object.values(SOROBAN_CUSTOM_SECTIONS));

export type ExternalKindName = (typeof EXTERNAL_KINDS)[number];

export interface WasmImport {
  module: string;
  field: string;
  kind: ExternalKindName;
  /** WASM-level signature, e.g. `(i64, i64) -> i64`; `null` for non-functions. */
  signature: string | null;
  /** Canonical Soroban host function, when the import is a known host call. */
  hostFunction: { module: string; name: string; signature: string } | null;
}

export interface WasmExport {
  name: string;
  kind: ExternalKindName;
  index: number;
  signature: string | null;
}

export type SpecEntryKind = "function" | "struct" | "union" | "enum" | "error" | "event";

export interface SpecEntrySummary {
  kind: SpecEntryKind;
  name: string;
  doc: string;
  /** One rendered line per signature, field, case or parameter. */
  details: string[];
}

export interface WasmInspection {
  byteLength: number;
  imports: WasmImport[];
  exports: WasmExport[];
  /** Names of every custom section in the module, in binary order. */
  customSections: string[];
  spec: SpecEntrySummary[];
  /** `contractmetav0` key/value pairs such as `rsver` and `rssdkver`. */
  meta: { key: string; value: string }[];
  /** Soroban environment interface version from `contractenvmetav0`. */
  interfaceVersion: { protocol: number; preRelease: number } | null;
  /** Non-fatal decoding problems (e.g. a missing or malformed spec section). */
  warnings: string[];
}

export type BudgetCategoryKey = "compute" | "memory" | "storage" | "host_calls" | "crypto";

export interface BudgetCategoryBreakdown {
  category: BudgetCategoryKey;
  label: string;
  instructions: number;
  limit: number;
  fractionOfTotal: number;
  fractionOfLimit: number;
}

export interface SorobanBudgetReport {
  source: "soroban_budget" | "static_analysis";
  totalInstructions: number;
  instructionLimit: number;
  memBytes: number;
  memoryLimit: number;
  categories: BudgetCategoryBreakdown[];
}

export interface ContractMetadataSummary {
  contractName: string;
  sdkVersion: string;
  interfaceHash: string;
  protocolVersion: number | null;
  preRelease: number;
  rustVersion: string;
  byteLength: number;
  customSections: string[];
  rawMeta: { key: string; value: string }[];
  envMetaEntries: { key: string; value: string }[];
}

export interface WasmAstInstruction {
  type: "Instr";
  id: string;
  args: (string | number)[];
  line: number;
}

export interface WasmAstSection {
  id: number;
  name: string;
  byteOffset: number;
  byteLength: number;
}

export interface WasmModuleAst {
  type: "Program";
  body: [
    {
      type: "Module";
      id: string | null;
      fields: unknown[];
      sections: WasmAstSection[];
      customSections: { name: string; byteLength: number; payload: Uint8Array }[];
      instructions: WasmAstInstruction[];
    }
  ];
}

export interface HostFunctionCallRecord {
  module: string;
  field: string;
  canonicalName: string;
  args: string[];
  returnValue: string;
}

export interface WasmExecutionTrace {
  functionName: string;
  returnValue: string | number | null;
  rawReturnValue: unknown;
  hostCalls: HostFunctionCallRecord[];
  instructionsExecuted: number;
  memoryBytes: number;
}

export class WasmInspectionError extends Error {}

export function hasWasmHeader(bytes: Uint8Array): boolean {
  return bytes.length >= WASM_HEADER.length && WASM_HEADER.every((byte, index) => bytes[index] === byte);
}

function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

function createReader(bytes: Uint8Array): BinaryReader {
  const reader = new BinaryReader();
  reader.setData(toArrayBuffer(bytes), 0, bytes.byteLength, true);
  return reader;
}

function assertNoReaderError(reader: BinaryReader): void {
  if (reader.state === ReaderState.ERROR) {
    throw new WasmInspectionError(`Malformed WASM: ${reader.error?.message ?? "unknown parse error"}`);
  }
}

function valueTypeName(type: Type): string {
  return VALUE_TYPES[type.kind] ?? (type.isIndex ? `(ref ${type.index})` : `type(${type.kind})`);
}

function formatFuncType(entry: ITypeEntry | undefined): string | null {
  if (!entry?.params || !entry.returns) return null;
  const params = entry.params.map(valueTypeName).join(", ");
  const returns = entry.returns.map(valueTypeName);
  return `(${params}) -> ${returns.length === 1 ? returns[0] : `(${returns.join(", ")})`}`;
}

/** Concatenates repeated custom sections: each is an XDR stream, so the join is too. */
function concatChunks(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) return chunks[0];
  const joined = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return joined;
}

const PRIMITIVE_SPEC_TYPES: Readonly<Record<string, string>> = {
  scSpecTypeVal: "Val",
  scSpecTypeBool: "bool",
  scSpecTypeVoid: "()",
  scSpecTypeError: "Error",
  scSpecTypeU32: "u32",
  scSpecTypeI32: "i32",
  scSpecTypeU64: "u64",
  scSpecTypeI64: "i64",
  scSpecTypeTimepoint: "Timepoint",
  scSpecTypeDuration: "Duration",
  scSpecTypeU128: "u128",
  scSpecTypeI128: "i128",
  scSpecTypeU256: "U256",
  scSpecTypeI256: "I256",
  scSpecTypeBytes: "Bytes",
  scSpecTypeString: "String",
  scSpecTypeSymbol: "Symbol",
  scSpecTypeAddress: "Address",
  scSpecTypeMuxedAddress: "MuxedAddress",
};

/** Renders a contract spec type using the soroban-sdk (Rust) type names. */
export function formatSpecType(type: xdr.ScSpecTypeDef): string {
  switch (type.type) {
    case "scSpecTypeOption":
      return `Option<${formatSpecType(type.option.valueType)}>`;
    case "scSpecTypeResult":
      return `Result<${formatSpecType(type.result.okType)}, ${formatSpecType(type.result.errorType)}>`;
    case "scSpecTypeVec":
      return `Vec<${formatSpecType(type.vec.elementType)}>`;
    case "scSpecTypeMap":
      return `Map<${formatSpecType(type.map.keyType)}, ${formatSpecType(type.map.valueType)}>`;
    case "scSpecTypeTuple":
      return `(${type.tuple.valueTypes.map(formatSpecType).join(", ")})`;
    case "scSpecTypeBytesN":
      return `BytesN<${type.bytesN.n}>`;
    case "scSpecTypeUdt":
      return type.udt.name.toString();
    default:
      return PRIMITIVE_SPEC_TYPES[type.type] ?? type.type;
  }
}

function summarizeSpecEntry(entry: xdr.ScSpecEntry): SpecEntrySummary {
  switch (entry.type) {
    case "scSpecEntryFunctionV0": {
      const fn = entry.functionV0;
      const name = fn.name.toString();
      const inputs = fn.inputs.map((input) => `${input.name.toString()}: ${formatSpecType(input.type)}`).join(", ");
      const output = fn.outputs.length > 0 ? ` -> ${fn.outputs.map(formatSpecType).join(", ")}` : "";
      return { kind: "function", name, doc: fn.doc.toString(), details: [`fn ${name}(${inputs})${output}`] };
    }
    case "scSpecEntryUdtStructV0": {
      const udt = entry.udtStructV0;
      return {
        kind: "struct",
        name: udt.name.toString(),
        doc: udt.doc.toString(),
        details: udt.fields.map((field) => `${field.name.toString()}: ${formatSpecType(field.type)}`),
      };
    }
    case "scSpecEntryUdtUnionV0": {
      const udt = entry.udtUnionV0;
      return {
        kind: "union",
        name: udt.name.toString(),
        doc: udt.doc.toString(),
        details: udt.cases.map((unionCase) =>
          unionCase.type === "scSpecUdtUnionCaseTupleV0"
            ? `${unionCase.tupleCase.name.toString()}(${unionCase.tupleCase.type.map(formatSpecType).join(", ")})`
            : unionCase.voidCase.name.toString()
        ),
      };
    }
    case "scSpecEntryUdtEnumV0": {
      const udt = entry.udtEnumV0;
      return {
        kind: "enum",
        name: udt.name.toString(),
        doc: udt.doc.toString(),
        details: udt.cases.map((enumCase) => `${enumCase.name.toString()} = ${enumCase.value}`),
      };
    }
    case "scSpecEntryUdtErrorEnumV0": {
      const udt = entry.udtErrorEnumV0;
      return {
        kind: "error",
        name: udt.name.toString(),
        doc: udt.doc.toString(),
        details: udt.cases.map((errorCase) => `${errorCase.name.toString()} = ${errorCase.value}`),
      };
    }
    case "scSpecEntryEventV0": {
      const event = entry.eventV0;
      const topics = event.prefixTopics.map((topic) => topic.toString());
      return {
        kind: "event",
        name: event.name.toString(),
        doc: event.doc.toString(),
        details: [
          ...(topics.length > 0 ? [`topics: [${topics.join(", ")}]`] : []),
          ...event.params.map((param) => `${param.name.toString()}: ${formatSpecType(param.type)}`),
        ],
      };
    }
  }
}

/** Parses an arbitrary custom section payload as UTF-8 JSON or key=value pairs when not XDR. */
function tryDecodeTextEntries(payload: Uint8Array): { key: string; value: string }[] | null {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(payload).trim();
    if (!text) return null;
    if (text.startsWith("{") && text.endsWith("}")) {
      const parsed = JSON.parse(text) as Record<string, unknown>;
      return Object.entries(parsed).map(([key, value]) => ({
        key,
        value: typeof value === "string" ? value : JSON.stringify(value),
      }));
    }
    if (text.includes("=") && /^[\x20-\x7e\r\n\t]+$/.test(text)) {
      const pairs: { key: string; value: string }[] = [];
      for (const rawLine of text.split(/\r?\n|;/)) {
        const line = rawLine.trim();
        if (!line) continue;
        const eq = line.indexOf("=");
        if (eq > 0) {
          pairs.push({ key: line.slice(0, eq).trim(), value: line.slice(eq + 1).trim() });
        }
      }
      if (pairs.length > 0) return pairs;
    }
  } catch {
    // Not valid UTF-8 text or JSON
  }
  return null;
}

/** Reads all custom section raw payloads from a WebAssembly binary. */
function readCustomSectionMap(bytes: Uint8Array): Map<string, Uint8Array[]> {
  const reader = createReader(bytes);
  const map = new Map<string, Uint8Array[]>();
  let pendingName: string | null = null;

  while (reader.read()) {
    switch (reader.state) {
      case ReaderState.BEGIN_SECTION: {
        const section = reader.result as ISectionInformation;
        if (section.id === CUSTOM_SECTION_ID) {
          pendingName = bytesToString(section.name);
          reader.fetchSectionRawData();
        } else {
          reader.skipSection();
        }
        break;
      }
      case ReaderState.SECTION_RAW_DATA:
        if (pendingName !== null) {
          const chunks = map.get(pendingName) ?? [];
          chunks.push(reader.result as Uint8Array);
          map.set(pendingName, chunks);
          pendingName = null;
        }
        break;
    }
  }
  assertNoReaderError(reader);
  return map;
}

/**
 * Decodes a contract's interface in a single pass over the module: function
 * types, imports (resolved to Soroban host functions), exports, and the
 * Soroban custom sections. Code and data sections are skipped, so the cost is
 * O(n) in the module size with O(types + imports + exports) extra memory.
 */
export function inspectWasm(bytes: Uint8Array): WasmInspection {
  if (!hasWasmHeader(bytes)) {
    throw new WasmInspectionError("Not a WebAssembly module: missing the `\\0asm` version 1 header.");
  }

  const reader = createReader(bytes);
  const types: ITypeEntry[] = [];
  const functionTypeIndices: number[] = [];
  const rawImports: IImportEntry[] = [];
  const rawExports: IExportEntry[] = [];
  const customSections: string[] = [];
  const sorobanSections = new Map<SorobanSectionName, Uint8Array[]>();
  let pendingSorobanSection: SorobanSectionName | null = null;

  while (reader.read()) {
    switch (reader.state) {
      case ReaderState.BEGIN_SECTION: {
        const section = reader.result as ISectionInformation;
        if (section.id === CUSTOM_SECTION_ID) {
          const name = bytesToString(section.name);
          customSections.push(name);
          if (SOROBAN_SECTION_NAMES.has(name)) {
            pendingSorobanSection = name as SorobanSectionName;
            reader.fetchSectionRawData();
          } else {
            reader.skipSection();
          }
        } else if (!INTERFACE_SECTION_IDS.has(section.id)) {
          // Function bodies, data and the remaining sections are not part of
          // the interface view; skipping them keeps this pass cheap.
          reader.skipSection();
        }
        break;
      }
      case ReaderState.SECTION_RAW_DATA:
        if (pendingSorobanSection) {
          const chunks = sorobanSections.get(pendingSorobanSection) ?? [];
          chunks.push(reader.result as Uint8Array);
          sorobanSections.set(pendingSorobanSection, chunks);
          pendingSorobanSection = null;
        }
        break;
      case ReaderState.TYPE_SECTION_ENTRY:
        types.push(reader.result as ITypeEntry);
        break;
      case ReaderState.IMPORT_SECTION_ENTRY: {
        const entry = reader.result as IImportEntry;
        rawImports.push(entry);
        if (entry.kind === 0) functionTypeIndices.push(entry.funcTypeIndex ?? -1);
        break;
      }
      case ReaderState.FUNCTION_SECTION_ENTRY:
        functionTypeIndices.push((reader.result as { typeIndex: number }).typeIndex);
        break;
      case ReaderState.EXPORT_SECTION_ENTRY:
        rawExports.push(reader.result as IExportEntry);
        break;
    }
  }
  assertNoReaderError(reader);

  const imports: WasmImport[] = rawImports.map((entry) => {
    const moduleName = bytesToString(entry.module);
    const field = bytesToString(entry.field);
    const isFunction = entry.kind === 0;
    return {
      module: moduleName,
      field,
      kind: EXTERNAL_KINDS[entry.kind] ?? "function",
      signature: isFunction ? formatFuncType(types[entry.funcTypeIndex ?? -1]) : null,
      hostFunction: isFunction ? resolveHostFunction(moduleName, field) : null,
    };
  });

  const exports: WasmExport[] = rawExports.map((entry) => ({
    name: bytesToString(entry.field),
    kind: EXTERNAL_KINDS[entry.kind] ?? "function",
    index: entry.index,
    signature: entry.kind === 0 ? formatFuncType(types[functionTypeIndices[entry.index]]) : null,
  }));

  const warnings: string[] = [];
  const sectionBytes = (name: SorobanSectionName) => {
    const chunks = sorobanSections.get(name);
    return chunks ? concatChunks(chunks) : null;
  };

  let spec: SpecEntrySummary[] = [];
  const specBytes = sectionBytes(SOROBAN_CUSTOM_SECTIONS.spec);
  if (!specBytes) {
    warnings.push("No `contractspecv0` section: this module does not look like a Soroban contract.");
  } else {
    try {
      spec = new contract.Spec(specBytes).entries.map(summarizeSpecEntry);
    } catch (error) {
      warnings.push(`Could not decode \`contractspecv0\`: ${(error as Error).message}`);
    }
  }

  let meta: WasmInspection["meta"] = [];
  const metaBytes = sectionBytes(SOROBAN_CUSTOM_SECTIONS.meta);
  if (metaBytes) {
    try {
      meta = xdr.decodeStream(xdr.ScMetaEntry, metaBytes).map((entry) => ({
        key: entry.v0.key.toString(),
        value: entry.v0.val.toString(),
      }));
    } catch (error) {
      const textEntries = tryDecodeTextEntries(metaBytes);
      if (textEntries) {
        meta = textEntries;
      } else {
        warnings.push(`Could not decode \`contractmetav0\`: ${(error as Error).message}`);
      }
    }
  }

  let interfaceVersion: WasmInspection["interfaceVersion"] = null;
  const envMetaBytes = sectionBytes(SOROBAN_CUSTOM_SECTIONS.envMeta) ?? sectionBytes(SOROBAN_CUSTOM_SECTIONS.envMetaLegacy);
  if (envMetaBytes) {
    try {
      const [first] = xdr.decodeStream(xdr.ScEnvMetaEntry, envMetaBytes);
      if (first) {
        interfaceVersion = {
          protocol: first.interfaceVersion.protocol,
          preRelease: first.interfaceVersion.preRelease,
        };
      }
    } catch (error) {
      const textEntries = tryDecodeTextEntries(envMetaBytes);
      if (textEntries) {
        const map = new Map(textEntries.map((e) => [e.key, e.value]));
        const protocol = Number(map.get("protocol") ?? map.get("interface_version") ?? map.get("protocolVersion") ?? NaN);
        const preRelease = Number(map.get("pre_release") ?? map.get("preRelease") ?? 0);
        if (Number.isFinite(protocol)) {
          interfaceVersion = { protocol, preRelease: Number.isFinite(preRelease) ? preRelease : 0 };
        }
        for (const entry of textEntries) {
          if (!meta.some((m) => m.key === entry.key)) {
            meta.push(entry);
          }
        }
      } else {
        warnings.push(`Could not decode \`contractenvmetav0\`: ${(error as Error).message}`);
      }
    }
  }

  return { byteLength: bytes.byteLength, imports, exports, customSections, spec, meta, interfaceVersion, warnings };
}

/**
 * Disassembles a module into WebAssembly Text Format, one entry per line, in a
 * single linear pass: O(n) time, O(output lines) memory.
 */
export function disassembleWasm(bytes: Uint8Array): string[] {
  const disassembler = new WasmDisassembler();
  disassembler.addOffsets = false;

  const reader = createReader(bytes);
  if (!disassembler.disassembleChunk(reader)) {
    assertNoReaderError(reader);
    throw new WasmInspectionError("Malformed WASM: the module ended before disassembly completed.");
  }
  return disassembler.getResult().lines;
}

/**
 * Decodes a WebAssembly binary into a `@webassemblyjs/wasm-parser`-compatible
 * module AST containing section headers, custom sections, module fields, and
 * disassembled instruction nodes.
 */
export function decodeWasmAst(bytes: Uint8Array): WasmModuleAst {
  const inspection = inspectWasm(bytes);
  const lines = disassembleWasm(bytes);
  const reader = createReader(bytes);
  const sections: WasmAstSection[] = [];
  const customSections: { name: string; byteLength: number; payload: Uint8Array }[] = [];
  let pendingCustomName: string | null = null;

  while (reader.read()) {
    switch (reader.state) {
      case ReaderState.BEGIN_SECTION: {
        const section = reader.result as ISectionInformation;
        const sectionName =
          section.id === CUSTOM_SECTION_ID ? bytesToString(section.name) : (SECTION_ID_NAMES[section.id] ?? `section_${section.id}`);
        sections.push({
          id: section.id,
          name: sectionName,
          byteOffset: reader.position,
          byteLength: (section as { length?: number }).length ?? 0,
        });
        if (section.id === CUSTOM_SECTION_ID) {
          pendingCustomName = sectionName;
          reader.fetchSectionRawData();
        } else {
          reader.skipSection();
        }
        break;
      }
      case ReaderState.SECTION_RAW_DATA:
        if (pendingCustomName !== null) {
          const payload = reader.result as Uint8Array;
          customSections.push({ name: pendingCustomName, byteLength: payload.byteLength, payload });
          pendingCustomName = null;
        }
        break;
    }
  }
  assertNoReaderError(reader);

  const instructions: WasmAstInstruction[] = [];
  lines.forEach((rawLine, index) => {
    const trimmed = rawLine.trim();
    if (!trimmed || trimmed.startsWith("(module") || trimmed.startsWith("(type") || trimmed.startsWith("(import") || trimmed.startsWith("(export") || trimmed.startsWith("(func") || trimmed === ")") {
      return;
    }
    const tokens = trimmed.replace(/[()]/g, "").split(/\s+/).filter(Boolean);
    if (tokens.length > 0) {
      instructions.push({
        type: "Instr",
        id: tokens[0],
        args: tokens.slice(1).map((arg) => (/^-?\d+$/.test(arg) ? Number(arg) : arg)),
        line: index + 1,
      });
    }
  });

  const fields: unknown[] = [
    ...inspection.imports.map((imp) => ({ type: "ModuleImport", module: imp.module, name: imp.field, descr: { type: imp.kind } })),
    ...inspection.exports.map((exp) => ({ type: "ModuleExport", name: exp.name, descr: { type: exp.kind, id: exp.index } })),
  ];

  return {
    type: "Program",
    body: [
      {
        type: "Module",
        id: null,
        fields,
        sections,
        customSections,
        instructions,
      },
    ],
  };
}

const BUDGET_CATEGORY_DEFS: readonly { key: BudgetCategoryKey; label: string }[] = [
  { key: "compute", label: "Compute & arithmetic" },
  { key: "memory", label: "Linear memory" },
  { key: "storage", label: "Ledger storage" },
  { key: "host_calls", label: "Host invocations" },
  { key: "crypto", label: "Cryptography & auth" },
];

function buildBudgetReport(
  source: "soroban_budget" | "static_analysis",
  counts: Record<BudgetCategoryKey, number>,
  memBytes = 0,
  instructionLimit = SOROBAN_MAINNET_LIMITS.limits.instructions,
  memoryLimit = SOROBAN_MAINNET_LIMITS.limits.memBytes
): SorobanBudgetReport {
  const totalInstructions = BUDGET_CATEGORY_DEFS.reduce((sum, { key }) => sum + Math.max(0, Math.round(counts[key] ?? 0)), 0);
  const categories: BudgetCategoryBreakdown[] = BUDGET_CATEGORY_DEFS.map(({ key, label }) => {
    const instructions = Math.max(0, Math.round(counts[key] ?? 0));
    return {
      category: key,
      label,
      instructions,
      limit: instructionLimit,
      fractionOfTotal: totalInstructions > 0 ? instructions / totalInstructions : 0,
      fractionOfLimit: instructionLimit > 0 ? instructions / instructionLimit : 0,
    };
  });
  return {
    source,
    totalInstructions,
    instructionLimit,
    memBytes: Math.max(0, Math.round(memBytes)),
    memoryLimit,
    categories,
  };
}

/**
 * Parses the `soroban_budget` custom section emitted by `soroban build --debug`.
 * Supports JSON payloads, `key=value` text records, and packed binary u32/u64 tables.
 * Returns `null` when the module does not contain a `soroban_budget` custom section.
 */
export function parseSorobanBudgetSection(bytes: Uint8Array): SorobanBudgetReport | null {
  if (!hasWasmHeader(bytes)) {
    throw new WasmInspectionError("Not a WebAssembly module: missing the `\\0asm` version 1 header.");
  }

  const customSections = readCustomSectionMap(bytes);
  const chunks = customSections.get(SOROBAN_CUSTOM_SECTIONS.budget);
  if (!chunks || chunks.length === 0) return null;

  const payload = concatChunks(chunks);
  const counts: Record<BudgetCategoryKey, number> = {
    compute: 0,
    memory: 0,
    storage: 0,
    host_calls: 0,
    crypto: 0,
  };
  let memBytes = 0;
  let instructionLimit = SOROBAN_MAINNET_LIMITS.limits.instructions;
  let memoryLimit = SOROBAN_MAINNET_LIMITS.limits.memBytes;

  const textEntries = tryDecodeTextEntries(payload);
  if (textEntries) {
    const map = new Map(textEntries.map(({ key, value }) => [key.toLowerCase(), value]));
    const parseNum = (val: string | undefined): number => {
      if (!val) return 0;
      const n = Number(val);
      return Number.isFinite(n) ? n : 0;
    };

    if (map.has("categories")) {
      try {
        const nested = JSON.parse(map.get("categories")!) as Record<string, number>;
        counts.compute = Number(nested.compute ?? nested.cpu ?? 0);
        counts.memory = Number(nested.memory ?? nested.mem ?? 0);
        counts.storage = Number(nested.storage ?? nested.ledger ?? 0);
        counts.host_calls = Number(nested.host_calls ?? nested.hostCalls ?? nested.host ?? 0);
        counts.crypto = Number(nested.crypto ?? 0);
      } catch {
        // ignore nested parse error
      }
    } else {
      counts.compute = parseNum(map.get("compute") ?? map.get("cpu") ?? map.get("arithmetic"));
      counts.memory = parseNum(map.get("memory") ?? map.get("mem"));
      counts.storage = parseNum(map.get("storage") ?? map.get("ledger"));
      counts.host_calls = parseNum(map.get("host_calls") ?? map.get("hostcalls") ?? map.get("host"));
      counts.crypto = parseNum(map.get("crypto") ?? map.get("auth"));
    }

    memBytes = parseNum(map.get("mem_bytes") ?? map.get("membytes") ?? map.get("memory_bytes"));
    const customInstrLimit = parseNum(map.get("instruction_limit") ?? map.get("max_instructions"));
    if (customInstrLimit > 0) instructionLimit = customInstrLimit;
    const customMemLimit = parseNum(map.get("memory_limit") ?? map.get("max_mem_bytes"));
    if (customMemLimit > 0) memoryLimit = customMemLimit;

    return buildBudgetReport("soroban_budget", counts, memBytes, instructionLimit, memoryLimit);
  }

  // Binary table fallback: 5 x u32 (20 bytes) or 5 x u64 (40 bytes) little-endian
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  if (payload.byteLength >= 40) {
    counts.compute = Number(view.getBigUint64(0, true));
    counts.memory = Number(view.getBigUint64(8, true));
    counts.storage = Number(view.getBigUint64(16, true));
    counts.host_calls = Number(view.getBigUint64(24, true));
    counts.crypto = Number(view.getBigUint64(32, true));
    if (payload.byteLength >= 48) {
      memBytes = Number(view.getBigUint64(40, true));
    }
  } else if (payload.byteLength >= 20) {
    counts.compute = view.getUint32(0, true);
    counts.memory = view.getUint32(4, true);
    counts.storage = view.getUint32(8, true);
    counts.host_calls = view.getUint32(12, true);
    counts.crypto = view.getUint32(16, true);
    if (payload.byteLength >= 24) {
      memBytes = view.getUint32(20, true);
    }
  }

  return buildBudgetReport("soroban_budget", counts, memBytes, instructionLimit, memoryLimit);
}

/**
 * Extracts the `soroban_budget` custom section if present, or synthesizes an
 * instruction category budget breakdown from the disassembled WASM instructions
 * and Soroban host function imports.
 */
export function analyzeWasmInstructionBudget(bytes: Uint8Array): SorobanBudgetReport {
  const fromSection = parseSorobanBudgetSection(bytes);
  if (fromSection) return fromSection;

  const inspection = inspectWasm(bytes);
  const lines = disassembleWasm(bytes);
  const counts: Record<BudgetCategoryKey, number> = {
    compute: 0,
    memory: 0,
    storage: 0,
    host_calls: 0,
    crypto: 0,
  };

  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!line || line.startsWith("(module") || line.startsWith("(type") || line.startsWith("(import") || line.startsWith("(export") || line.startsWith("(func") || line === ")") {
      continue;
    }
    if (/\b(?:load|store|memory\.size|memory\.grow|memory\.copy|memory\.fill)\b/.test(line)) {
      counts.memory += 1500;
    } else if (/\bcall\b/.test(line)) {
      counts.host_calls += 5000;
    } else {
      counts.compute += 500;
    }
  }

  for (const imp of inspection.imports) {
    const mod = imp.hostFunction?.module ?? imp.module;
    if (mod === "ledger" || mod === "l") {
      counts.storage += 15000;
    } else if (mod === "crypto" || mod === "c" || mod === "address" || mod === "a") {
      counts.crypto += 20000;
    } else if (mod === "buf" || mod === "b" || mod === "vec" || mod === "v" || mod === "map" || mod === "m") {
      counts.memory += 4000;
    } else {
      counts.host_calls += 3500;
    }
  }

  return buildBudgetReport("static_analysis", counts, inspection.byteLength * 64);
}

/** Deterministic 64-bit FNV-1a hex digest (`0x` + 16 hex chars) for contract interface hashing. */
function computeFnv1a64Hex(input: string): string {
  let hash = BigInt("0xcbf29ce484222325");
  const prime = BigInt("0x100000001b3");
  const mask = BigInt("0xffffffffffffffff");
  for (let i = 0; i < input.length; i++) {
    hash ^= BigInt(input.charCodeAt(i));
    hash = (hash * prime) & mask;
  }
  return `0x${hash.toString(16).padStart(16, "0")}`;
}

/**
 * Extracts `contractenvmeta` / `contractenvmetav0` and `contractmetav0` metadata
 * from a `.wasm` binary or a pre-decoded `WasmInspection` object, returning
 * SDK version, contract name, deterministic interface hash, protocol version,
 * and Rust compiler version.
 */
export function extractContractMetadata(
  bytesOrInspection: Uint8Array | WasmInspection,
  fileName?: string
): ContractMetadataSummary {
  let inspection: WasmInspection;
  const envMetaEntries: { key: string; value: string }[] = [];

  if (bytesOrInspection instanceof Uint8Array) {
    inspection = inspectWasm(bytesOrInspection);
    const customMap = readCustomSectionMap(bytesOrInspection);
    const legacyChunks = customMap.get(SOROBAN_CUSTOM_SECTIONS.envMetaLegacy);
    if (legacyChunks) {
      const parsed = tryDecodeTextEntries(concatChunks(legacyChunks));
      if (parsed) envMetaEntries.push(...parsed);
    }
    const v0Chunks = customMap.get(SOROBAN_CUSTOM_SECTIONS.envMeta);
    if (v0Chunks) {
      const parsed = tryDecodeTextEntries(concatChunks(v0Chunks));
      if (parsed) envMetaEntries.push(...parsed);
    }
  } else {
    inspection = bytesOrInspection;
  }

  if (inspection.interfaceVersion && !envMetaEntries.some((e) => e.key === "protocol")) {
    envMetaEntries.unshift(
      { key: "protocol", value: String(inspection.interfaceVersion.protocol) },
      { key: "pre_release", value: String(inspection.interfaceVersion.preRelease) }
    );
  }

  const allMeta = new Map<string, string>();
  for (const entry of envMetaEntries) allMeta.set(entry.key.toLowerCase(), entry.value);
  for (const entry of inspection.meta) allMeta.set(entry.key.toLowerCase(), entry.value);

  const rawSdk =
    allMeta.get("rssdkver") ??
    allMeta.get("sdk_version") ??
    allMeta.get("soroban_sdk") ??
    allMeta.get("sdkversion") ??
    "—";
  const sdkVersion = rawSdk !== "—" ? rawSdk.split("#")[0] : "—";

  const rustVersion =
    allMeta.get("rsver") ??
    allMeta.get("rust_version") ??
    allMeta.get("rustc") ??
    "—";

  const cleanFileName = fileName ? fileName.replace(/\.wasm$/i, "") : "";
  const specFallbackName =
    inspection.spec.find((s) => s.kind === "struct")?.name ??
    inspection.spec.find((s) => s.kind === "function")?.name ??
    inspection.exports.find((e) => e.kind === "function")?.name ??
    "";

  const contractName =
    allMeta.get("contract_name") ??
    allMeta.get("contractname") ??
    allMeta.get("name") ??
    allMeta.get("pkg") ??
    allMeta.get("crate") ??
    (cleanFileName || specFallbackName || "Unnamed contract");

  const protocolVersion =
    inspection.interfaceVersion?.protocol ??
    (allMeta.has("protocol") ? Number(allMeta.get("protocol")) : null);
  const preRelease =
    inspection.interfaceVersion?.preRelease ??
    (allMeta.has("pre_release") ? Number(allMeta.get("pre_release")) : 0);

  const explicitHash = allMeta.get("interface_hash") ?? allMeta.get("interfacehash");
  const interfaceDescriptor = JSON.stringify({
    protocol: protocolVersion,
    preRelease,
    spec: inspection.spec.map((s) => ({ kind: s.kind, name: s.name, details: s.details })),
    exports: inspection.exports.map((e) => ({ name: e.name, kind: e.kind, sig: e.signature })),
    imports: inspection.imports.map((i) => ({ module: i.module, field: i.field, sig: i.signature })),
  });
  const interfaceHash = explicitHash ?? computeFnv1a64Hex(interfaceDescriptor);

  return {
    contractName,
    sdkVersion,
    interfaceHash,
    protocolVersion,
    preRelease,
    rustVersion,
    byteLength: inspection.byteLength,
    customSections: inspection.customSections,
    rawMeta: inspection.meta,
    envMetaEntries,
  };
}

/**
 * Instantiates and executes a compiled `.wasm` binary inside an isolated
 * WebAssembly runtime populated with mock Soroban host functions (`SOROBAN_HOST_MODULES`),
 * recording all host function invocations and the returned value.
 */
export async function executeWasmWithMockHost(
  bytes: Uint8Array,
  fnName?: string,
  args: (number | bigint)[] = []
): Promise<WasmExecutionTrace> {
  const inspection = inspectWasm(bytes);
  const lines = disassembleWasm(bytes);
  const hostCalls: HostFunctionCallRecord[] = [];
  const imports: WebAssembly.Imports = {};

  const ensureModule = (modName: string): Record<string, WebAssembly.ImportValue> => {
    if (!imports[modName]) imports[modName] = {};
    return imports[modName] as Record<string, WebAssembly.ImportValue>;
  };

  // Populate every Soroban host module from SOROBAN_HOST_MODULES
  for (const [modKey, hostMod] of Object.entries(SOROBAN_HOST_MODULES)) {
    const modObj = ensureModule(modKey);
    for (const [fieldKey, [canonicalFnName, sig]] of Object.entries(hostMod.functions)) {
      const canonicalName = `${hostMod.name}::${canonicalFnName}`;
      const returnsVoid = sig.endsWith("-> Void");
      modObj[fieldKey] = (...callArgs: unknown[]) => {
        let ret: bigint | number | undefined;
        if (returnsVoid) {
          ret = undefined;
        } else {
          const sum = callArgs.reduce<bigint>((acc, val) => {
            if (typeof val === "bigint") return acc + val;
            if (typeof val === "number") return acc + BigInt(Math.trunc(val));
            return acc;
          }, 0n);
          ret = sum !== 0n ? sum : 1n;
        }
        hostCalls.push({
          module: modKey,
          field: fieldKey,
          canonicalName,
          args: callArgs.map((a) => String(a)),
          returnValue: ret === undefined ? "void" : String(ret),
        });
        return ret;
      };
    }
  }

  // Ensure all declared imports in the WASM binary have matching mock implementations
  for (const imp of inspection.imports) {
    const modObj = ensureModule(imp.module);
    if (imp.kind === "function") {
      const canonicalName = imp.hostFunction
        ? `${imp.hostFunction.module}::${imp.hostFunction.name}`
        : `${imp.module}.${imp.field}`;
      const sig = imp.signature ?? "";
      const returnsI32 = sig.endsWith("-> i32");
      const returnsVoid = sig.endsWith("-> ()");
      modObj[imp.field] = (...callArgs: unknown[]) => {
        let ret: bigint | number | undefined;
        if (returnsVoid) {
          ret = undefined;
        } else if (returnsI32) {
          const sum = callArgs.reduce<number>((acc, val) => acc + Number(val ?? 0), 0);
          ret = sum !== 0 ? sum : 1;
        } else {
          const sum = callArgs.reduce<bigint>((acc, val) => {
            if (typeof val === "bigint") return acc + val;
            if (typeof val === "number") return acc + BigInt(Math.trunc(val));
            return acc;
          }, 0n);
          ret = sum !== 0n ? sum : 1n;
        }
        hostCalls.push({
          module: imp.module,
          field: imp.field,
          canonicalName,
          args: callArgs.map((a) => String(a)),
          returnValue: ret === undefined ? "void" : String(ret),
        });
        return ret;
      };
    } else if (imp.kind === "memory" && !modObj[imp.field]) {
      modObj[imp.field] = new WebAssembly.Memory({ initial: 1 });
    }
  }

  const instantiated = await WebAssembly.instantiate(toArrayBuffer(bytes), imports);
  const instance =
    "instance" in instantiated
      ? (instantiated as WebAssembly.WebAssemblyInstantiatedSource).instance
      : (instantiated as WebAssembly.Instance);

  const targetExport =
    inspection.exports.find((e) => e.kind === "function" && e.name === fnName) ??
    inspection.exports.find((e) => e.kind === "function");

  if (!targetExport) {
    throw new WasmInspectionError("No exported function found in WebAssembly module.");
  }

  const exportedFn = instance.exports[targetExport.name];
  if (typeof exportedFn !== "function") {
    throw new WasmInspectionError(`Export "${targetExport.name}" is not a callable function.`);
  }

  const paramTypes =
    targetExport.signature
      ?.match(/^\(([^)]*)\)/)?.[1]
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean) ?? [];

  const callArgs = paramTypes.map((pType, idx) => {
    const provided = args[idx] ?? idx + 1;
    if (pType === "i64") {
      return typeof provided === "bigint" ? provided : BigInt(Math.trunc(Number(provided) || 0));
    }
    return typeof provided === "bigint" ? Number(provided) : Number(provided) || 0;
  });

  const rawReturnValue = (exportedFn as (...fnArgs: (number | bigint)[]) => unknown)(...callArgs);
  const returnValue =
    typeof rawReturnValue === "bigint"
      ? rawReturnValue.toString()
      : typeof rawReturnValue === "number"
      ? rawReturnValue
      : null;

  const exportedMemory = Object.values(instance.exports).find(
    (val): val is WebAssembly.Memory => val instanceof WebAssembly.Memory
  );
  const memoryBytes = exportedMemory ? exportedMemory.buffer.byteLength : 65536;

  return {
    functionName: targetExport.name,
    returnValue,
    rawReturnValue,
    hostCalls,
    instructionsExecuted: Math.max(1, lines.length),
    memoryBytes,
  };
}
