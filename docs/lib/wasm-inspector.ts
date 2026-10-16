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
import { resolveHostFunction } from "@/lib/soroban-host-functions";

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

/** `\0asm` followed by binary-format version 1. */
const WASM_HEADER = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/** Custom sections the Soroban SDK embeds in every contract. */
export const SOROBAN_CUSTOM_SECTIONS = {
  spec: "contractspecv0",
  meta: "contractmetav0",
  envMeta: "contractenvmetav0",
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
      warnings.push(`Could not decode \`contractmetav0\`: ${(error as Error).message}`);
    }
  }

  let interfaceVersion: WasmInspection["interfaceVersion"] = null;
  const envMetaBytes = sectionBytes(SOROBAN_CUSTOM_SECTIONS.envMeta);
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
      warnings.push(`Could not decode \`contractenvmetav0\`: ${(error as Error).message}`);
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
