"use client";

import { useEffect, useMemo, useState } from "react";
import { getAddress, requestAccess } from "@stellar/freighter-api";
import { normalizeSha256, sha256Hex } from "./ProspectusViewer";

export type DocumentSigningPayload = {
  AssetID: string;
  DocumentHash: string;
  Timestamp: string;
};

export interface DocumentSigningCanvasProps {
  assetId: string;
  /** HTTPS URL to the legal PDF. */
  documentUrl: string;
  /** Expected SHA-256 (hex, with optional sha256:/0x prefix). */
  documentHash: string;
  /** Optional API endpoint receiving the signed record. */
  recordEndpoint?: string;
  className?: string;
}

type SigningStatus = "idle" | "checking" | "verified" | "mismatch" | "signing" | "signed" | "recording" | "recorded" | "error";

function asMessage(payload: DocumentSigningPayload): string {
  // Stable field order is essential: the exact bytes below are what both the
  // wallet and the verification service hash and verify.
  return JSON.stringify({ type: "TesseraDocumentSignature", version: "1", payload });
}

function messageFromResult(result: unknown): string | null {
  if (!result || typeof result !== "object") return null;
  const record = result as Record<string, unknown>;
  for (const key of ["signedMessage", "signature", "signedMessageBase64"]) {
    if (typeof record[key] === "string" && record[key]) return record[key] as string;
  }
  return null;
}

/**
 * Legal-document signing flow. Freighter signs the canonical typed message;
 * private key material never enters this component or the API request.
 */
export function DocumentSigningCanvas({ assetId, documentUrl, documentHash, recordEndpoint, className = "" }: DocumentSigningCanvasProps) {
  const [status, setStatus] = useState<SigningStatus>("idle");
  const [actualHash, setActualHash] = useState<string | null>(null);
  const [signature, setSignature] = useState<string | null>(null);
  const [signatureHash, setSignatureHash] = useState<string | null>(null);
  const [signer, setSigner] = useState<string | null>(null);
  const [timestamp, setTimestamp] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const expectedHash = useMemo(() => normalizeSha256(documentHash), [documentHash]);

  async function verifyDocument() {
    setError(null);
    setStatus("checking");
    try {
      if (!expectedHash) throw new Error("The supplied document hash is not a SHA-256 digest.");
      const response = await fetch(documentUrl, { credentials: "omit", cache: "no-store" });
      if (!response.ok) throw new Error(`Unable to load legal document (${response.status}).`);
      const hash = await sha256Hex(await response.arrayBuffer());
      setActualHash(hash);
      setStatus(hash === expectedHash ? "verified" : "mismatch");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Unable to verify the document.");
      setStatus("error");
    }
  }

  useEffect(() => { void verifyDocument(); }, [documentUrl, expectedHash]); // Revalidate whenever the source or expected on-chain hash changes.

  async function sign() {
    if (status !== "verified" || !expectedHash) return;
    setError(null);
    setStatus("signing");
    try {
      const access = await requestAccess();
      if (access.error) throw new Error(access.error.message);
      const addressResult = await getAddress();
      if (addressResult.error || !addressResult.address) throw new Error(addressResult.error?.message || "No Freighter account is available.");
      const signedAt = new Date().toISOString();
      const payload: DocumentSigningPayload = { AssetID: assetId, DocumentHash: expectedHash, Timestamp: signedAt };
      const message = asMessage(payload);
      // Freighter's API has added response fields over time; retain the
      // protocol boundary here while accepting the supported response forms.
      const freighter = await import("@stellar/freighter-api") as unknown as {
        signMessage?: (input: { message: string }) => Promise<unknown>;
      };
      if (!freighter.signMessage) throw new Error("This Freighter version does not support message signing.");
      const signed = await freighter.signMessage({ message });
      const encodedSignature = messageFromResult(signed);
      if (!encodedSignature) throw new Error("Freighter did not return a signature.");
      const hash = await sha256Hex(new TextEncoder().encode(encodedSignature));
      setSigner(addressResult.address);
      setTimestamp(signedAt);
      setSignature(encodedSignature);
      setSignatureHash(hash);
      setStatus("signed");

      if (recordEndpoint) {
        setStatus("recording");
        const recorded = await fetch(recordEndpoint, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ payload, signer: addressResult.address, signature: encodedSignature, signatureHash: hash }),
        });
        if (!recorded.ok) throw new Error(`Signature recording failed (${recorded.status}).`);
        setStatus("recorded");
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Document signing failed.");
      setStatus("error");
    }
  }

  return (
    <section className={`grid gap-5 rounded-xl border border-white/10 bg-white/[0.03] p-4 lg:grid-cols-2 ${className}`} data-testid="document-signing-canvas">
      <div>
        <div className="mb-2 flex items-center justify-between"><h3 className="font-bold text-base-50">Legal document</h3><button className="rounded border border-white/20 px-2 py-1 text-xs" onClick={() => void verifyDocument()}>Verify hash</button></div>
        <iframe title="Legal document preview" src={documentUrl} className="h-[32rem] w-full rounded border border-white/10 bg-white" sandbox="allow-same-origin" />
        <p className="mt-2 break-all text-xs text-base-300">SHA-256: {actualHash || "checking…"}</p>
      </div>
      <div className="rounded-lg border border-white/10 p-4">
        <h3 className="font-bold text-base-50">Cryptographic signature</h3>
        <dl className="mt-3 space-y-2 text-sm"><div><dt className="text-base-300">Asset ID</dt><dd>{assetId}</dd></div><div><dt className="text-base-300">Document hash</dt><dd className="break-all font-mono text-xs">{expectedHash || "Invalid hash"}</dd></div><div><dt className="text-base-300">Timestamp</dt><dd>{timestamp || "Created at signing"}</dd></div></dl>
        <p className={`mt-4 text-sm ${status === "verified" || status === "recorded" ? "text-emerald-400" : status === "mismatch" || status === "error" ? "text-red-400" : "text-base-300"}`} role="status">
          {status === "verified" ? "Document hash verified. Ready to sign." : status === "recorded" ? "Signature verified and recorded." : status === "signed" ? "Signature created locally." : status === "mismatch" ? "Hash mismatch — signing is disabled." : status === "checking" ? "Verifying document hash…" : error || "Verify the document before signing."}
        </p>
        <button type="button" onClick={() => void sign()} disabled={status !== "verified"} className="mt-4 rounded bg-brand-500 px-4 py-2 text-sm font-semibold text-white disabled:cursor-not-allowed disabled:opacity-50">Sign with Freighter</button>
        {signatureHash && <p className="mt-4 break-all text-xs text-base-300">Signature hash: <span className="font-mono">{signatureHash}</span>{signer ? ` · ${signer}` : ""}</p>}
        {signature && <details className="mt-2 text-xs text-base-300"><summary>Signed payload</summary><code className="break-all">{signature}</code></details>}
      </div>
    </section>
  );
}

export { asMessage as canonicalDocumentSigningMessage };
