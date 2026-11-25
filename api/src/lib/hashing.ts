/**
 * SHA-256 and Keccak-256 batch hashing (Issue #505).
 *
 * Backends:
 * - "js":        synchronous pure-JS hashing (@noble/hashes). Fastest for many small
 *                items, works everywhere, and is the only option for Keccak-256 because
 *                Web Crypto has no Keccak.
 * - "webcrypto": native `crypto.subtle.digest` (SHA-256 only). Hardware accelerated, but
 *                every call is async, so 10,000 tiny digests are much slower than the
 *                synchronous path. It only pays off for large payloads.
 * - "auto":      "webcrypto" for SHA-256 when available and items are large, else "js".
 */

import { sha256 } from "@noble/hashes/sha2.js";
import { keccak_256 } from "@noble/hashes/sha3.js";
import { bytesToHex } from "@noble/hashes/utils.js";

export type HashAlgorithm = "sha256" | "keccak256";
export type HashBackend = "auto" | "webcrypto" | "js";
export type ResolvedBackend = "webcrypto" | "js";

/** Benchmark target: this many hashes must finish within the budget. */
export const BENCHMARK_ITEM_COUNT = 10_000;
export const BENCHMARK_BUDGET_MS = 250;

/** Average item size from which Web Crypto beats the synchronous JS path. */
export const WEBCRYPTO_MIN_ITEM_BYTES = 16 * 1024;

export function hasWebCrypto(): boolean {
  return typeof globalThis.crypto?.subtle?.digest === "function";
}

/** Picks the backend that will actually run for this algorithm and workload. */
export function resolveBackend(
  algorithm: HashAlgorithm,
  items: readonly Uint8Array[],
  requested: HashBackend = "auto",
): ResolvedBackend {
  if (algorithm !== "sha256" || requested === "js" || !hasWebCrypto()) return "js";
  if (requested === "webcrypto") return "webcrypto";
  if (items.length === 0) return "js";
  const totalBytes = items.reduce((sum, item) => sum + item.byteLength, 0);
  return totalBytes / items.length >= WEBCRYPTO_MIN_ITEM_BYTES ? "webcrypto" : "js";
}

/** Synchronous pure-JS hashing of every item, returning lowercase hex digests. */
export function hashBatchSync(items: readonly Uint8Array[], algorithm: HashAlgorithm): string[] {
  const fn = algorithm === "sha256" ? sha256 : keccak_256;
  const out = new Array<string>(items.length);
  for (let i = 0; i < items.length; i++) out[i] = bytesToHex(fn(items[i]));
  return out;
}

async function hashBatchWebCrypto(items: readonly Uint8Array[]): Promise<string[]> {
  const digests = await Promise.all(
    items.map((item) => globalThis.crypto.subtle.digest("SHA-256", item as BufferSource)),
  );
  return digests.map((d) => bytesToHex(new Uint8Array(d)));
}

/** Hashes every item with the best available backend. */
export async function hashBatch(
  items: readonly Uint8Array[],
  algorithm: HashAlgorithm,
  backend: HashBackend = "auto",
): Promise<string[]> {
  if (resolveBackend(algorithm, items, backend) === "webcrypto") {
    try {
      return await hashBatchWebCrypto(items);
    } catch {
      // Native digest failed (e.g. insecure context); use the JS implementation.
    }
  }
  return hashBatchSync(items, algorithm);
}
