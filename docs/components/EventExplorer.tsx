"use client";

import { useState, useId, useMemo } from "react";
import {
  decodeSorobanXdr,
  fetchSorobanTxEvents,
  SAMPLE_SOROBAN_EVENTS,
  type DecodeResult,
  type DecodedContractEvent,
  type DecodedScVal,
} from "@/lib/xdr-decoder";

interface EventExplorerProps {
  /** Optional initial XDR or transaction hash */
  initialValue?: string;
  /** Optional title */
  title?: string;
  /** Optional custom CSS class */
  className?: string;
}

/** Pretty badge colors for Soroban event types */
const EVENT_TYPE_STYLES = {
  CONTRACT: "bg-brand-500/15 text-brand-300 border-brand-500/30",
  SYSTEM: "bg-sky-500/15 text-sky-300 border-sky-500/30",
  DIAGNOSTIC: "bg-gold-500/15 text-gold-300 border-gold-500/30",
};

/** Type badge colors */
const VALUE_TYPE_STYLES: Record<string, string> = {
  Address: "text-brand-300 bg-brand-500/10 border-brand-500/20",
  Symbol: "text-sky-300 bg-sky-500/10 border-sky-500/20",
  String: "text-emerald-300 bg-emerald-500/10 border-emerald-500/20",
  i128: "text-amber-300 bg-amber-500/10 border-amber-500/20",
  u128: "text-amber-300 bg-amber-500/10 border-amber-500/20",
  Vec: "text-purple-300 bg-purple-500/10 border-purple-500/20",
  Map: "text-indigo-300 bg-indigo-500/10 border-indigo-500/20",
  Bytes: "text-rose-300 bg-rose-500/10 border-rose-500/20",
  default: "text-base-300 bg-white/5 border-white/10",
};

/** Helper to copy text to clipboard with feedback */
function useCopy() {
  const [copiedId, setCopiedId] = useState<string | null>(null);

  async function copy(text: string, id: string) {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopiedId(id);
      setTimeout(() => setCopiedId(null), 1800);
    } catch {
      /* clipboard write failed */
    }
  }

  return { copiedId, copy };
}

/** Render a single ScVal payload in an accessible tree view */
function ScValNode({
  label,
  data,
  nodeId,
  depth = 0,
}: {
  label?: string;
  data: DecodedScVal;
  nodeId: string;
  depth?: number;
}) {
  const [expanded, setExpanded] = useState(true);
  const { copiedId, copy } = useCopy();
  const isComplex =
    data.type === "Map" ||
    data.type === "Vec" ||
    (typeof data.value === "object" && data.value !== null);

  const typeStyle = VALUE_TYPE_STYLES[data.type] || VALUE_TYPE_STYLES.default;

  if (isComplex) {
    const isMap = data.type === "Map";
    const entries = isMap
      ? Object.entries(data.value || {})
      : Array.isArray(data.value)
      ? data.value
      : [];

    return (
      <div className={`mt-1 text-xs ${depth > 0 ? "ml-4 border-l border-white/10 pl-3" : ""}`}>
        <div className="flex items-center gap-2 py-0.5">
          <button
            type="button"
            onClick={() => setExpanded(!expanded)}
            aria-expanded={expanded}
            aria-label={`${expanded ? "Collapse" : "Expand"} ${label || data.type}`}
            className="flex items-center gap-1.5 font-mono text-base-300 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1"
          >
            <span className="text-[10px]" aria-hidden="true">
              {expanded ? "▼" : "▶"}
            </span>
            {label && <span className="font-semibold text-base-200">{label}:</span>}
            <span className={`rounded border px-1.5 py-0.2 text-[10px] uppercase tracking-wider ${typeStyle}`}>
              {data.type} ({entries.length})
            </span>
          </button>
          <button
            type="button"
            onClick={() => copy(data.rawJson, `node-${nodeId}`)}
            aria-label={`Copy ${label || data.type} JSON`}
            className="text-[10px] text-base-400 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1"
          >
            {copiedId === `node-${nodeId}` ? "✓ Copied" : "Copy"}
          </button>
        </div>

        {expanded && (
          <div className="mt-1 space-y-1">
            {isMap
              ? entries.map(([key, val], idx) => (
                  <div key={key} className="flex items-start gap-1 font-mono">
                    <span className="text-base-400 font-medium">{key}:</span>
                    {typeof val === "object" && val !== null ? (
                      <ScValNode
                        data={{ type: Array.isArray(val) ? "Vec" : "Map", value: val, rawJson: JSON.stringify(val) }}
                        nodeId={`${nodeId}-${idx}`}
                        depth={depth + 1}
                      />
                    ) : (
                      <span className="text-base-200">
                        {typeof val === "string" ? `"${val}"` : String(val)}
                      </span>
                    )}
                  </div>
                ))
              : entries.map((item: any, idx: number) => (
                  <div key={idx} className="flex items-start gap-1 font-mono">
                    <span className="text-base-400">[{idx}]:</span>
                    {typeof item === "object" && item !== null ? (
                      <ScValNode
                        data={{ type: Array.isArray(item) ? "Vec" : "Map", value: item, rawJson: JSON.stringify(item) }}
                        nodeId={`${nodeId}-${idx}`}
                        depth={depth + 1}
                      />
                    ) : (
                      <span className="text-base-200">
                        {typeof item === "string" ? `"${item}"` : String(item)}
                      </span>
                    )}
                  </div>
                ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-wrap items-center gap-2 py-0.5 text-xs font-mono">
      {label && <span className="font-semibold text-base-300">{label}:</span>}
      <span className={`rounded border px-1.5 py-0.2 text-[10px] font-semibold uppercase ${typeStyle}`}>
        {data.type}
      </span>
      <span className="text-base-100 break-all select-all font-mono">
        {typeof data.value === "string" ? `"${data.value}"` : JSON.stringify(data.value)}
      </span>
      <button
        type="button"
        onClick={() => copy(typeof data.value === "string" ? data.value : data.rawJson, `val-${nodeId}`)}
        aria-label={`Copy ${label || "value"}`}
        className="text-[10px] text-base-400 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1"
      >
        {copiedId === `val-${nodeId}` ? "✓ Copied" : "Copy"}
      </button>
    </div>
  );
}

/**
 * Production-grade Soroban Transaction Explorer & Event Log Viewer:
 * - Decodes raw base64 Soroban TransactionMeta, ContractEvent, and ScVal XDR client-side
 * - Direct Soroban RPC testnet transaction lookup by 64-character hash
 * - Collapsible tree inspector for event topics, data payloads, and contract IDs
 * - One-click copy buttons for XDR fragments, addresses, and JSON representations
 * - Fully accessible and screen-reader compliant
 */
export function EventExplorer({
  initialValue = "",
  title = "Soroban Event Explorer & XDR Decoder",
  className = "",
}: EventExplorerProps) {
  const inputId = useId();
  const [inputText, setInputText] = useState<string>(
    initialValue || SAMPLE_SOROBAN_EVENTS[0].xdr
  );
  const [loading, setLoading] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [decodedResult, setDecodedResult] = useState<DecodeResult | null>(() => {
    return decodeSorobanXdr(initialValue || SAMPLE_SOROBAN_EVENTS[0].xdr);
  });

  const [expandedEvents, setExpandedEvents] = useState<Record<string, boolean>>({
    "event-0": true,
  });

  const { copiedId, copy } = useCopy();

  function toggleEvent(id: string) {
    setExpandedEvents((prev) => ({
      ...prev,
      [id]: !prev[id],
    }));
  }

  async function handleDecode() {
    setErrorMessage(null);
    const trimmed = inputText.trim();
    if (!trimmed) {
      setErrorMessage("Please enter an XDR string or 64-character transaction hash.");
      setDecodedResult(null);
      return;
    }

    // Check if it's a 64-character hex transaction hash
    if (/^[0-9a-fA-F]{64}$/.test(trimmed)) {
      setLoading(true);
      try {
        const rpcResult = await fetchSorobanTxEvents(trimmed);
        if (rpcResult.success) {
          setDecodedResult(rpcResult);
          // Expand all events by default
          const exp: Record<string, boolean> = {};
          rpcResult.events.forEach((_, idx) => {
            exp[`event-${idx}`] = true;
          });
          setExpandedEvents(exp);
        } else {
          setErrorMessage(rpcResult.error || "Failed to decode transaction events.");
          setDecodedResult(null);
        }
      } catch (err) {
        setErrorMessage(err instanceof Error ? err.message : "Error fetching from RPC.");
        setDecodedResult(null);
      } finally {
        setLoading(false);
      }
      return;
    }

    // Otherwise, decode as raw base64 XDR
    const result = decodeSorobanXdr(trimmed);
    if (result.success) {
      setDecodedResult(result);
      const exp: Record<string, boolean> = {};
      result.events.forEach((_, idx) => {
        exp[`event-${idx}`] = true;
      });
      setExpandedEvents(exp);
    } else {
      setErrorMessage(result.error || "Failed to decode XDR.");
      setDecodedResult(null);
    }
  }

  function handleSelectSample(sampleXdr: string) {
    setInputText(sampleXdr);
    setErrorMessage(null);
    const result = decodeSorobanXdr(sampleXdr);
    setDecodedResult(result);
    setExpandedEvents({ "event-0": true });
  }

  return (
    <div
      className={`my-6 rounded-xl border border-white/10 bg-base-950/80 p-4 shadow-xl backdrop-blur-md ${className}`}
      data-testid="event-explorer"
    >
      {/* Header */}
      <div className="mb-4 flex flex-wrap items-center justify-between gap-2 border-b border-white/10 pb-3">
        <div>
          <h3 className="text-base font-bold text-base-50">{title}</h3>
          <p className="text-xs text-base-300">
            Decode client-side base64 Soroban <code className="text-brand-300">ContractEvent</code>,{" "}
            <code className="text-brand-300">TransactionMeta</code>, or query live testnet transaction hashes.
          </p>
        </div>
      </div>

      {/* Preset Sample Quick Selectors */}
      <div className="mb-3">
        <span className="mr-2 text-xs font-semibold text-base-300">Sample presets:</span>
        <div className="mt-1.5 flex flex-wrap gap-2">
          {SAMPLE_SOROBAN_EVENTS.map((sample) => (
            <button
              key={sample.name}
              type="button"
              onClick={() => handleSelectSample(sample.xdr)}
              aria-label={`Load sample: ${sample.name}`}
              className="rounded-lg border border-white/10 bg-base-900/60 px-2.5 py-1 text-xs font-medium text-base-200 transition-colors hover:border-brand-500/40 hover:bg-base-800 hover:text-base-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
            >
              {sample.name}
            </button>
          ))}
        </div>
      </div>

      {/* Input Area */}
      <div className="mb-4">
        <label htmlFor={inputId} className="mb-1 block text-xs font-semibold text-base-300">
          Base64 Soroban XDR or 64-char Tx Hash:
        </label>
        <textarea
          id={inputId}
          rows={3}
          value={inputText}
          onChange={(e) => setInputText(e.target.value)}
          placeholder="Paste base64 ContractEvent / TransactionMeta XDR or 64-char hex transaction hash..."
          className="w-full rounded-lg border border-white/10 bg-base-900 px-3 py-2 font-mono text-xs text-base-100 placeholder-base-400 focus:border-brand-500/60 focus:outline-none focus:ring-2 focus:ring-brand-500/20"
        />
        <div className="mt-2 flex flex-wrap items-center justify-between gap-2">
          <button
            type="button"
            onClick={handleDecode}
            disabled={loading}
            className="flex items-center gap-2 rounded-lg bg-brand-500 px-4 py-2 text-xs font-bold text-base-950 transition-colors hover:bg-brand-400 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-300 disabled:opacity-60"
          >
            {loading ? (
              <>
                <span className="h-3 w-3 animate-spin rounded-full border-2 border-base-950 border-t-transparent" aria-hidden="true" />
                <span>Querying RPC &amp; Decoding…</span>
              </>
            ) : (
              <span>Decode &amp; Inspect Events</span>
            )}
          </button>
          <button
            type="button"
            onClick={() => setInputText("")}
            className="text-xs text-base-300 hover:text-base-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-2 py-1"
          >
            Clear
          </button>
        </div>
      </div>

      {/* Error Message Display */}
      {errorMessage && (
        <div role="alert" className="mb-4 rounded-lg border border-red-500/30 bg-red-500/10 p-3 text-xs text-red-300">
          <strong>Error: </strong> {errorMessage}
        </div>
      )}

      {/* Decoded Results Section */}
      {decodedResult && decodedResult.success && (
        <div className="mt-4 space-y-4" aria-live="polite">
          {/* Summary Metadata Bar */}
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-white/10 bg-base-900/60 p-3 text-xs">
            <div className="flex flex-wrap items-center gap-3">
              <span className="font-semibold text-base-100">
                Decoded Events: <strong className="text-brand-300">{decodedResult.totalEvents}</strong>
              </span>
              <span className="rounded border border-white/10 bg-white/5 px-2 py-0.5 font-mono text-[11px] text-base-300">
                Source: {decodedResult.sourceType}
              </span>
              {decodedResult.contractCount > 0 && (
                <span className="text-base-300">
                  Contracts: <strong className="text-base-100">{decodedResult.contractCount}</strong>
                </span>
              )}
            </div>

            <button
              type="button"
              onClick={() => copy(decodedResult.rawJson, "full-json")}
              aria-label="Copy full decoded JSON representation"
              className="flex items-center gap-1.5 rounded-md border border-white/10 bg-base-800 px-2.5 py-1 text-xs font-medium text-base-200 hover:border-brand-500/40 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400"
            >
              {copiedId === "full-json" ? "✓ JSON Copied!" : "Copy Full JSON"}
            </button>
          </div>

          {/* Event Tree List */}
          <div className="space-y-3">
            {decodedResult.events.map((event, idx) => {
              const eventKey = `event-${idx}`;
              const isExpanded = !!expandedEvents[eventKey];
              const eventTypeClass = EVENT_TYPE_STYLES[event.type] || EVENT_TYPE_STYLES.CONTRACT;

              return (
                <div
                  key={eventKey}
                  className="overflow-hidden rounded-xl border border-white/10 bg-[#0c0f17] shadow-md transition-all"
                >
                  {/* Event Accordion Header */}
                  <div className="flex flex-wrap items-center justify-between gap-2 border-b border-white/5 bg-base-900/40 px-4 py-2.5">
                    <div className="flex items-center gap-2.5">
                      <button
                        type="button"
                        onClick={() => toggleEvent(eventKey)}
                        aria-expanded={isExpanded}
                        aria-controls={`event-body-${idx}`}
                        className="flex items-center gap-2 text-left font-mono text-xs font-semibold text-base-100 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1"
                      >
                        <span className="text-[10px] text-base-400" aria-hidden="true">
                          {isExpanded ? "▼" : "▶"}
                        </span>
                        <span>Event #{idx + 1}</span>
                      </button>

                      <span className={`rounded-md border px-2 py-0.5 text-[10px] font-bold ${eventTypeClass}`}>
                        {event.type}
                      </span>
                    </div>

                    {/* Contract ID with Copy button */}
                    {event.contractId && (
                      <div className="flex items-center gap-1.5 text-xs font-mono">
                        <span className="text-base-400">Contract:</span>
                        <span className="rounded bg-white/5 px-1.5 py-0.5 text-base-200 select-all">
                          {event.contractId.slice(0, 8)}…{event.contractId.slice(-6)}
                        </span>
                        <button
                          type="button"
                          onClick={() => copy(event.contractId || "", `contract-${idx}`)}
                          aria-label={`Copy Contract ID ${event.contractId}`}
                          className="text-[10px] text-base-400 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1"
                        >
                          {copiedId === `contract-${idx}` ? "✓ Copied" : "Copy"}
                        </button>
                      </div>
                    )}
                  </div>

                  {/* Event Accordion Body */}
                  {isExpanded && (
                    <div id={`event-body-${idx}`} className="p-4 space-y-4">
                      {/* Topics Section */}
                      <div>
                        <div className="mb-2 flex items-center justify-between border-b border-white/5 pb-1">
                          <h4 className="text-xs font-bold uppercase tracking-wider text-base-300">
                            Topics ({event.topics.length})
                          </h4>
                          <span className="text-[11px] text-base-400">Indexed filter keys</span>
                        </div>
                        {event.topics.length > 0 ? (
                          <div className="space-y-1.5 rounded-lg border border-white/5 bg-base-950/60 p-3">
                            {event.topics.map((topic, tIdx) => (
                              <ScValNode
                                key={tIdx}
                                label={`Topic [${tIdx}]`}
                                data={topic}
                                nodeId={`topic-${idx}-${tIdx}`}
                              />
                            ))}
                          </div>
                        ) : (
                          <p className="text-xs italic text-base-400">No topics emitted.</p>
                        )}
                      </div>

                      {/* Data Payload Section */}
                      <div>
                        <div className="mb-2 flex items-center justify-between border-b border-white/5 pb-1">
                          <h4 className="text-xs font-bold uppercase tracking-wider text-base-300">
                            Data Payload
                          </h4>
                          <button
                            type="button"
                            onClick={() => copy(event.data.rawJson, `data-${idx}`)}
                            aria-label={`Copy event ${idx + 1} data payload JSON`}
                            className="text-[10px] text-base-300 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1.5 py-0.5"
                          >
                            {copiedId === `data-${idx}` ? "✓ Payload Copied" : "Copy Payload JSON"}
                          </button>
                        </div>
                        <div className="rounded-lg border border-white/5 bg-base-950/60 p-3">
                          <ScValNode
                            label="Payload"
                            data={event.data}
                            nodeId={`data-${idx}`}
                          />
                        </div>
                      </div>

                      {/* Raw XDR Fragment with Copy */}
                      {event.rawXdrBase64 && (
                        <div>
                          <div className="mb-1 flex items-center justify-between">
                            <span className="text-[11px] font-semibold text-base-400">
                              Raw Event Base64 XDR:
                            </span>
                            <button
                              type="button"
                              onClick={() => copy(event.rawXdrBase64, `xdr-${idx}`)}
                              aria-label={`Copy raw XDR fragment for event ${idx + 1}`}
                              className="text-[10px] text-base-300 hover:text-brand-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-brand-400 rounded px-1.5 py-0.5"
                            >
                              {copiedId === `xdr-${idx}` ? "✓ XDR Copied" : "Copy XDR"}
                            </button>
                          </div>
                          <pre className="overflow-x-auto rounded-lg border border-white/5 bg-black/50 p-2 text-[11px] font-mono text-base-300 select-all">
                            <code>{event.rawXdrBase64}</code>
                          </pre>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

export default EventExplorer;
