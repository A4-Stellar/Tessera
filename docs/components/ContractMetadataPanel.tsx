"use client";

import { useId, useMemo } from "react";
import {
  extractContractMetadata,
  type ContractMetadataSummary,
  type WasmInspection,
} from "@/lib/wasm-inspector";

export interface ContractMetadataPanelProps {
  /** Raw compiled `.wasm` binary bytes from which `contractenvmeta` and `contractmetav0` sections are extracted. */
  wasmBytes?: Uint8Array;
  /** Pre-decoded `WasmInspection` result to summarize when binary bytes have already been parsed. */
  inspection?: WasmInspection;
  /** Pre-extracted contract metadata summary object. */
  metadata?: ContractMetadataSummary;
  /** Optional `.wasm` file name used as a fallback contract identifier. */
  fileName?: string;
  /** Extra CSS classes for the outer section element. */
  className?: string;
}

const DEFAULT_METADATA: ContractMetadataSummary = {
  contractName: "tessera_asset_token",
  sdkVersion: "25.3.2",
  interfaceHash: "0x8f4e2b91c03d7a65",
  protocolVersion: 25,
  preRelease: 0,
  rustVersion: "1.98.0",
  byteLength: 18432,
  customSections: ["contractspecv0", "contractenvmetav0", "contractmetav0"],
  rawMeta: [
    { key: "rsver", value: "1.98.0" },
    { key: "rssdkver", value: "25.3.2" },
  ],
  envMetaEntries: [
    { key: "protocol", value: "25" },
    { key: "pre_release", value: "0" },
  ],
};

/**
 * Soroban contract metadata panel: extracts the `contractenvmeta` / `contractenvmetav0`
 * and `contractmetav0` custom sections from a WebAssembly binary and displays the
 * Soroban SDK version, contract name, interface hash, environment protocol version,
 * and Rust compiler version.
 */
export function ContractMetadataPanel({
  wasmBytes,
  inspection,
  metadata,
  fileName,
  className = "",
}: ContractMetadataPanelProps) {
  const titleId = useId();

  const resolved = useMemo<{ summary: ContractMetadataSummary; error: string | null }>(() => {
    try {
      if (metadata) return { summary: metadata, error: null };
      if (wasmBytes) return { summary: extractContractMetadata(wasmBytes, fileName), error: null };
      if (inspection) return { summary: extractContractMetadata(inspection, fileName), error: null };
      return { summary: DEFAULT_METADATA, error: null };
    } catch (err) {
      return {
        summary: DEFAULT_METADATA,
        error: err instanceof Error ? err.message : "Could not extract contract metadata.",
      };
    }
  }, [wasmBytes, inspection, metadata, fileName]);

  const { summary, error } = resolved;
  const protocolLabel =
    summary.protocolVersion !== null
      ? `protocol ${summary.protocolVersion}${summary.preRelease ? ` (pre-release ${summary.preRelease})` : ""}`
      : "—";

  const combinedEntries = useMemo(() => {
    const seen = new Set<string>();
    const list: { key: string; value: string; section: string }[] = [];
    for (const entry of summary.envMetaEntries) {
      const id = `env:${entry.key}`;
      if (!seen.has(id)) {
        seen.add(id);
        list.push({ ...entry, section: "contractenvmeta" });
      }
    }
    for (const entry of summary.rawMeta) {
      const id = `meta:${entry.key}`;
      if (!seen.has(id)) {
        seen.add(id);
        list.push({ ...entry, section: "contractmetav0" });
      }
    }
    return list;
  }, [summary]);

  return (
    <section
      aria-labelledby={titleId}
      className={`my-4 overflow-hidden rounded-xl border border-white/10 bg-white/[0.03] ${className}`}
    >
      <header className="border-b border-white/10 px-4 py-3">
        <h4 id={titleId} className="text-sm font-bold text-base-50">
          Contract metadata
        </h4>
        <p className="mt-0.5 text-xs text-base-300">
          Extracted from <code>contractenvmeta</code> and <code>contractmetav0</code> custom sections.
        </p>
      </header>

      <div className="space-y-4 p-4">
        {error ? (
          <p role="alert" className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
          </p>
        ) : (
          <>
            <dl className="grid gap-2 text-xs sm:grid-cols-2 lg:grid-cols-5">
              {[
                ["Contract name", summary.contractName],
                ["Soroban SDK", summary.sdkVersion],
                ["Interface hash", summary.interfaceHash],
                ["Env protocol", protocolLabel],
                ["Rust compiler", summary.rustVersion],
              ].map(([term, value]) => (
                <div key={term} className="rounded-lg border border-white/10 bg-base-900/60 px-3 py-2">
                  <dt className="text-base-300">{term}</dt>
                  <dd className="mt-0.5 break-all font-mono text-base-50">{value}</dd>
                </div>
              ))}
            </dl>

            {combinedEntries.length > 0 && (
              <div className="overflow-x-auto">
                <table className="w-full text-left text-xs" aria-label="Custom section metadata entries">
                  <thead className="text-base-300">
                    <tr>
                      <th scope="col" className="py-1.5 pr-4 font-semibold">
                        Section
                      </th>
                      <th scope="col" className="py-1.5 pr-4 font-semibold">
                        Key
                      </th>
                      <th scope="col" className="py-1.5 font-semibold">
                        Value
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {combinedEntries.map((entry) => (
                      <tr key={`${entry.section}:${entry.key}`} className="border-t border-white/5">
                        <td className="py-1.5 pr-4 font-mono text-base-300">{entry.section}</td>
                        <td className="py-1.5 pr-4 font-mono text-base-100">{entry.key}</td>
                        <td className="py-1.5 font-mono text-base-50">{`${entry.key}=${entry.value}`}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}

export default ContractMetadataPanel;
