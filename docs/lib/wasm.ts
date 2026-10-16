/**
 * Client-side WebAssembly binary parser and WAT-style disassembler.
 *
 * Parses the standard binary sections (custom, type, import, function, table,
 * memory, global, export, start, element, code, data, datacount) and decodes
 * function bodies into human-readable WebAssembly Text (WAT) instructions.
 * No WebAssembly compilation is performed — the module is never instantiated.
 */

export interface WasmCustomSection {
  name: string;
  size: number;
  /** UTF-8 preview of the section payload (first 512 bytes). */
  preview: string;
  data: Uint8Array;
}

export interface WasmImportEntry {
  module: string;
  field: string;
  kind: "func" | "table" | "memory" | "global";
  detail: string;
}

export interface WasmExportEntry {
  name: string;
  kind: "func" | "table" | "memory" | "global";
  index: number;
}

export interface WasmFunctionBody {
  index: number;
  /** Exported name, when this function is exported. */
  exportName?: string;
  typeIndex: number;
  locals: string[];
  code: string[];
  /** Set when instruction decoding bailed out on an unknown opcode. */
  truncated?: boolean;
}

export interface WasmDataString {
  segment: number;
  offset: number;
  value: string;
}

export interface ParsedWasm {
  version: number;
  typeSignatures: string[];
  imports: WasmImportEntry[];
  exports: WasmExportEntry[];
  functions: WasmFunctionBody[];
  memories: string[];
  tables: string[];
  globals: string[];
  dataStrings: WasmDataString[];
  customSections: WasmCustomSection[];
  errors: string[];
}

export class WasmParseError extends Error {}

const WASM_MAGIC = [0x00, 0x61, 0x73, 0x6d];

class Reader {
  private view: DataView;
  offset = 0;

  constructor(private bytes: Uint8Array) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  get remaining(): number {
    return this.bytes.length - this.offset;
  }

  u8(): number {
    if (this.offset >= this.bytes.length) {
      throw new WasmParseError(`Unexpected end of binary at offset ${this.offset}`);
    }
    return this.bytes[this.offset++];
  }

  peek(): number {
    if (this.offset >= this.bytes.length) {
      throw new WasmParseError(`Unexpected end of binary at offset ${this.offset}`);
    }
    return this.bytes[this.offset];
  }

  u32(): number {
    let result = 0;
    let shift = 0;
    for (;;) {
      const byte = this.u8();
      result |= (byte & 0x7f) << shift;
      shift += 7;
      if ((byte & 0x80) === 0) break;
      if (shift > 35) throw new WasmParseError("LEB128 u32 too long");
    }
    return result >>> 0;
  }

  s32(): number {
    let result = 0;
    let shift = 0;
    let byte = 0;
    for (;;) {
      byte = this.u8();
      result |= (byte & 0x7f) << shift;
      shift += 7;
      if ((byte & 0x80) === 0) break;
      if (shift > 35) throw new WasmParseError("LEB128 s32 too long");
    }
    if (shift < 32 && (byte & 0x40) !== 0) {
      result |= -(1 << shift);
    }
    return result;
  }

  s64(): bigint {
    let result = 0n;
    let shift = 0n;
    let byte = 0;
    for (;;) {
      byte = this.u8();
      result |= BigInt(byte & 0x7f) << shift;
      shift += 7n;
      if ((byte & 0x80) === 0) break;
      if (shift > 70n) throw new WasmParseError("LEB128 s64 too long");
    }
    if (shift < 64n && (byte & 0x40) !== 0) {
      result |= -1n << shift;
    }
    return result;
  }

  f32(): number {
    const value = this.view.getFloat32(this.offset, true);
    this.offset += 4;
    return value;
  }

  f64(): number {
    const value = this.view.getFloat64(this.offset, true);
    this.offset += 8;
    return value;
  }

  readBytes(n: number): Uint8Array {
    if (this.offset + n > this.bytes.length) {
      throw new WasmParseError(`Read of ${n} bytes past end at offset ${this.offset}`);
    }
    const out = this.bytes.subarray(this.offset, this.offset + n);
    this.offset += n;
    return out;
  }

  name(): string {
    const len = this.u32();
    return new TextDecoder("utf-8", { fatal: false }).decode(this.readBytes(len));
  }

  skip(n: number): void {
    this.readBytes(n);
  }
}

const VALTYPE_NAMES: Record<number, string> = {
  0x7f: "i32",
  0x7e: "i64",
  0x7d: "f32",
  0x7c: "f64",
  0x7b: "v128",
  0x70: "funcref",
  0x6f: "externref",
};

const SIMPLE_OPCODES: Record<number, string> = {
  0x00: "unreachable",
  0x01: "nop",
  0x05: "else",
  0x0b: "end",
  0x0f: "return",
  0x1a: "drop",
  0x1b: "select",
  0x45: "i32.eqz",
  0x46: "i32.eq",
  0x47: "i32.ne",
  0x48: "i32.lt_s",
  0x49: "i32.lt_u",
  0x4a: "i32.gt_s",
  0x4b: "i32.gt_u",
  0x4c: "i32.le_s",
  0x4d: "i32.le_u",
  0x4e: "i32.ge_s",
  0x4f: "i32.ge_u",
  0x50: "i64.eqz",
  0x51: "i64.eq",
  0x52: "i64.ne",
  0x53: "i64.lt_s",
  0x54: "i64.lt_u",
  0x55: "i64.gt_s",
  0x56: "i64.gt_u",
  0x57: "i64.le_s",
  0x58: "i64.le_u",
  0x59: "i64.ge_s",
  0x5a: "i64.ge_u",
  0x5b: "f32.eq",
  0x5c: "f32.ne",
  0x5d: "f32.lt",
  0x5e: "f32.gt",
  0x5f: "f32.le",
  0x60: "f32.ge",
  0x61: "f64.eq",
  0x62: "f64.ne",
  0x63: "f64.lt",
  0x64: "f64.gt",
  0x65: "f64.le",
  0x66: "f64.ge",
  0x67: "i32.clz",
  0x68: "i32.ctz",
  0x69: "i32.popcnt",
  0x6a: "i32.add",
  0x6b: "i32.sub",
  0x6c: "i32.mul",
  0x6d: "i32.div_s",
  0x6e: "i32.div_u",
  0x6f: "i32.rem_s",
  0x70: "i32.rem_u",
  0x71: "i32.and",
  0x72: "i32.or",
  0x73: "i32.xor",
  0x74: "i32.shl",
  0x75: "i32.shr_s",
  0x76: "i32.shr_u",
  0x77: "i32.rotl",
  0x78: "i32.rotr",
  0x79: "i64.clz",
  0x7a: "i64.ctz",
  0x7b: "i64.popcnt",
  0x7c: "i64.add",
  0x7d: "i64.sub",
  0x7e: "i64.mul",
  0x7f: "i64.div_s",
  0x80: "i64.div_u",
  0x81: "i64.rem_s",
  0x82: "i64.rem_u",
  0x83: "i64.and",
  0x84: "i64.or",
 0x85: "i64.xor",
  0x86: "i64.shl",
  0x87: "i64.shr_s",
  0x88: "i64.shr_u",
  0x89: "i64.rotl",
  0x8a: "i64.rotr",
  0x8b: "f32.abs",
  0x8c: "f32.neg",
  0x8d: "f32.ceil",
  0x8e: "f32.floor",
  0x8f: "f32.trunc",
  0x90: "f32.nearest",
  0x91: "f32.sqrt",
  0x92: "f32.add",
  0x93: "f32.sub",
  0x94: "f32.mul",
  0x95: "f32.div",
  0x96: "f32.min",
  0x97: "f32.max",
  0x98: "f32.copysign",
  0x99: "f64.abs",
  0x9a: "f64.neg",
  0x9b: "f64.ceil",
  0x9c: "f64.floor",
  0x9d: "f64.trunc",
  0x9e: "f64.nearest",
  0x9f: "f64.sqrt",
  0xa0: "f64.add",
  0xa1: "f64.sub",
  0xa2: "f64.mul",
  0xa3: "f64.div",
  0xa4: "f64.min",
  0xa5: "f64.max",
  0xa6: "f64.copysign",
  0xa7: "i32.wrap_i64",
  0xa8: "i32.trunc_f32_s",
  0xa9: "i32.trunc_f32_u",
  0xaa: "i32.trunc_f64_s",
  0xab: "i32.trunc_f64_u",
  0xac: "i64.extend_i32_s",
  0xad: "i64.extend_i32_u",
  0xae: "i64.trunc_f32_s",
  0xaf: "i64.trunc_f32_u",
  0xb0: "i64.trunc_f64_s",
  0xb1: "i64.trunc_f64_u",
  0xb2: "f32.convert_i32_s",
  0xb3: "f32.convert_i32_u",
  0xb4: "f32.convert_i64_s",
  0xb5: "f32.convert_i64_u",
  0xb6: "f32.demote_f64",
  0xb7: "f64.convert_i32_s",
  0xb8: "f64.convert_i32_u",
  0xb9: "f64.convert_i64_s",
  0xba: "f64.convert_i64_u",
  0xbb: "f64.promote_f32",
  0xbc: "i32.reinterpret_f32",
  0xbd: "i64.reinterpret_f64",
  0xbe: "f32.reinterpret_i32",
  0xbf: "f64.reinterpret_i64",
  0xc0: "i32.extend8_s",
  0xc1: "i32.extend16_s",
  0xc2: "i64.extend8_s",
  0xc3: "i64.extend16_s",
  0xc4: "i64.extend32_s",
  0xd1: "ref.is_null",
};

const FC_OPCODES: Record<number, string> = {
  0: "i32.trunc_sat_f32_s",
  1: "i32.trunc_sat_f32_u",
  2: "i32.trunc_sat_f64_s",
  3: "i32.trunc_sat_f64_u",
  4: "i64.trunc_sat_f32_s",
  5: "i64.trunc_sat_f32_u",
  6: "i64.trunc_sat_f64_s",
  7: "i64.trunc_sat_f64_u",
  8: "memory.init",
  9: "data.drop",
  10: "memory.copy",
  11: "memory.fill",
  12: "table.init",
  13: "elem.drop",
  14: "table.copy",
  15: "table.grow",
  16: "table.size",
  17: "table.fill",
};

const LOAD_OPCODES: Record<number, string> = {
  0x28: "i32.load",
  0x29: "i64.load",
  0x2a: "f32.load",
  0x2b: "f64.load",
  0x2c: "i32.load8_s",
  0x2d: "i32.load8_u",
  0x2e: "i32.load16_s",
  0x2f: "i32.load16_u",
  0x30: "i64.load8_s",
  0x31: "i64.load8_u",
  0x32: "i64.load16_s",
  0x33: "i64.load16_u",
  0x34: "i64.load32_s",
  0x35: "i64.load32_u",
};

const STORE_OPCODES: Record<number, string> = {
  0x36: "i32.store",
  0x37: "i64.store",
  0x38: "f32.store",
  0x39: "f64.store",
  0x3a: "i32.store8",
  0x3b: "i32.store16",
  0x3c: "i64.store8",
  0x3d: "i64.store16",
  0x3e: "i64.store32",
};

const SECTION_NAMES: Record<number, string> = {
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

function decodeBlockType(reader: Reader): string {
  const byte = reader.peek();
  if (byte === 0x40) {
    reader.skip(1);
    return "";
  }
  const named = VALTYPE_NAMES[byte];
  if (named) {
    reader.skip(1);
    return ` (result ${named})`;
  }
  const typeIndex = reader.s32();
  return ` (type ${typeIndex})`;
}

function decodeInstructions(reader: Reader, out: string[], indent: string): void {
  let depth = 0;
  const step = "  ";

  while (reader.remaining > 0) {
    const op = reader.u8();

    if (op === 0x0b) {
      depth = Math.max(depth - 1, 0);
      out.push(`${indent.slice(step.length)}end`);
      if (depth === 0 && reader.remaining === 0) return;
      continue;
    }
    if (op === 0x05) {
      out.push(`${indent.slice(step.length)}else`);
      continue;
    }
    if (op === 0x02 || op === 0x03 || op === 0x04) {
      const kind = op === 0x02 ? "block" : op === 0x03 ? "loop" : "if";
      const blockType = decodeBlockType(reader);
      out.push(`${indent}${kind}${blockType}`);
      depth += 1;
      indent += step;
      continue;
    }
    if (op === 0x0c || op === 0x0d) {
      out.push(`${indent}${op === 0x0c ? "br" : "br_if"} ${reader.u32()}`);
      continue;
    }
    if (op === 0x0e) {
      const count = reader.u32();
      const labels: number[] = [];
      for (let i = 0; i < count; i++) labels.push(reader.u32());
      out.push(`${indent}br_table [${labels.join(" ")}] ${reader.u32()}`);
      continue;
    }
    if (op === 0x0f) {
      out.push(`${indent}return`);
      continue;
    }
    if (op === 0x10 || op === 0x12) {
      out.push(`${indent}${op === 0x10 ? "call" : "return_call"} ${reader.u32()}`);
      continue;
    }
    if (op === 0x11) {
      const typeIndex = reader.u32();
      const tableIndex = reader.u32();
      out.push(`${indent}call_indirect (type ${typeIndex}) (table ${tableIndex})`);
      continue;
    }
    if (op === 0x13) {
      const typeIndex = reader.u32();
      const tableIndex = reader.u32();
      out.push(`${indent}return_call_indirect (type ${typeIndex}) (table ${tableIndex})`);
      continue;
    }
    if (op === 0x1c) {
      const count = reader.u32();
      const types: string[] = [];
      for (let i = 0; i < count; i++) {
        types.push(VALTYPE_NAMES[reader.u8()] ?? "unknown");
      }
      out.push(`${indent}select (${types.join(" ")})`);
      continue;
    }
    if (op >= 0x20 && op <= 0x24) {
      const names = ["local.get", "local.set", "local.tee", "global.get", "global.set"];
      out.push(`${indent}${names[op - 0x20]} ${reader.u32()}`);
      continue;
    }
    if (op === 0x25 || op === 0x26) {
      out.push(`${indent}${op === 0x25 ? "table.get" : "table.set"} ${reader.u32()}`);
      continue;
    }
    if (LOAD_OPCODES[op] || STORE_OPCODES[op]) {
      const name = LOAD_OPCODES[op] ?? STORE_OPCODES[op];
      const align = reader.u32();
      const offset = reader.u32();
      out.push(`${indent}${name} align=${align} offset=${offset}`);
      continue;
    }
    if (op === 0x3f || op === 0x40) {
      reader.skip(1); // reserved 0x00
      out.push(`${indent}${op === 0x3f ? "memory.size" : "memory.grow"}`);
      continue;
    }
    if (op === 0x41) {
      out.push(`${indent}i32.const ${reader.s32()}`);
      continue;
    }
    if (op === 0x42) {
      out.push(`${indent}i64.const ${reader.s64().toString()}`);
      continue;
    }
    if (op === 0x43) {
      out.push(`${indent}f32.const ${reader.f32()}`);
      continue;
    }
    if (op === 0x44) {
      out.push(`${indent}f64.const ${reader.f64()}`);
      continue;
    }
    if (op === 0xd0) {
      const heapType = reader.s32();
      const name = heapType === -16 ? "extern" : heapType === -17 ? "func" : `type ${heapType}`;
      out.push(`${indent}ref.null ${name}`);
      continue;
    }
    if (op === 0xd2) {
      out.push(`${indent}ref.func ${reader.u32()}`);
      continue;
    }
    if (op === 0xfc) {
      const sub = reader.u32();
      const name = FC_OPCODES[sub];
      if (!name) {
        out.push(`${indent};; unknown 0xfc sub-opcode ${sub}`);
        continue;
      }
      if (sub === 8) {
        out.push(`${indent}memory.init ${reader.u32()}`);
      } else if (sub === 9 || sub === 13) {
        out.push(`${indent}${name} ${reader.u32()}`);
      } else if (sub === 10) {
        reader.skip(2);
        out.push(`${indent}memory.copy`);
      } else if (sub === 11) {
        reader.skip(1);
        out.push(`${indent}memory.fill`);
      } else if (sub === 12) {
        out.push(`${indent}table.init ${reader.u32()}`);
      } else if (sub >= 14 && sub <= 17) {
        out.push(`${indent}${name} ${reader.u32()}`);
      } else {
        out.push(`${indent}${name}`);
      }
      continue;
    }

    const simple = SIMPLE_OPCODES[op];
    if (simple) {
      out.push(`${indent}${simple}`);
      if (simple === "end" && depth === 0) return;
      continue;
    }

    out.push(`${indent};; unsupported opcode 0x${op.toString(16).padStart(2, "0")}`);
    return;
  }
}

function decodeLimits(reader: Reader): string {
  const flags = reader.u32();
  const min = reader.u32();
  const max = flags & 0x01 ? ` ${reader.u32()}` : "";
  return `${min}${max}`;
}

function decodeInitExpr(reader: Reader): string {
  const parts: string[] = [];
  for (;;) {
    const op = reader.u8();
    if (op === 0x0b) break;
    if (op === 0x41) parts.push(`i32.const ${reader.s32()}`);
    else if (op === 0x42) parts.push(`i64.const ${reader.s64().toString()}`);
    else if (op === 0x43) parts.push(`f32.const ${reader.f32()}`);
    else if (op === 0x44) parts.push(`f64.const ${reader.f64()}`);
    else if (op === 0x23) parts.push(`global.get ${reader.u32()}`);
    else {
      parts.push(`;; opcode 0x${op.toString(16)}`);
      break;
    }
  }
  return parts.join(" ");
}

function printablePreview(data: Uint8Array, limit = 512): string {
  const slice = data.subarray(0, limit);
  let out = "";
  for (const byte of slice) {
    out += byte >= 0x20 && byte < 0x7f ? String.fromCharCode(byte) : "·";
  }
  return out;
}

function extractStrings(data: Uint8Array, minLength = 4): string[] {
  const out: string[] = [];
  let current = "";
  for (const byte of data) {
    if (byte >= 0x20 && byte < 0x7f) {
      current += String.fromCharCode(byte);
    } else {
      if (current.length >= minLength) out.push(current);
      current = "";
    }
  }
  if (current.length >= minLength) out.push(current);
  return out;
}

/** Parses a raw `.wasm` binary into sections, exports and WAT-like code. */
export function parseWasm(buffer: ArrayBuffer | Uint8Array): ParsedWasm {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  if (bytes.length < 8) {
    throw new WasmParseError("File too small to be a WebAssembly module");
  }
  for (let i = 0; i < 4; i++) {
    if (bytes[i] !== WASM_MAGIC[i]) {
      throw new WasmParseError("Missing WebAssembly magic header (expected \\0asm)");
    }
  }

  const reader = new Reader(bytes);
  reader.skip(4); // \0asm magic
  const versionBytes = reader.readBytes(4);
  const version =
    (versionBytes[0] | (versionBytes[1] << 8) | (versionBytes[2] << 16) | (versionBytes[3] << 24)) >>> 0;
  if (version !== 1) {
    throw new WasmParseError(`Unsupported WebAssembly version ${version}`);
  }

  const result: ParsedWasm = {
    version,
    typeSignatures: [],
    imports: [],
    exports: [],
    functions: [],
    memories: [],
    tables: [],
    globals: [],
    dataStrings: [],
    customSections: [],
    errors: [],
  };

  const funcTypeIndices: number[] = [];
  const bodies: Array<{ size: number; start: number }> = [];

  while (reader.remaining > 0) {
    const sectionId = reader.u8();
    const size = reader.u32();
    const sectionStart = reader.offset;
    const sectionEnd = sectionStart + size;
    if (sectionEnd > bytes.length) {
      throw new WasmParseError(
        `Section ${SECTION_NAMES[sectionId] ?? sectionId} extends past end of file`
      );
    }

    try {
      switch (sectionId) {
        case 0: {
          const name = reader.name();
          const data = bytes.subarray(reader.offset, sectionEnd);
          result.customSections.push({
            name,
            size: data.length,
            preview: printablePreview(data),
            data: new Uint8Array(data),
          });
          break;
        }
        case 1: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            const form = reader.u8();
            if (form !== 0x60) {
              throw new WasmParseError(`Unexpected type form 0x${form.toString(16)}`);
            }
            const paramCount = reader.u32();
            const params: string[] = [];
            for (let p = 0; p < paramCount; p++) {
              params.push(VALTYPE_NAMES[reader.u8()] ?? "unknown");
            }
            const resultCount = reader.u32();
            const results: string[] = [];
            for (let r = 0; r < resultCount; r++) {
              results.push(VALTYPE_NAMES[reader.u8()] ?? "unknown");
            }
            result.typeSignatures.push(
              `(func (param${params.length ? ` ${params.join(" ")}` : ""})${
                results.length ? ` (result ${results.join(" ")})` : ""
              })`
            );
          }
          break;
        }
        case 2: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            const moduleName = reader.name();
            const field = reader.name();
            const kindByte = reader.u8();
            let kind: WasmImportEntry["kind"] = "func";
            let detail = "";
            if (kindByte === 0) {
              kind = "func";
              detail = `(type ${reader.u32()})`;
            } else if (kindByte === 1) {
              kind = "table";
              reader.skip(1);
              detail = `(limits ${decodeLimits(reader)})`;
            } else if (kindByte === 2) {
              kind = "memory";
              detail = `(memory ${decodeLimits(reader)})`;
            } else if (kindByte === 3) {
              kind = "global";
              reader.skip(2);
              detail = "(global)";
            } else {
              throw new WasmParseError(`Unknown import kind ${kindByte}`);
            }
            result.imports.push({ module: moduleName, field, kind, detail });
          }
          break;
        }
        case 3: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) funcTypeIndices.push(reader.u32());
          break;
        }
        case 4: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            reader.skip(1); // elem type
            result.tables.push(`(table ${decodeLimits(reader)})`);
          }
          break;
        }
        case 5: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            result.memories.push(`(memory ${decodeLimits(reader)})`);
          }
          break;
        }
        case 6: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            const typeByte = reader.u8();
            const mutable = reader.u8() === 1;
            const expr = decodeInitExpr(reader);
            result.globals.push(
              `(global (;${i};) ${VALTYPE_NAMES[typeByte] ?? "unknown"}${
                mutable ? " (mut)" : ""
              } ${expr})`
            );
          }
          break;
        }
        case 7: {
          const count = reader.u32();
          const kinds = ["func", "table", "memory", "global"] as const;
          for (let i = 0; i < count; i++) {
            const name = reader.name();
            const kindByte = reader.u8();
            const index = reader.u32();
            result.exports.push({
              name,
              kind: kinds[kindByte] ?? "func",
              index,
            });
          }
          break;
        }
        case 8: {
          result.globals.push(`(start ${reader.u32()})`);
          break;
        }
        case 9: {
          // Element segments — skipped; their contents are reconstructible from the code.
          break;
        }
        case 10: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            const bodySize = reader.u32();
            bodies.push({ size: bodySize, start: reader.offset });
            reader.skip(bodySize);
          }
          break;
        }
        case 11: {
          const count = reader.u32();
          for (let i = 0; i < count; i++) {
            const flags = reader.u32();
            let offset = 0;
            if ((flags & 0x01) === 0) {
              const expr = decodeInitExpr(reader);
              const match = expr.match(/i32\.const (-?\d+)/);
              if (match) offset = Number(match[1]);
              if ((flags & 0x02) !== 0) reader.u32(); // explicit memory index
            }
            const byteLen = reader.u32();
            const data = bytes.subarray(reader.offset, reader.offset + byteLen);
            for (const value of extractStrings(data)) {
              result.dataStrings.push({ segment: i, offset, value });
            }
            reader.offset = Math.min(reader.offset + byteLen, sectionEnd);
          }
          break;
        }
        case 12: {
          reader.u32();
          break;
        }
        default:
          result.errors.push(`Unknown section id ${sectionId} (size ${size})`);
      }
    } catch (err) {
      result.errors.push(
        `${SECTION_NAMES[sectionId] ?? `section ${sectionId}`}: ${
          err instanceof Error ? err.message : String(err)
        }`
      );
    }

    reader.offset = sectionEnd;
  }

  const funcExports = new Map(
    result.exports.filter((e) => e.kind === "func").map((e) => [e.index, e.name])
  );
  const importFuncCount = result.imports.filter((i) => i.kind === "func").length;

  bodies.forEach((body, i) => {
    const index = importFuncCount + i;
    const bodyReader = new Reader(bytes.subarray(body.start, body.start + body.size));
    const locals: string[] = [];
    try {
      const localGroups = bodyReader.u32();
      for (let g = 0; g < localGroups; g++) {
        const occurrences = bodyReader.u32();
        const type = VALTYPE_NAMES[bodyReader.u8()] ?? "unknown";
        for (let o = 0; o < occurrences; o++) locals.push(type);
      }
      const code: string[] = [];
      decodeInstructions(bodyReader, code, "  ");
      result.functions.push({
        index,
        exportName: funcExports.get(index),
        typeIndex: funcTypeIndices[i] ?? 0,
        locals,
        code,
      });
    } catch (err) {
      result.functions.push({
        index,
        exportName: funcExports.get(index),
        typeIndex: funcTypeIndices[i] ?? 0,
        locals,
        code: [`  ;; decode error: ${err instanceof Error ? err.message : String(err)}`],
        truncated: true,
      });
    }
  });

  return result;
}

/** Renders a parsed module as WebAssembly Text (WAT). */
export function toWat(parsed: ParsedWasm): string {
  const lines: string[] = ["(module"];

  parsed.typeSignatures.forEach((signature, i) => {
    lines.push(`  (type (;${i};) ${signature})`);
  });

  for (const entry of parsed.imports) {
    lines.push(`  (import "${escapeWat(entry.module)}" "${escapeWat(entry.field)}" (; ${entry.kind} ${entry.detail} ;))`);
  }

  for (const func of parsed.functions) {
    const label = func.exportName ? ` $${sanitizeId(func.exportName)}` : "";
    lines.push(
      `  (func${label} (;${func.index};) (type ${func.typeIndex})${
        func.locals.length ? ` ;; ${func.locals.length} local(s): ${[...new Set(func.locals)].join(", ")}` : ""
      }`
    );
    if (func.locals.length > 0) {
      const grouped = [...new Set(func.locals)]
        .map((type) => `${func.locals.filter((l) => l === type).length} ${type}`)
        .join(" ");
      lines.push(`    (local ${grouped})`);
    }
    for (const instruction of func.code) {
      lines.push(`  ${instruction}`);
    }
    lines.push("  )");
  }

  for (const memory of parsed.memories) {
    lines.push(`  ${memory}`);
  }
  for (const table of parsed.tables) {
    lines.push(`  ${table}`);
  }
  for (const global of parsed.globals) {
    lines.push(`  ${global}`);
  }

  for (const entry of parsed.exports) {
    lines.push(`  (export "${escapeWat(entry.name)}" (${entry.kind} ${entry.index}))`);
  }

  parsed.dataStrings.forEach((entry, i) => {
    lines.push(
      `  (data (;${i};) (; segment ${entry.segment}, offset ${entry.offset} ;) "${escapeWat(entry.value)}")`
    );
  });

  for (const custom of parsed.customSections) {
    lines.push(
      `  ;; custom section "${escapeWat(custom.name)}" (${custom.size} bytes)`
    );
  }

  lines.push(")");
  return lines.join("\n");
}

function escapeWat(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function sanitizeId(value: string): string {
  const cleaned = value.replace(/[^A-Za-z0-9_.]/g, "_");
  return /^[A-Za-z0-9_.]/.test(cleaned) ? cleaned : `_${cleaned}`;
}

/** Extracts the `contractspecv0` custom section payload (XDR-encoded SCSpecEntry stream). */
export function extractContractSpecSection(parsed: ParsedWasm): Uint8Array | null {
  const section = parsed.customSections.find((c) => c.name === "contractspecv0");
  return section ? section.data : null;
}

/** Extracts the `contractenvmetav0` custom section payload (XDR-encoded ScEnvMetaEntry stream). */
export function extractEnvMetaSection(parsed: ParsedWasm): Uint8Array | null {
  const section = parsed.customSections.find((c) => c.name === "contractenvmetav0");
  return section ? section.data : null;
}

/** Extracts the `contractmetav0` custom section payload (XDR-encoded ScMetaV0 stream). */
export function extractContractMetaSection(parsed: ParsedWasm): Uint8Array | null {
  const section = parsed.customSections.find((c) => c.name === "contractmetav0");
  return section ? section.data : null;
}
