"use client";

import React, { useState, useEffect, useRef } from "react";

interface IdentityProvider {
  role: string;
  auth_method: "email" | "phone_number" | "totp";
  identifier?: string;
}

interface AccountRecoveryModalProps {
  isOpen: boolean;
  onClose: () => void;
  apiBaseUrl?: string;
}

export function AccountRecoveryModal({
  isOpen,
  onClose,
  apiBaseUrl = "/v1",
}: AccountRecoveryModalProps) {
  const [step, setStep] = useState<1 | 2 | 3 | 4>(1);
  const [accountAddress, setAccountAddress] = useState("");
  const [newSignerKey, setNewSignerKey] = useState("");
  const [identities, setIdentities] = useState<IdentityProvider[]>([]);
  const [threshold, setThreshold] = useState<number>(2);

  // Verification proofs
  const [emailProof, setEmailProof] = useState("");
  const [phoneProof, setPhoneProof] = useState("");
  const [totpProof, setTotpProof] = useState("");

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [coSignedXdr, setCoSignedXdr] = useState<string | null>(null);
  const [recoverySigner, setRecoverySigner] = useState<string | null>(null);

  const modalRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) {
      setStep(1);
      setError(null);
      setCoSignedXdr(null);
    }
  }, [isOpen]);

  // Trap focus and close on Escape
  useEffect(() => {
    if (!isOpen) return;

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        onClose();
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, onClose]);

  // Step 1: Query registered signers and identities for address
  async function handleLookupAccount(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);

    try {
      const res = await fetch(`${apiBaseUrl}/sep30/accounts/${accountAddress.trim()}`);
      if (!res.ok) {
        if (res.status === 404) {
          throw new Error("Stellar account is not registered with this SEP-30 recovery server.");
        }
        const data = await res.json().catch(() => ({}));
        throw new Error(data.message || `Lookup failed with HTTP ${res.status}`);
      }

      const data = await res.json();
      setIdentities(data.identities || []);
      setThreshold(data.threshold || 2);
      setStep(2);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Failed to lookup account");
    } finally {
      setLoading(false);
    }
  }

  // Step 2 & 3: Submit recovery request with verified identity proofs
  async function handleExecuteRecovery(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);

    const proofs = [];
    if (emailProof.trim()) {
      proofs.push({ auth_method: "email", proof: emailProof.trim() });
    }
    if (phoneProof.trim()) {
      proofs.push({ auth_method: "phone_number", proof: phoneProof.trim() });
    }
    if (totpProof.trim()) {
      proofs.push({ auth_method: "totp", proof: totpProof.trim() });
    }

    if (proofs.length < threshold) {
      setError(`At least ${threshold} verified identity proofs are required to meet recovery threshold.`);
      setLoading(false);
      return;
    }

    try {
      const res = await fetch(`${apiBaseUrl}/sep30/accounts/${accountAddress.trim()}/recover`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          new_signer_key: newSignerKey.trim(),
          identity_proofs: proofs,
        }),
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        if (res.status === 429) {
          throw new Error("Rate limit exceeded: maximum 3 recovery attempts per 24 hours per account.");
        }
        if (res.status === 403) {
          throw new Error("Security Violation: Recovery signer rejected operation. Only SetOptions recovery transactions are allowed.");
        }
        throw new Error(data.message || `Recovery failed with HTTP ${res.status}`);
      }

      const data = await res.json();
      setCoSignedXdr(data.transaction_xdr);
      setRecoverySigner(data.recovery_signer);
      setStep(4);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Recovery execution failed");
    } finally {
      setLoading(false);
    }
  }

  if (!isOpen) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 overflow-y-auto"
      role="dialog"
      aria-modal="true"
      aria-labelledby="sep30-modal-title"
    >
      <div
        ref={modalRef}
        className="w-full max-w-xl rounded-2xl bg-zinc-900 border border-zinc-800 p-6 shadow-2xl text-zinc-100"
      >
        <div className="flex items-center justify-between pb-4 border-b border-zinc-800">
          <div>
            <h2 id="sep30-modal-title" className="text-xl font-bold tracking-tight text-white flex items-center gap-2">
              <span className="inline-block w-3 h-3 rounded-full bg-emerald-500 animate-pulse" />
              Stellar Account Social Recovery (SEP-30)
            </h2>
            <p className="text-xs text-zinc-400 mt-1">
              Multi-party recovery server · Threshold multisig co-signer
            </p>
          </div>
          <button
            onClick={onClose}
            className="rounded-lg p-2 text-zinc-400 hover:text-white hover:bg-zinc-800 transition"
            aria-label="Close dialog"
          >
            ✕
          </button>
        </div>

        {/* Security Policy Badge */}
        <div className="my-4 p-3 rounded-xl bg-zinc-800/60 border border-zinc-700/50 flex items-start gap-3">
          <div className="text-lg">🛡️</div>
          <div className="text-xs text-zinc-300">
            <span className="font-semibold text-white">Zero-Trust Guardrails:</span> Rate limited to max 3 attempts per 24 hours. The recovery server holds a key that only co-signs <code className="text-emerald-400">SetOptions</code> key rotations and strictly rejects any asset transfers.
          </div>
        </div>

        {error && (
          <div className="mb-4 p-3 rounded-xl bg-rose-950/50 border border-rose-800 text-rose-200 text-xs">
            {error}
          </div>
        )}

        {/* Step 1: Account Lookup */}
        {step === 1 && (
          <form onSubmit={handleLookupAccount} className="space-y-4">
            <div>
              <label htmlFor="stellar-addr" className="block text-xs font-medium text-zinc-300 mb-1">
                Stellar Account Address (Public Key)
              </label>
              <input
                id="stellar-addr"
                type="text"
                placeholder="G..."
                value={accountAddress}
                onChange={(e) => setAccountAddress(e.target.value)}
                required
                className="w-full px-3 py-2.5 rounded-xl bg-zinc-800 border border-zinc-700 text-white font-mono text-xs focus:outline-none focus:ring-2 focus:ring-emerald-500"
              />
            </div>
            <div className="flex justify-end gap-3 pt-2">
              <button
                type="button"
                onClick={onClose}
                className="px-4 py-2 rounded-xl text-xs font-medium text-zinc-300 hover:bg-zinc-800 transition"
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={loading || !accountAddress.trim()}
                className="px-5 py-2 rounded-xl text-xs font-semibold bg-emerald-600 hover:bg-emerald-500 text-white transition disabled:opacity-50"
              >
                {loading ? "Checking..." : "Next: Verify Identities →"}
              </button>
            </div>
          </form>
        )}

        {/* Step 2 & 3: Identity Verification & New Key Entry */}
        {(step === 2 || step === 3) && (
          <form onSubmit={handleExecuteRecovery} className="space-y-4">
            <div className="p-3 rounded-xl bg-zinc-800/40 border border-zinc-800 text-xs space-y-1">
              <div className="text-zinc-400">Account: <span className="font-mono text-zinc-200">{accountAddress}</span></div>
              <div className="text-zinc-400">
                Required Social Proofs: <span className="font-semibold text-emerald-400">{threshold} of {identities.length}</span>
              </div>
            </div>

            <div className="space-y-3">
              <div className="text-xs font-semibold text-zinc-200">Enter Verification Proofs:</div>

              {identities.some((id) => id.auth_method === "email") && (
                <div>
                  <label htmlFor="proof-email" className="block text-xs text-zinc-400 mb-1">Email Confirmation Code</label>
                  <input
                    id="proof-email"
                    type="text"
                    placeholder="Enter email OTP code"
                    value={emailProof}
                    onChange={(e) => setEmailProof(e.target.value)}
                    className="w-full px-3 py-2 rounded-xl bg-zinc-800 border border-zinc-700 text-white text-xs"
                  />
                </div>
              )}

              {identities.some((id) => id.auth_method === "phone_number") && (
                <div>
                  <label htmlFor="proof-phone" className="block text-xs text-zinc-400 mb-1">Phone SMS Verification Code</label>
                  <input
                    id="proof-phone"
                    type="text"
                    placeholder="Enter SMS OTP code"
                    value={phoneProof}
                    onChange={(e) => setPhoneProof(e.target.value)}
                    className="w-full px-3 py-2 rounded-xl bg-zinc-800 border border-zinc-700 text-white text-xs"
                  />
                </div>
              )}

              {identities.some((id) => id.auth_method === "totp") && (
                <div>
                  <label htmlFor="proof-totp" className="block text-xs text-zinc-400 mb-1">TOTP 6-Digit Authenticator Code</label>
                  <input
                    id="proof-totp"
                    type="text"
                    placeholder="e.g. 543210"
                    value={totpProof}
                    onChange={(e) => setTotpProof(e.target.value)}
                    className="w-full px-3 py-2 rounded-xl bg-zinc-800 border border-zinc-700 text-white text-xs"
                  />
                </div>
              )}

              <div className="pt-2 border-t border-zinc-800">
                <label htmlFor="new-signer" className="block text-xs font-medium text-zinc-300 mb-1">
                  New Signer Public Key (New Device Key)
                </label>
                <input
                  id="new-signer"
                  type="text"
                  placeholder="G..."
                  value={newSignerKey}
                  onChange={(e) => setNewSignerKey(e.target.value)}
                  required
                  className="w-full px-3 py-2.5 rounded-xl bg-zinc-800 border border-zinc-700 text-white font-mono text-xs focus:ring-2 focus:ring-emerald-500"
                />
              </div>
            </div>

            <div className="flex justify-between items-center pt-2">
              <button
                type="button"
                onClick={() => setStep(1)}
                className="text-xs text-zinc-400 hover:text-white"
              >
                ← Back
              </button>
              <button
                type="submit"
                disabled={loading || !newSignerKey.trim()}
                className="px-5 py-2 rounded-xl text-xs font-semibold bg-emerald-600 hover:bg-emerald-500 text-white transition disabled:opacity-50"
              >
                {loading ? "Verifying & Co-signing..." : "Co-Sign Recovery Transaction →"}
              </button>
            </div>
          </form>
        )}

        {/* Step 4: Recovery Success & Signed XDR */}
        {step === 4 && (
          <div className="space-y-4">
            <div className="p-4 rounded-xl bg-emerald-950/40 border border-emerald-800 text-emerald-200 text-xs flex items-start gap-3">
              <div className="text-xl">✅</div>
              <div>
                <div className="font-semibold text-white">Recovery Transaction Successfully Co-Signed!</div>
                <div className="mt-1 text-emerald-300">
                  The SEP-30 recovery server has verified your threshold proofs and appended its threshold Ed25519 signature.
                </div>
              </div>
            </div>

            <div>
              <label htmlFor="signed-xdr-output" className="block text-xs text-zinc-400 mb-1">Signed Transaction Envelope XDR:</label>
              <textarea
                id="signed-xdr-output"
                readOnly
                rows={5}
                value={coSignedXdr || ""}
                className="w-full p-2.5 rounded-xl bg-zinc-950 border border-zinc-800 text-emerald-400 font-mono text-[11px] break-all focus:outline-none"
              />
            </div>

            {recoverySigner && (
              <div className="text-[11px] text-zinc-400">
                Co-signed by recovery signer: <code className="text-zinc-200">{recoverySigner}</code>
              </div>
            )}

            <div className="flex justify-end gap-3 pt-2">
              <button
                onClick={() => {
                  navigator.clipboard.writeText(coSignedXdr || "");
                }}
                className="px-4 py-2 rounded-xl text-xs font-medium bg-zinc-800 hover:bg-zinc-700 text-white transition"
              >
                Copy XDR
              </button>
              <button
                onClick={onClose}
                className="px-5 py-2 rounded-xl text-xs font-semibold bg-emerald-600 hover:bg-emerald-500 text-white transition"
              >
                Done
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
