"use client";

import { useDeferredValue, useEffect, useId, useMemo, useRef, useState, type ChangeEvent, type ReactNode } from "react";
import {
  disassembleWasm,
  inspectWasm,
  type SpecEntrySummary,
  type WasmInspection,
} from "@/lib/wasm-inspector";
import { SOROBAN_MAINNET_LIMITS } from "@/lib/soroban-resources";

/** Upper bound on accepted files; unoptimized debug builds stay well below it. */
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const ROW_HEIGHT_PX = 20;
const VIEWPORT_HEIGHT_PX = 480;
const OVERSCAN_ROWS = 12;

type View = "interface" | "wat";
type LoadState =
  | { status: "idle" }
  | { status: "loading"; fileName: string }
  | { status: "error"; message: string }
  | { status: "ready"; fileName: string; inspection: WasmInspection; lines: string[] };

const TOKEN_PATTERN =
  /(;;.*$)|("(?:[^"\\]|\\.)*")|(\$[^\s()]+)|\b(module|func|param|result|local|type|import|export|memory|data|global|table|elem|mut|start|offset)\b|\b((?:[a-z][a-z0-9]*\.[a-z0-9_.]+)|call_indirect|call|br_table|br_if|br|block|loop|if|then|else|end|return|drop|select|unreachable|nop)\b|(-?\b(?:0x[0-9a-fA-F]+|\d+(?:\.\d+)?)\b)/g;
const TOKEN_CLASSES = ["text-base-300", "text-gold-300", "text-sky-300", "text-brand-300", "text-violet-300", "text-orange-300"];

interface Segment {
  start: number;
  end: number;
  className: string;
}

/** Splits a WAT line into highlighted token segments in one regex pass. */
function tokenize(line: string): Segment[] {
  const segments: Segment[] = [];
  let cursor = 0;
  for (const match of line.matchAll(TOKEN_PATTERN)) {
    const start = match.index ?? 0;
    if (start > cursor) segments.push({ start: cursor, end: start, className: "text-base-100" });
    const group = match.slice(1).findIndex((value) => value !== undefined);
    segments.push({ start, end: start + match[0].length, className: TOKEN_CLASSES[group] ?? "text-base-100" });
    cursor = start + match[0].length;
  }
  if (cursor < line.length) segments.push({ start: cursor, end: line.length, className: "text-base-100" });
  return segments;
}

/** Non-overlapping, case-insensitive occurrences of `query` in `lowerLine`. */
function findRanges(lowerLine: string, query: string): [number, number][] {
  const ranges: [number, number][] = [];
  if (!query) return ranges;
  for (let index = lowerLine.indexOf(query); index !== -1; index = lowerLine.indexOf(query, index + query.length)) {
    ranges.push([index, index + query.length]);
  }
  return ranges;
}

/**
 * Renders syntax tokens with search matches overlaid. Matches may span token
 * boundaries, so tokens are split at match edges: O(tokens + matches).
 */
function renderLine(line: string, lowerLine: string, query: string): ReactNode[] {
  const ranges = findRanges(lowerLine, query);
  const nodes: ReactNode[] = [];
  let rangeIndex = 0;
  for (const segment of tokenize(line)) {
    let position = segment.start;
    while (position < segment.end) {
      while (rangeIndex < ranges.length && ranges[rangeIndex][1] <= position) rangeIndex += 1;
      const range = ranges[rangeIndex];
      const inMatch = range !== undefined && range[0] <= position;
      const end = Math.min(segment.end, inMatch ? range[1] : (range?.[0] ?? segment.end));
      const text = line.slice(position, end);
      nodes.push(
        inMatch ? (
          <mark key={position} className="rounded-sm bg-gold-400/30 text-base-50">
            {text}
          </mark>
        ) : (
          <span key={position} className={segment.className}>
            {text}
          </span>
        )
      );
      position = end;
    }
  }
  return nodes;
}

function readFileBytes(file: File): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(reader.error ?? new Error("Could not read the file."));
    reader.readAsArrayBuffer(file);
  });
}

function formatBytes(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KiB`;
}

function WatViewer({ lines }: { lines: string[] }) {
  const searchId = useId();
  const viewportRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [query, setQuery] = useState("");
  const [activeMatch, setActiveMatch] = useState(0);
  const deferredQuery = useDeferredValue(query.trim().toLowerCase());

  // Lower-cased once per module so each search is a plain O(total chars) scan.
  const lowerLines = useMemo(() => lines.map((line) => line.toLowerCase()), [lines]);
  const matchLines = useMemo(() => {
    if (!deferredQuery) return [];
    const matches: number[] = [];
    lowerLines.forEach((line, index) => {
      if (line.includes(deferredQuery)) matches.push(index);
    });
    return matches;
  }, [lowerLines, deferredQuery]);

  useEffect(() => setActiveMatch(0), [deferredQuery]);

  const activeLine = matchLines[activeMatch];
  useEffect(() => {
    const viewport = viewportRef.current;
    if (viewport && activeLine !== undefined) {
      viewport.scrollTop = Math.max(0, activeLine * ROW_HEIGHT_PX - VIEWPORT_HEIGHT_PX / 2);
    }
  }, [activeLine]);

  const step = (delta: number) => {
    if (matchLines.length === 0) return;
    setActiveMatch((current) => (current + delta + matchLines.length) % matchLines.length);
  };

  // Only rows inside the viewport (plus overscan) are mounted: O(visible rows).
  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT_PX) - OVERSCAN_ROWS);
  const last = Math.min(lines.length, Math.ceil((scrollTop + VIEWPORT_HEIGHT_PX) / ROW_HEIGHT_PX) + OVERSCAN_ROWS);
  const gutterWidth = `${String(lines.length).length + 1}ch`;

  return (
    <div className="space-y-3">
      <form
        role="search"
        className="flex flex-wrap items-end gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          step(1);
        }}
      >
        <div className="min-w-0 flex-1">
          <label htmlFor={searchId} className="mb-1 block text-xs font-semibold text-base-300">
            Search instructions
          </label>
          <input
            id={searchId}
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="e.g. call $func12, i64.const, (export"
            className="w-full rounded-md border border-white/10 bg-base-900 px-2.5 py-1.5 font-mono text-sm text-base-100 placeholder-base-300/60 focus:border-brand-500/60 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
          />
        </div>
        <button
          type="button"
          onClick={() => step(-1)}
          disabled={matchLines.length === 0}
          className="rounded-md border border-white/10 px-3 py-1.5 text-sm text-base-100 hover:border-brand-500/50 disabled:cursor-not-allowed disabled:opacity-40"
        >
          Previous
        </button>
        <button
          type="submit"
          disabled={matchLines.length === 0}
          className="rounded-md border border-white/10 px-3 py-1.5 text-sm text-base-100 hover:border-brand-500/50 disabled:cursor-not-allowed disabled:opacity-40"
        >
          Next
        </button>
      </form>
      <p className="min-h-4 text-xs text-base-300" aria-live="polite">
        {deferredQuery
          ? matchLines.length > 0
            ? `Match ${activeMatch + 1} of ${matchLines.length} lines (line ${activeLine + 1})`
            : "No matching lines"
          : `${lines.length.toLocaleString()} lines`}
      </p>
      <div
        ref={viewportRef}
        role="region"
        aria-label="Disassembled WebAssembly text"
        // Keyboard users must be able to focus the scrollable region to scroll it (WCAG 2.1.1).
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
        tabIndex={0}
        onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
        className="overflow-auto rounded-lg border border-white/10 bg-base-950 font-mono text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
        style={{ height: VIEWPORT_HEIGHT_PX }}
      >
        <div className="relative" style={{ height: lines.length * ROW_HEIGHT_PX }}>
          {lines.slice(first, last).map((line, offset) => {
            const index = first + offset;
            return (
              <div
                key={index}
                className={`absolute left-0 right-0 flex whitespace-pre px-2 ${index === activeLine ? "bg-gold-400/10" : ""}`}
                style={{ top: index * ROW_HEIGHT_PX, height: ROW_HEIGHT_PX, lineHeight: `${ROW_HEIGHT_PX}px` }}
              >
                <span className="shrink-0 select-none pr-3 text-right text-base-300/50" style={{ width: gutterWidth }}>
                  {index + 1}
                </span>
                <span>{renderLine(line, lowerLines[index], deferredQuery)}</span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

const SPEC_KIND_LABELS: Record<SpecEntrySummary["kind"], string> = {
  function: "fn",
  struct: "struct",
  union: "union",
  enum: "enum",
  error: "error",
  event: "event",
};

function SpecList({ entries }: { entries: SpecEntrySummary[] }) {
  return (
    <ul className="space-y-2">
      {entries.map((entry) => (
        <li key={`${entry.kind}:${entry.name}`} className="rounded-lg border border-white/10 bg-base-900/60 px-3 py-2">
          <div className="flex items-center gap-2">
            <span className="rounded border border-white/10 px-1.5 py-0.5 font-mono text-[0.7rem] uppercase text-base-300">
              {SPEC_KIND_LABELS[entry.kind]}
            </span>
            <code className="font-mono text-sm text-base-50">{entry.name}</code>
          </div>
          {entry.doc && <p className="mt-1 whitespace-pre-line text-xs text-base-300">{entry.doc}</p>}
          <ul className="mt-1.5 space-y-0.5">
            {entry.details.map((detail) => (
              <li key={detail} className="break-all font-mono text-xs text-base-100">
                {detail}
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ul>
  );
}

function InterfaceView({ inspection }: { inspection: WasmInspection }) {
  const functions = inspection.spec.filter((entry) => entry.kind === "function");
  const types = inspection.spec.filter((entry) => entry.kind !== "function");
  const functionExports = inspection.exports.filter((entry) => entry.kind === "function");

  return (
    <div className="space-y-6">
      <section aria-label="Contract functions">
        <h4 className="mb-2 text-sm font-semibold text-base-50">Contract functions ({functions.length})</h4>
        {functions.length > 0 ? <SpecList entries={functions} /> : <p className="text-xs text-base-300">No spec functions.</p>}
      </section>

      {types.length > 0 && (
        <section aria-label="Custom types, errors and events">
          <h4 className="mb-2 text-sm font-semibold text-base-50">Types, errors &amp; events ({types.length})</h4>
          <SpecList entries={types} />
        </section>
      )}

      <section aria-label="WASM exports">
        <h4 className="mb-2 text-sm font-semibold text-base-50">WASM exports ({functionExports.length} functions)</h4>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs">
            <thead className="text-base-300">
              <tr>
                <th scope="col" className="py-1.5 pr-4 font-semibold">Name</th>
                <th scope="col" className="py-1.5 pr-4 font-semibold">Kind</th>
                <th scope="col" className="py-1.5 font-semibold">WASM signature</th>
              </tr>
            </thead>
            <tbody>
              {inspection.exports.map((entry) => (
                <tr key={`${entry.kind}:${entry.name}`} className="border-t border-white/5">
                  <td className="py-1.5 pr-4 font-mono text-base-50">{entry.name}</td>
                  <td className="py-1.5 pr-4 text-base-300">{entry.kind}</td>
                  <td className="py-1.5 font-mono text-base-100">{entry.signature ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section aria-label="Imported host functions">
        <h4 className="mb-2 text-sm font-semibold text-base-50">Imported host functions ({inspection.imports.length})</h4>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs">
            <thead className="text-base-300">
              <tr>
                <th scope="col" className="py-1.5 pr-4 font-semibold">Import</th>
                <th scope="col" className="py-1.5 pr-4 font-semibold">Host function</th>
                <th scope="col" className="py-1.5 font-semibold">Host signature</th>
              </tr>
            </thead>
            <tbody>
              {inspection.imports.map((entry, index) => (
                <tr key={index} className="border-t border-white/5">
                  <td className="py-1.5 pr-4 font-mono text-base-100">
                    {entry.module}.{entry.field}
                  </td>
                  <td className="py-1.5 pr-4 font-mono text-base-50">
                    {entry.hostFunction ? `${entry.hostFunction.module}::${entry.hostFunction.name}` : "Unknown import"}
                  </td>
                  <td className="py-1.5 font-mono text-base-100">{entry.hostFunction?.signature ?? entry.signature ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section aria-label="Custom sections">
        <h4 className="mb-2 text-sm font-semibold text-base-50">Custom sections</h4>
        <p className="font-mono text-xs text-base-100">{inspection.customSections.join(", ") || "None"}</p>
      </section>
    </div>
  );
}

/**
 * Client-side Soroban contract inspector: decodes a compiled `.wasm` file's
 * contract spec, exports and host-function imports, and disassembles it to
 * WebAssembly Text Format with search and instruction highlighting. The file
 * is processed in the browser and never uploaded.
 */
export function WasmDisassembler() {
  const titleId = useId();
  const inputId = useId();
  const [state, setState] = useState<LoadState>({ status: "idle" });
  const [view, setView] = useState<View>("interface");
  const requestRef = useRef(0);

  const handleFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;
    const request = ++requestRef.current;
    if (file.size > MAX_FILE_BYTES) {
      setState({ status: "error", message: `${file.name} is ${formatBytes(file.size)}; the limit is ${formatBytes(MAX_FILE_BYTES)}.` });
      return;
    }
    setState({ status: "loading", fileName: file.name });
    try {
      const bytes = await readFileBytes(file);
      const inspection = inspectWasm(bytes);
      const lines = disassembleWasm(bytes);
      // Ignore results for a file the user has already replaced.
      if (request === requestRef.current) setState({ status: "ready", fileName: file.name, inspection, lines });
    } catch (error) {
      if (request === requestRef.current) {
        setState({ status: "error", message: error instanceof Error ? error.message : "Could not decode this file." });
      }
    }
  };

  const ready = state.status === "ready" ? state : null;
  const meta = ready ? new Map(ready.inspection.meta.map(({ key, value }) => [key, value])) : null;
  const overNetworkLimit = ready ? ready.inspection.byteLength > SOROBAN_MAINNET_LIMITS.contractMaxSizeBytes : false;

  return (
    <section aria-labelledby={titleId} className="my-6 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03]">
      <header className="border-b border-white/10 px-4 py-3">
        <h3 id={titleId} className="text-base font-bold text-base-50">
          Soroban WASM disassembler
        </h3>
        <p className="mt-1 text-xs text-base-300">Decoded entirely in your browser — the file is never uploaded.</p>
      </header>

      <div className="space-y-4 p-4">
        <div>
          <label htmlFor={inputId} className="mb-1 block text-sm font-semibold text-base-100">
            Compiled contract (.wasm)
          </label>
          <input
            id={inputId}
            type="file"
            accept=".wasm,application/wasm"
            onChange={handleFile}
            className="block w-full text-sm text-base-200 file:mr-3 file:rounded-md file:border file:border-brand-500/40 file:bg-brand-500/10 file:px-3 file:py-1.5 file:text-sm file:font-semibold file:text-brand-300"
          />
        </div>

        {state.status === "loading" && (
          <p role="status" className="text-sm text-base-200">
            Decoding {state.fileName}…
          </p>
        )}
        {state.status === "error" && (
          <p role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {state.message}
          </p>
        )}

        {ready && meta && (
          <>
            <dl className="grid gap-2 text-xs sm:grid-cols-2 lg:grid-cols-4">
              {[
                ["File", `${ready.fileName} · ${formatBytes(ready.inspection.byteLength)}`],
                [
                  "Env interface",
                  ready.inspection.interfaceVersion
                    ? `protocol ${ready.inspection.interfaceVersion.protocol}${ready.inspection.interfaceVersion.preRelease ? ` (pre-release ${ready.inspection.interfaceVersion.preRelease})` : ""}`
                    : "—",
                ],
                ["Rust compiler", meta.get("rsver") ?? "—"],
                ["Soroban SDK", meta.get("rssdkver")?.split("#")[0] ?? "—"],
              ].map(([term, value]) => (
                <div key={term} className="rounded-lg border border-white/10 bg-base-900/60 px-3 py-2">
                  <dt className="text-base-300">{term}</dt>
                  <dd className="mt-0.5 break-all font-mono text-base-50">{value}</dd>
                </div>
              ))}
            </dl>

            {(overNetworkLimit || ready.inspection.warnings.length > 0) && (
              <ul role="alert" className="space-y-1 rounded-md border border-gold-500/30 bg-gold-500/10 px-3 py-2 text-xs text-gold-300">
                {overNetworkLimit && (
                  <li>
                    {formatBytes(ready.inspection.byteLength)} exceeds the network&apos;s maximum contract size of{" "}
                    {formatBytes(SOROBAN_MAINNET_LIMITS.contractMaxSizeBytes)}; it cannot be uploaded as-is.
                  </li>
                )}
                {ready.inspection.warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
            )}

            <div role="group" aria-label="Disassembler view" className="flex gap-2">
              {(
                [
                  ["interface", "Interface"],
                  ["wat", "WAT"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  type="button"
                  aria-pressed={view === value}
                  onClick={() => setView(value)}
                  className={`rounded-md border px-3 py-1.5 text-sm font-medium ${
                    view === value
                      ? "border-brand-500/50 bg-brand-500/10 text-brand-300"
                      : "border-white/10 text-base-200 hover:border-brand-500/40"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>

            {view === "interface" ? <InterfaceView inspection={ready.inspection} /> : <WatViewer lines={ready.lines} />}
          </>
        )}
      </div>
    </section>
  );
}

export default WasmDisassembler;
