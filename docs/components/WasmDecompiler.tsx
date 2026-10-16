"use client";

/**
 * WasmDecompiler
 *
 * Client-side Soroban/WebAssembly contract decompiler:
 *  - Parses a `.wasm` binary and disassembles it to WAT-style text
 *  - Extracts custom Soroban spec entries (contractspecv0), env/meta sections,
 *    export functions, and data-segment strings (storage-key candidates)
 *  - Interactive code search with line-number deep links
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  extractContractSpecSection,
  extractContractMetaSection,
  extractEnvMetaSection,
  parseWasm,
  toWat,
  type ParsedWasm,
  WasmParseError,
} from "@/lib/wasm";

type Tab = "wat" | "interface" | "storage";

type SpecEntryInfo = {
  kind: string;
  name: string;
  lib: string;
  doc: string;
  lines: string[];
};

type SpecState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; entries: SpecEntryInfo[]; jsonSchema: unknown }
  | { status: "error"; message: string };

const SIMPLE_TYPE_NAMES: Record<string, string> = {
  scSpecTypeVal: "val",
  scSpecTypeBool: "bool",
  scSpecTypeVoid: "void",
  scSpecTypeError: "Error",
  scSpecTypeU32: "u32",
  scSpecTypeI32: "i32",
  scSpecTypeU64: "u64",
  scSpecTypeI64: "i64",
  scSpecTypeTimepoint: "timepoint",
  scSpecTypeDuration: "duration",
  scSpecTypeU128: "u128",
  scSpecTypeI128: "i128",
  scSpecTypeU256: "u256",
  scSpecTypeI256: "i256",
  scSpecTypeBytes: "bytes",
  scSpecTypeString: "string",
  scSpecTypeSymbol: "symbol",
  scSpecTypeAddress: "address",
  scSpecTypeMuxedAddress: "muxed_address",
};

type ScTypeLike = {
  type?: string;
  value?: unknown;
  elementType?: ScTypeLike;
  valueType?: ScTypeLike;
  keyType?: ScTypeLike;
  valueTypes?: ScTypeLike[];
  okType?: ScTypeLike;
  errorType?: ScTypeLike;
  n?: number;
  name?: unknown;
};

function formatSpecType(def: unknown): string {
  const type = def as ScTypeLike | null;
  if (!type || typeof type.type !== "string") return "?";
  const simple = SIMPLE_TYPE_NAMES[type.type];
  if (simple) return simple;
  switch (type.type) {
    case "scSpecTypeVec":
      return `vec<${formatSpecType(type.elementType ?? type.value)}>`;
    case "scSpecTypeOption":
      return `option<${formatSpecType(type.valueType ?? type.value)}>`;
    case "scSpecTypeResult": {
      const wrapper = (type.okType ? type : (type.value as ScTypeLike)) as ScTypeLike;
      return `result<${formatSpecType(wrapper.okType)}, ${formatSpecType(wrapper.errorType)}>`;
    }
    case "scSpecTypeMap": {
      const wrapper = (type.keyType ? type : (type.value as ScTypeLike)) as ScTypeLike;
      return `map<${formatSpecType(wrapper.keyType)}, ${formatSpecType(wrapper.valueType)}>`;
    }
    case "scSpecTypeTuple": {
      const wrapper = (type.valueTypes ? type : (type.value as ScTypeLike)) as ScTypeLike;
      return `(${(wrapper.valueTypes ?? []).map(formatSpecType).join(", ")})`;
    }
    case "scSpecTypeBytesN": {
      const wrapper = (type.n !== undefined ? type : (type.value as ScTypeLike)) as ScTypeLike;
      return `bytes<${wrapper.n ?? "?"}>`;
    }
    case "scSpecTypeUdt": {
      const wrapper = (type.name !== undefined ? type : (type.value as ScTypeLike)) as ScTypeLike;
      return String(wrapper.name ?? "udt");
    }
    default:
      return type.type.replace(/^scSpecType/, "").toLowerCase();
  }
}

type SpecEntryLike = {
  type?: string;
  value?: {
    name?: unknown;
    lib?: unknown;
    doc?: unknown;
    inputs?: Array<{ name?: unknown; doc?: unknown; type?: unknown }>;
    outputs?: unknown[];
    fields?: Array<{ name?: unknown; doc?: unknown; type?: unknown }>;
    cases?: Array<{
      type?: string;
      value?: unknown;
      name?: unknown;
      doc?: unknown;
    }>;
    prefixTopics?: unknown[];
    params?: Array<{ name?: unknown; doc?: unknown; type?: unknown; location?: { name?: string } }>;
    dataFormat?: { name?: string };
  };
};

function describeEntry(entry: SpecEntryLike): SpecEntryInfo {
  const value = entry.value ?? {};
  const name = String(value.name ?? "?");
  const lib = value.lib !== undefined ? String(value.lib) : "";
  const doc = value.doc !== undefined ? String(value.doc) : "";
  const lines: string[] = [];

  switch (entry.type) {
    case "scSpecEntryFunctionV0": {
      const inputs = (value.inputs ?? [])
        .map((input) => `${String(input.name ?? "?")}: ${formatSpecType(input.type)}`)
        .join(", ");
      const outputs = (value.outputs ?? []).map(formatSpecType);
      lines.push(`fn ${name}(${inputs})${outputs.length ? ` -> ${outputs.join(" | ")}` : ""}`);
      if (doc) lines.push(`  /// ${doc}`);
      (value.inputs ?? []).forEach((input) => {
        if (input.doc && String(input.doc)) lines.push(`  /// @param ${String(input.name)} ${String(input.doc)}`);
      });
      break;
    }
    case "scSpecEntryUdtStructV0": {
      lines.push(`struct ${name} {`);
      (value.fields ?? []).forEach((field) => {
        lines.push(`  ${String(field.name)}: ${formatSpecType(field.type)},`);
      });
      lines.push("}");
      if (doc) lines.push(`/// ${doc}`);
      break;
    }
    case "scSpecEntryUdtUnionV0": {
      lines.push(`union ${name} =`);
      (value.cases ?? []).forEach((unionCase) => {
        const caseValue =
          unionCase.value && typeof unionCase.value === "object"
            ? (unionCase.value as { name?: unknown; type?: unknown })
            : null;
        const caseName = String(caseValue?.name ?? unionCase.name ?? "?");
        if (unionCase.type === "scSpecUdtUnionCaseTupleV0" && Array.isArray(caseValue?.type)) {
          const types = (caseValue.type as unknown[]).map(formatSpecType);
          lines.push(`  | ${caseName}(${types.join(", ")})`);
        } else {
          lines.push(`  | ${caseName}`);
        }
      });
      if (doc) lines.push(`/// ${doc}`);
      break;
    }
    case "scSpecEntryUdtEnumV0":
    case "scSpecEntryUdtErrorEnumV0": {
      const keyword = entry.type === "scSpecEntryUdtEnumV0" ? "enum" : "error_enum";
      lines.push(`${keyword} ${name} {`);
      (value.cases ?? []).forEach((enumCase) => {
        lines.push(`  ${String(enumCase.name)},`);
      });
      lines.push("}");
      if (doc) lines.push(`/// ${doc}`);
      break;
    }
    case "scSpecEntryEventV0": {
      const topics = (value.prefixTopics ?? []).map(formatSpecType);
      const params = (value.params ?? [])
        .map(
          (param) =>
            `${param.location?.name === "scSpecEventParamLocationTopicList" ? "#topic " : ""}${String(
              param.name
            )}: ${formatSpecType(param.type)}`
        )
        .join(", ");
      lines.push(`event ${name}[${topics.join(", ")}](${params}) : ${value.dataFormat?.name ?? "?"}`);
      if (doc) lines.push(`  /// ${doc}`);
      break;
    }
    default:
      lines.push(`${entry.type ?? "unknown"} ${name}`);
  }

  return { kind: entry.type ?? "unknown", name, lib, doc, lines };
}

function buildEmptyInterface(): SpecState {
  return { status: "idle" };
}

export function WasmDecompiler() {
  const [parsed, setParsed] = useState<ParsedWasm | null>(null);
  const [wat, setWat] = useState<string>("");
  const [fileName, setFileName] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("wat");
  const [query, setQuery] = useState("");
  const [activeLine, setActiveLine] = useState<number | null>(null);
  const [spec, setSpec] = useState<SpecState>(buildEmptyInterface);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const lineRefs = useRef<Map<number, HTMLDivElement | null>>(new Map());

  const watLines = useMemo(() => (wat ? wat.split("\n") : []), [wat]);

  const matchIndices = useMemo(() => {
    if (!query.trim()) return [] as number[];
    const needle = query.trim().toLowerCase();
    const hits: number[] = [];
    watLines.forEach((line, index) => {
      if (line.toLowerCase().includes(needle)) hits.push(index + 1);
    });
    return hits;
  }, [query, watLines]);

  const loadSpec = useCallback(async (result: ParsedWasm) => {
    const specSection = extractContractSpecSection(result);
    if (!specSection || specSection.length === 0) {
      setSpec({ status: "error", message: "No contractspecv0 section found in this module." });
      return;
    }
    setSpec({ status: "loading" });
    try {
      const { Spec } = await import("@stellar/stellar-sdk/contract");
      const contractSpec = new Spec(specSection);
      const entries = contractSpec.entries.map(describeEntry);
      setSpec({ status: "ready", entries, jsonSchema: contractSpec.jsonSchema() });
    } catch (err) {
      setSpec({
        status: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  const handleFile = useCallback(
    async (file: File) => {
      setError(null);
      setSpec(buildEmptyInterface());
      setActiveLine(null);
      setQuery("");
      try {
        const buffer = new Uint8Array(await file.arrayBuffer());
        const result = parseWasm(buffer);
        setParsed(result);
        setWat(toWat(result));
        setFileName(file.name);
        setTab("wat");
        void loadSpec(result);
      } catch (err) {
        setParsed(null);
        setWat("");
        setFileName(file.name);
        setError(
          err instanceof WasmParseError || err instanceof Error
            ? err.message
            : "Failed to parse WebAssembly module"
        );
      }
    },
    [loadSpec]
  );

  const jumpToLine = useCallback(
    (line: number) => {
      setActiveLine(line);
      const el = lineRefs.current.get(line);
      if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
      if (typeof window !== "undefined") {
        window.history.replaceState(null, "", `#wat-line-${line}`);
      }
    },
    []
  );

  const goToMatch = useCallback(
    (direction: 1 | -1) => {
      if (matchIndices.length === 0) return;
      const current = activeLine ?? 0;
      const position = matchIndices.findIndex((line) => (direction === 1 ? line > current : line >= current));
      let next: number;
      if (direction === 1) {
        next = position === -1 ? matchIndices[0] : matchIndices[position];
      } else {
        const before = matchIndices.filter((line) => line < current);
        next = before.length > 0 ? before[before.length - 1] : matchIndices[matchIndices.length - 1];
      }
      jumpToLine(next);
    },
    [activeLine, jumpToLine, matchIndices]
  );

  useEffect(() => {
    if (typeof window === "undefined") return;
    const hash = window.location.hash.match(/^#wat-line-(\d+)$/);
    if (hash && watLines.length > 0) {
      const line = Number(hash[1]);
      if (line >= 1 && line <= watLines.length) jumpToLine(line);
    }
  }, [watLines, jumpToLine]);

  const storageStrings = useMemo(() => {
    if (!parsed) return [] as string[];
    const seen = new Set<string>();
    const out: string[] = [];
    for (const entry of parsed.dataStrings) {
      if (/^[A-Za-z0-9_.:\-/# ]{4,}$/.test(entry.value) && !seen.has(entry.value)) {
        seen.add(entry.value);
        out.push(entry.value);
      }
    }
    return out;
  }, [parsed]);

  const sorobanSections = useMemo(
    () =>
      parsed
        ? parsed.customSections.filter((section) => section.name.startsWith("contract"))
        : [],
    [parsed]
  );

  const copyLink = useCallback((line: number) => {
    const url = `${typeof window !== "undefined" ? window.location.href.split("#")[0] : ""}#wat-line-${line}`;
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      void navigator.clipboard.writeText(url).catch(() => undefined);
    }
    jumpToLine(line);
  }, [jumpToLine]);

  return (
    <div className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4" data-testid="wasm-decompiler">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold text-white/90">WASM Decompiler &amp; Soroban Interface</h3>
          <p className="text-xs text-white/50">
            {fileName
              ? `${fileName} — ${parsed ? `${parsed.functions.length} functions, ${parsed.exports.length} exports` : ""}`
              : "Load a .wasm contract to disassemble it"}
          </p>
        </div>
        <button
          type="button"
          onClick={() => fileInputRef.current?.click()}
          className="rounded-lg bg-emerald-500/90 px-3 py-1.5 text-xs font-semibold text-black transition hover:bg-emerald-400"
        >
          Open .wasm file
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept=".wasm,application/wasm"
          className="hidden"
          aria-label="WebAssembly contract file"
          onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void handleFile(file);
            event.target.value = "";
          }}
        />
      </div>

      {error && (
        <div className="mb-3 rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300" role="alert">
          {error}
        </div>
      )}

      <div className="mb-3 flex flex-wrap gap-1" role="tablist" aria-label="Decompiler views">
        {(
          [
            ["wat", "Disassembly"],
            ["interface", "Interface"],
            ["storage", "Exports & Storage"],
          ] as Array<[Tab, string]>
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={tab === id}
            onClick={() => setTab(id)}
            className={`rounded-md px-3 py-1 text-xs font-medium transition ${
              tab === id
                ? "bg-white/15 text-white"
                : "bg-white/5 text-white/50 hover:bg-white/10 hover:text-white/80"
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {!parsed && !error && (
        <div className="rounded-lg border border-dashed border-white/15 px-4 py-8 text-center text-xs text-white/40">
          Drop or open a compiled contract (<code className="text-white/60">.wasm</code>) — parsing runs
          entirely in your browser; the module is never executed.
        </div>
      )}

      {tab === "wat" && parsed && (
        <div>
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search instructions, exports, callees…"
              aria-label="Search disassembly"
              className="w-64 rounded-lg border border-white/10 bg-black/30 px-3 py-1.5 text-xs text-white/90 outline-none placeholder:text-white/30 focus:border-emerald-400/60"
            />
            <span className="text-[11px] text-white/40" data-testid="wat-search-count">
              {query.trim()
                ? `${matchIndices.length} match${matchIndices.length === 1 ? "" : "es"}`
                : `${watLines.length} lines`}
            </span>
            <button
              type="button"
              onClick={() => goToMatch(-1)}
              disabled={matchIndices.length === 0}
              className="rounded-md bg-white/10 px-2 py-1 text-[11px] text-white/70 transition hover:bg-white/20 disabled:opacity-30"
              aria-label="Previous match"
            >
              ↑ Prev
            </button>
            <button
              type="button"
              onClick={() => goToMatch(1)}
              disabled={matchIndices.length === 0}
              className="rounded-md bg-white/10 px-2 py-1 text-[11px] text-white/70 transition hover:bg-white/20 disabled:opacity-30"
              aria-label="Next match"
            >
              ↓ Next
            </button>
          </div>
          <div className="max-h-[28rem] overflow-auto rounded-lg border border-white/10 bg-black/40 py-1 font-mono text-[11px] leading-5">
            {watLines.map((line, index) => {
              const lineNumber = index + 1;
              const isActive = activeLine === lineNumber;
              const isMatch = matchIndices.includes(lineNumber);
              return (
                <div
                  key={lineNumber}
                  id={`wat-line-${lineNumber}`}
                  ref={(node) => {
                    lineRefs.current.set(lineNumber, node);
                  }}
                  className={`flex ${isActive ? "bg-emerald-500/15" : isMatch ? "bg-amber-400/10" : ""}`}
                  style={{ contentVisibility: "auto", containIntrinsicSize: "0 20px" }}
                >
                  <button
                    type="button"
                    onClick={() => copyLink(lineNumber)}
                    title={`Copy link to line ${lineNumber}`}
                    aria-label={`Line ${lineNumber}`}
                    className={`w-12 shrink-0 select-none pr-2 text-right tabular-nums transition ${
                      isActive || isMatch
                        ? "text-emerald-300/90"
                        : "text-white/25 hover:text-emerald-300/80"
                    }`}
                  >
                    {lineNumber}
                  </button>
                  <pre className="whitespace-pre-wrap break-all pr-3 text-white/80">{line || " "}</pre>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {tab === "interface" && parsed && (
        <div className="rounded-lg border border-white/10 bg-black/40 p-3">
          {spec.status === "idle" && (
            <p className="text-xs text-white/40">No contractspecv0 section detected.</p>
          )}
          {spec.status === "loading" && <p className="text-xs text-white/50">Parsing Soroban interface…</p>}
          {spec.status === "error" && (
            <p className="text-xs text-amber-300/80">{spec.message}</p>
          )}
          {spec.status === "ready" && (
            <div className="space-y-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-[11px] uppercase tracking-wide text-white/40">
                  {spec.entries.length} spec entries
                </span>
                <button
                  type="button"
                  onClick={() => {
                    if (typeof navigator !== "undefined" && navigator.clipboard) {
                      void navigator.clipboard
                        .writeText(JSON.stringify(spec.jsonSchema, null, 2))
                        .catch(() => undefined);
                    }
                  }}
                  className="rounded-md bg-white/10 px-2 py-1 text-[11px] text-white/70 transition hover:bg-white/20"
                >
                  Copy JSON schema
                </button>
              </div>
              {spec.entries.map((entry, index) => (
                <div key={`${entry.kind}-${entry.name}-${index}`} className="border-b border-white/5 pb-2 last:border-0">
                  <div className="mb-1 flex flex-wrap items-center gap-2">
                    <span className="rounded bg-sky-500/20 px-1.5 py-0.5 text-[10px] font-medium text-sky-300">
                      {entry.kind.replace("scSpecEntry", "").replace("V0", "")}
                    </span>
                    <span className="text-xs font-semibold text-white/90">{entry.name}</span>
                    {entry.lib && <span className="text-[10px] text-white/35">lib: {entry.lib}</span>}
                  </div>
                  <pre className="whitespace-pre-wrap font-mono text-[11px] leading-5 text-emerald-200/80">
                    {entry.lines.join("\n")}
                  </pre>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {tab === "storage" && parsed && (
        <div className="grid gap-3 md:grid-cols-2">
          <div className="rounded-lg border border-white/10 bg-black/40 p-3">
            <h4 className="mb-2 text-[11px] uppercase tracking-wide text-white/40">
              Exported functions ({parsed.exports.filter((entry) => entry.kind === "func").length})
            </h4>
            <ul className="max-h-56 space-y-0.5 overflow-auto font-mono text-[11px]">
              {parsed.exports
                .filter((entry) => entry.kind === "func")
                .map((entry) => (
                  <li key={entry.name} className="flex justify-between gap-2 text-white/70">
                    <span className="text-emerald-200/80">{entry.name}</span>
                    <span className="text-white/30">func[{entry.index}]</span>
                  </li>
                ))}
            </ul>
            {parsed.exports.some((entry) => entry.kind !== "func") && (
              <div className="mt-2 border-t border-white/10 pt-2">
                <h4 className="mb-1 text-[11px] uppercase tracking-wide text-white/40">Other exports</h4>
                <ul className="space-y-0.5 font-mono text-[11px] text-white/60">
                  {parsed.exports
                    .filter((entry) => entry.kind !== "func")
                    .map((entry) => (
                      <li key={`${entry.kind}-${entry.name}`}>
                        {entry.name} → {entry.kind}[{entry.index}]
                      </li>
                    ))}
                </ul>
              </div>
            )}
          </div>
          <div className="rounded-lg border border-white/10 bg-black/40 p-3">
            <h4 className="mb-2 text-[11px] uppercase tracking-wide text-white/40">
              Storage-key candidates (data segments)
            </h4>
            {storageStrings.length === 0 ? (
              <p className="text-xs text-white/40">No string literals found in data segments.</p>
            ) : (
              <ul className="max-h-40 space-y-0.5 overflow-auto font-mono text-[11px] text-white/70">
                {storageStrings.map((value) => (
                  <li key={value} className="truncate">
                    {value}
                  </li>
                ))}
              </ul>
            )}
            <h4 className="mb-2 mt-3 text-[11px] uppercase tracking-wide text-white/40">
              Soroban custom sections
            </h4>
            <ul className="space-y-0.5 font-mono text-[11px] text-white/70">
              {sorobanSections.map((section) => (
                <li key={section.name} className="truncate" title={section.preview}>
                  {section.name} — {section.size} bytes
                </li>
              ))}
              {parsed.customSections
                .filter((section) => !section.name.startsWith("contract"))
                .map((section) => (
                  <li key={section.name} className="truncate text-white/40" title={section.preview}>
                    {section.name} — {section.size} bytes
                  </li>
                ))}
            </ul>
            {(extractEnvMetaSection(parsed) || extractContractMetaSection(parsed)) && (
              <p className="mt-2 text-[11px] text-white/40">
                Env/meta metadata present:{" "}
                {[
                  extractEnvMetaSection(parsed) ? "contractenvmetav0" : null,
                  extractContractMetaSection(parsed) ? "contractmetav0" : null,
                ]
                  .filter(Boolean)
                  .join(", ")}
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default WasmDecompiler;
