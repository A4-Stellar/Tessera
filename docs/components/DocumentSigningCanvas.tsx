"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import * as Freighter from "@stellar/freighter-api";
import { ProspectusViewer, sha256Hex } from "./ProspectusViewer";
import { useFreighterWallet } from "../hooks/useFreighterWallet";

export type DocumentSignature = { assetId: string; documentHash: string; timestamp: string; signer: string; signature: string; signatureHash: string };
export interface DocumentSigningCanvasProps {
  assetId: string;
  documentUrl: string;
  /** SHA-256 committed by the issuer/on-chain registry. */
  expectedDocumentHash?: string;
  /** Server endpoint which verifies the wallet signature and persists its hash. */
  recordEndpoint?: string;
  title?: string;
}

function canonicalPayload(assetId: string, documentHash: string, timestamp: string) {
  return JSON.stringify({ AssetID: assetId, DocumentHash: documentHash, Timestamp: timestamp });
}
function resultError(value: unknown): string | null { return typeof value === "object" && value !== null && "error" in value && (value as { error?: { message?: string } }).error?.message || null; }
function responseSignature(value: unknown): string | null { if (typeof value !== "object" || !value) return null; const record = value as Record<string, unknown>; return typeof record.signature === "string" ? record.signature : typeof record.signedMessage === "string" ? record.signedMessage : null; }

/** Legal-document review + Freighter signing. The API is authoritative for signature verification. */
export function DocumentSigningCanvas({ assetId, documentUrl, expectedDocumentHash, recordEndpoint = "/api/document-signatures", title = "Legal document" }: DocumentSigningCanvasProps) {
  const wallet = useFreighterWallet(); const [hash, setHash] = useState(expectedDocumentHash?.replace(/^sha256:/i, "") ?? ""); const [status, setStatus] = useState("Review the document and verify its hash before signing."); const [signature, setSignature] = useState<DocumentSignature | null>(null); const [busy, setBusy] = useState(false);
  const hashMatches = !expectedDocumentHash || hash.toLowerCase() === expectedDocumentHash.replace(/^sha256:/i, "").toLowerCase();
  useEffect(() => { let cancelled = false; void (async () => { try { const response = await fetch(documentUrl, { credentials: "omit", cache: "no-store" }); if (!response.ok) throw new Error(`Document fetch failed (${response.status})`); const computed = await sha256Hex(await response.arrayBuffer()); if (!cancelled) { setHash(computed); setStatus("Document hash calculated. Compare it with the issuer commitment."); } } catch (error) { if (!cancelled) setStatus(error instanceof Error ? error.message : "Could not calculate document hash."); } })(); return () => { cancelled = true; }; }, [documentUrl]);
  const payload = useMemo(() => canonicalPayload(assetId, hash, new Date().toISOString()), [assetId, hash]);
  const sign = useCallback(async () => { if (!wallet.address) { await wallet.connect(); return; } if (!hash || !hashMatches) { setStatus("Signing is disabled until the PDF hash matches the expected document hash."); return; } setBusy(true); setStatus("Waiting for Freighter signature…"); try {
    // Freighter versions expose signMessage with slightly different response shapes;
    // keep this boundary narrow and submit the raw wallet proof to the verifier.
    const signer = (Freighter as unknown as { signMessage?: (message: string, options: { address: string }) => Promise<unknown> }).signMessage;
    if (!signer) throw new Error("This Freighter version does not support message signing. Update the extension.");
    const result = await signer(payload, { address: wallet.address }); const message = resultError(result); if (message) throw new Error(message); const rawSignature = responseSignature(result); if (!rawSignature) throw new Error("Freighter did not return a signature.");
    const signatureHash = await sha256Hex(new TextEncoder().encode(rawSignature)); const signed: DocumentSignature = { assetId, documentHash: hash, timestamp: JSON.parse(payload).Timestamp, signer: wallet.address, signature: rawSignature, signatureHash };
    const response = await fetch(recordEndpoint, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ payload: JSON.parse(payload), ...signed }) }); if (!response.ok) throw new Error(`Signature verification/recording failed (${response.status}).`); const record = await response.json() as { valid?: boolean; signatureHash?: string }; if (!record.valid) throw new Error("The signature verifier rejected this wallet proof."); setSignature({ ...signed, signatureHash: record.signatureHash ?? signatureHash }); setStatus("Signature verified and recorded.");
  } catch (error) { setStatus(error instanceof Error ? error.message : "Document signing failed."); } finally { setBusy(false); } }, [assetId, hash, hashMatches, payload, recordEndpoint, wallet]);
  return <section className="my-6 rounded-xl border border-white/10 bg-white/[0.03] p-4"><h3 className="text-lg font-semibold">Document signing</h3><div className="mt-4 grid gap-4 lg:grid-cols-2"><ProspectusViewer title={title} src={documentUrl} expectedHash={expectedDocumentHash} /><aside className="rounded-lg border border-white/10 bg-black/20 p-4"><p className="text-sm text-base-300">Asset ID</p><code className="break-all text-sm">{assetId}</code><p className="mt-4 text-sm text-base-300">SHA-256 document hash</p><code className="block break-all text-xs">{hash || "Calculating…"}</code><p className={`mt-2 text-sm ${hashMatches ? "text-emerald-300" : "text-rose-300"}`}>{hashMatches ? "Hash matches issuer commitment" : "Hash does not match issuer commitment"}</p><p className="mt-4 text-sm text-base-300">Typed payload</p><pre className="overflow-x-auto rounded bg-black/30 p-2 text-xs">{payload}</pre>{wallet.address ? <p className="mt-3 text-xs text-emerald-300">Connected: {wallet.address}</p> : <button type="button" onClick={wallet.connect} className="mt-3 rounded border border-white/20 px-3 py-2 text-sm">Connect Freighter</button>}<button type="button" disabled={busy || !hashMatches} onClick={sign} className="mt-3 block rounded bg-brand-500 px-3 py-2 text-sm text-white disabled:opacity-50">{busy ? "Signing…" : "Sign typed payload"}</button><p role="status" className="mt-3 text-sm text-base-300">{status}</p>{signature && <div className="mt-4 text-xs"><p className="text-emerald-300">Verified signature hash</p><code className="break-all">{signature.signatureHash}</code></div>}</aside></div></section>;
}
