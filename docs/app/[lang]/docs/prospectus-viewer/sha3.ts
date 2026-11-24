/**
 * Client-side NIST FIPS 202 SHA-3-256 implementation (Issue #87).
 * Uses platform cryptographic primitives with standard SHA-3-256 digest.
 */

import crypto from "crypto";

/**
 * Computes the 256-bit SHA-3 digest of input data according to NIST FIPS 202.
 *
 * @param data ArrayBuffer or Uint8Array of bytes to hash.
 * @returns Lowercase 64-character hexadecimal digest.
 */
export function sha3_256(data: Uint8Array | ArrayBuffer): string {
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);

  // In Node.js / Next.js SSR / Jest test environments
  if (typeof crypto !== "undefined" && typeof crypto.createHash === "function") {
    return crypto.createHash("sha3-256").update(Buffer.from(bytes)).digest("hex");
  }

  throw new Error("SHA-3-256 cryptographic provider is unavailable in this environment.");
}

/**
 * Normalizes SHA-3-256 hex string by stripping sha3:, sha-3-256:, or 0x prefixes.
 */
export function normalizeSha3(value: string): string | null {
  const normalized = value.trim().replace(/^sha-?3(?:-256)?:/i, "").replace(/^0x/i, "").toLowerCase();
  return /^[0-9a-f]{64}$/.test(normalized) ? normalized : null;
}
