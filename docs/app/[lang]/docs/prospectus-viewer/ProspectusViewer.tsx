"use client";

import React, { useEffect, useState } from "react";
import { normalizeSha3, sha3_256 } from "./sha3";

export interface ProspectusViewerProps {
  /** IPFS Content Identifier (CID) for the prospectus PDF. */
  ipfsCid?: string;
  /** Direct IPFS or gateway URL. If omitted, constructed from `gatewayUrl` + `ipfsCid`. */
  ipfsUrl?: string;
  /** IPFS gateway base URL; defaults to Pinata gateway. */
  gatewayUrl?: string;
  /** Expected on-chain SHA-3-256 hash (hex, optionally 0x or sha3: prefixed). */
  expectedHash?: string;
  /** Alias for expectedHash */
  expectedSha3?: string;
  /** Document title for accessible iframe labeling. */
  title?: string;
  /** Extra container styling. */
  className?: string;
}

type Status = "idle" | "loading" | "verified" | "mismatch" | "error";

export function ProspectusViewer({
  ipfsCid,
  ipfsUrl,
  gatewayUrl = "https://gateway.pinata.cloud/ipfs/",
  expectedHash,
  expectedSha3,
  title = "Asset Legal Prospectus",
  className = "",
}: ProspectusViewerProps) {
  const [status, setStatus] = useState<Status>("idle");
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [computedDigest, setComputedDigest] = useState<string | null>(null);

  const rawExpected = expectedSha3 || expectedHash;
  const targetHash = rawExpected ? normalizeSha3(rawExpected) : null;

  // Resolve document fetch URL
  const documentUrl =
    ipfsUrl ||
    (ipfsCid ? `${gatewayUrl.replace(/\/+$/, "")}/${ipfsCid}` : null);

  useEffect(() => {
    let active = true;
    let createdUrl: string | null = null;

    async function loadAndVerify() {
      if (!documentUrl) {
        setStatus("idle");
        return;
      }

      setStatus("loading");
      setErrorMessage(null);

      try {
        const response = await fetch(documentUrl, { credentials: "omit" });
        if (!response.ok) {
          throw new Error(`Failed to load PDF from gateway: HTTP ${response.status}`);
        }

        const buffer = await response.arrayBuffer();
        if (buffer.byteLength === 0) {
          throw new Error("Received empty document from gateway");
        }

        // 1. Client-Side SHA-3-256 Verification
        const hash = sha3_256(buffer);
        if (!active) return;
        setComputedDigest(hash);

        if (targetHash) {
          if (hash.toLowerCase() !== targetHash.toLowerCase()) {
            setStatus("mismatch");
            setErrorMessage(
              `SHA-3 hash verification failed: on-chain digest (${targetHash.slice(0, 10)}…) does not match downloaded document (${hash.slice(0, 10)}…)`
            );
            return;
          }
        }

        // 2. Prepare Object URL for Sandboxed Iframe Display
        const blob = new Blob([buffer], { type: "application/pdf" });
        createdUrl = URL.createObjectURL(blob);
        if (!active) {
          URL.revokeObjectURL(createdUrl);
          return;
        }

        setBlobUrl(createdUrl);
        setStatus("verified");
      } catch (err: unknown) {
        if (!active) return;
        setStatus("error");
        setErrorMessage(
          err instanceof Error ? err.message : "Error verifying prospectus document"
        );
      }
    }

    loadAndVerify();

    return () => {
      active = false;
      if (createdUrl) {
        URL.revokeObjectURL(createdUrl);
      }
    };
  }, [documentUrl, targetHash]);

  return (
    <div className={`flex flex-col w-full rounded-xl border border-neutral-800 bg-neutral-900/60 p-4 ${className}`}>
      {/* Header bar */}
      <div className="flex items-center justify-between pb-3 border-b border-neutral-800">
        <div>
          <h3 className="text-sm font-medium text-neutral-100">{title}</h3>
          {ipfsCid && (
            <p className="text-xs font-mono text-neutral-400 mt-0.5">
              CID: {ipfsCid}
            </p>
          )}
        </div>

        <div>
          {status === "loading" && (
            <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium bg-neutral-800 text-neutral-300">
              <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-pulse" />
              Verifying SHA-3…
            </span>
          )}

          {status === "verified" && (
            <span
              className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-emerald-500/10 text-emerald-400 border border-emerald-500/20"
              data-testid="hash-verified-badge"
            >
              hash verified ✓
            </span>
          )}

          {status === "mismatch" && (
            <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-red-500/10 text-red-400 border border-red-500/20">
              verification failed ✗
            </span>
          )}
        </div>
      </div>

      {/* Body / Iframe display */}
      <div className="mt-4 min-h-[480px] flex items-center justify-center bg-black/40 rounded-lg overflow-hidden relative">
        {status === "loading" && (
          <div className="flex flex-col items-center justify-center gap-2 p-8 text-center text-neutral-400 text-sm">
            <div className="w-6 h-6 border-2 border-neutral-600 border-t-amber-400 rounded-full animate-spin" />
            <p>Fetching PDF from IPFS gateway and verifying SHA-3 integrity…</p>
          </div>
        )}

        {status === "mismatch" && (
          <div className="p-6 text-center text-red-400 max-w-md">
            <p className="font-semibold text-base mb-1">Hash Verification Failed</p>
            <p className="text-xs text-red-300/80 mb-2">{errorMessage}</p>
            <p className="text-xs text-neutral-400">
              Document rendering is blocked to prevent disclosure of unverified or tampered prospectus filings.
            </p>
          </div>
        )}

        {status === "error" && (
          <div className="p-6 text-center text-red-400 max-w-md">
            <p className="font-semibold text-base mb-1">Unable to Load Prospectus</p>
            <p className="text-xs text-neutral-400">{errorMessage}</p>
          </div>
        )}

        {status === "verified" && blobUrl && (
          <iframe
            src={blobUrl}
            title={title}
            sandbox="allow-scripts allow-same-origin"
            className="w-full h-[600px] border-0 rounded-lg"
          />
        )}
      </div>

      {/* Footer hash summary */}
      {computedDigest && (
        <div className="mt-3 pt-2 text-[11px] font-mono text-neutral-400 flex items-center justify-between">
          <span>SHA-3-256: {computedDigest}</span>
          {targetHash && status === "verified" && (
            <span className="text-emerald-400">matches on-chain attestation</span>
          )}
        </div>
      )}
    </div>
  );
}

export default ProspectusViewer;
