/**
 * Client-side verification of Tessera MMR checkpoint proofs (issue #318).
 *
 * The API appends one Merkle Mountain Range leaf per indexed ledger,
 * committing to `(ledger_sequence, ledger_hash, contract_state_root)`.
 * `GET /v1/proofs/mmr/:ledger_seq?account=:address` returns that leaf's
 * audit path plus, for `account`, each cap-table entry's path to the state
 * root. This module recomputes both from the raw values, so a light client
 * only has to trust the MMR root it already holds — not the API response.
 *
 * Hashing must match `api/src/indexer/mmr_checkpoint.rs` exactly; the shared
 * fixture `__fixtures__/mmr_vector.json` pins both implementations.
 *
 * Uses WebCrypto (`globalThis.crypto.subtle`): browsers, Deno, Node >= 18.
 */

export type Hex = string;

/** Body of `GET /v1/proofs/mmr/:ledger_seq`. */
export interface MmrProofResponse {
  ledger_sequence: number;
  ledger_hash: Hex;
  contract_state_root: Hex;
  leaf_hash: Hex;
  leaf_index: number;
  leaf_count: number;
  path: Hex[];
  peaks: Hex[];
  root: Hex;
  account?: AccountProof;
}

export interface AccountProof {
  address: string;
  state_leaf_count: number;
  entries: StateEntryProof[];
}

export interface StateEntryProof {
  asset_id: number;
  address: string;
  /** Raw i128 balance as a decimal string. */
  balance: string;
  leaf_index: number;
  path: Hex[];
}

export interface VerifiedCheckpoint {
  ledgerSequence: number;
  ledgerHash: Hex;
  contractStateRoot: Hex;
  /** Cap-table entries proven against `contractStateRoot` (empty if no `account`). */
  entries: { assetId: number; address: string; balance: bigint }[];
}

const MMR_LEAF = 0x00;
const MMR_NODE = 0x01;
const MMR_BAG = 0x02;
const MMR_ROOT = 0x03;
const STATE_LEAF = 0x10;
const STATE_NODE = 0x11;

export class MmrVerificationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MmrVerificationError";
  }
}

export function hexToBytes(hex: Hex): Uint8Array {
  if (!/^([0-9a-fA-F]{2})*$/.test(hex)) {
    throw new MmrVerificationError(`invalid hex: ${hex}`);
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export function bytesToHex(bytes: Uint8Array): Hex {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function hash32(hex: Hex): Uint8Array {
  const b = hexToBytes(hex);
  if (b.length !== 32) throw new MmrVerificationError(`expected 32-byte hash, got ${b.length}`);
  return b;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

async function sha256(...parts: Uint8Array[]): Promise<Uint8Array> {
  // `concat` always allocates a fresh, exactly-sized ArrayBuffer.
  const digest = await globalThis.crypto.subtle.digest("SHA-256", concat(...parts).buffer as ArrayBuffer);
  return new Uint8Array(digest);
}

function u32be(n: number): Uint8Array {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n);
  return b;
}

function u64be(n: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n);
  return b;
}

function i128be(n: bigint): Uint8Array {
  const min = -(1n << 127n);
  const max = (1n << 127n) - 1n;
  if (n < min || n > max) throw new MmrVerificationError("balance out of i128 range");
  let v = BigInt.asUintN(128, n);
  const b = new Uint8Array(16);
  for (let i = 15; i >= 0; i--) {
    b[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return b;
}

const node = (prefix: number, l: Uint8Array, r: Uint8Array) =>
  sha256(Uint8Array.of(prefix), l, r);

export function mmrLeafHash(
  ledgerSequence: number,
  ledgerHash: Uint8Array,
  stateRoot: Uint8Array,
): Promise<Uint8Array> {
  return sha256(Uint8Array.of(MMR_LEAF), u32be(ledgerSequence), ledgerHash, stateRoot);
}

export function stateLeafHash(assetId: number, address: string, balance: bigint): Promise<Uint8Array> {
  const addr = new TextEncoder().encode(address);
  return sha256(
    Uint8Array.of(STATE_LEAF),
    u64be(BigInt(assetId)),
    u32be(addr.length),
    addr,
    i128be(balance),
  );
}

/** Mountains of an MMR with `leafCount` leaves, largest first: [start, height]. */
function mountains(leafCount: number): [number, number][] {
  const out: [number, number][] = [];
  let start = 0;
  for (let h = 52; h >= 0; h--) {
    const size = 2 ** h;
    if (Math.floor(leafCount / size) % 2 === 1) {
      out.push([start, h]);
      start += size;
    }
  }
  return out;
}

export async function bagPeaks(peaks: Uint8Array[], leafCount: number): Promise<Uint8Array> {
  let bagged: Uint8Array = new Uint8Array(32);
  if (peaks.length > 0) {
    bagged = peaks[peaks.length - 1];
    for (let i = peaks.length - 2; i >= 0; i--) {
      bagged = await node(MMR_BAG, peaks[i], bagged);
    }
  }
  return sha256(Uint8Array.of(MMR_ROOT), u64be(BigInt(leafCount)), bagged);
}

function equal(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Recompute the MMR root from `leaf` and its proof; true iff it equals `root`. */
export async function verifyMmrProof(
  leaf: Uint8Array,
  leafIndex: number,
  leafCount: number,
  path: Uint8Array[],
  peaks: Uint8Array[],
  root: Uint8Array,
): Promise<boolean> {
  if (!Number.isSafeInteger(leafIndex) || !Number.isSafeInteger(leafCount)) return false;
  if (leafIndex < 0 || leafIndex >= leafCount) return false;
  const all = mountains(leafCount);
  if (all.length !== peaks.length) return false;
  const pos = all.findIndex(([start, h]) => leafIndex < start + 2 ** h);
  if (pos < 0) return false;
  const [start, height] = all[pos];
  if (path.length !== height) return false;

  let acc = leaf;
  let idx = leafIndex - start;
  for (const sibling of path) {
    acc = idx % 2 === 0 ? await node(MMR_NODE, acc, sibling) : await node(MMR_NODE, sibling, acc);
    idx = Math.floor(idx / 2);
  }
  if (!equal(acc, peaks[pos])) return false;
  return equal(await bagPeaks(peaks, leafCount), root);
}

/** Recompute a cap-table state root from one entry's proof. */
export async function verifyStateProof(
  leaf: Uint8Array,
  leafIndex: number,
  leafCount: number,
  path: Uint8Array[],
  root: Uint8Array,
): Promise<boolean> {
  if (leafIndex < 0 || leafIndex >= leafCount) return false;
  let acc = leaf;
  let idx = leafIndex;
  let size = leafCount;
  let next = 0;
  while (size > 1) {
    const siblingIdx = idx ^ 1;
    if (siblingIdx < size) {
      const sibling = path[next++];
      if (!sibling) return false;
      acc = idx % 2 === 0 ? await node(STATE_NODE, acc, sibling) : await node(STATE_NODE, sibling, acc);
    }
    idx = Math.floor(idx / 2);
    size = Math.ceil(size / 2);
  }
  return next === path.length && equal(acc, root);
}

/**
 * Verify a full `/v1/proofs/mmr/:ledger_seq` response against a root the
 * client already trusts. Every hash in the response is recomputed from raw
 * values; `leaf_hash` and `root` fields in the body are ignored except as a
 * cross-check. Throws `MmrVerificationError` on any mismatch.
 */
export async function verifyCheckpointProof(
  response: MmrProofResponse,
  trustedRoot: Hex,
): Promise<VerifiedCheckpoint> {
  const ledgerHash = hash32(response.ledger_hash);
  const stateRoot = hash32(response.contract_state_root);
  const leaf = await mmrLeafHash(response.ledger_sequence, ledgerHash, stateRoot);
  if (bytesToHex(leaf) !== response.leaf_hash.toLowerCase()) {
    throw new MmrVerificationError("leaf_hash does not match the committed values");
  }
  const ok = await verifyMmrProof(
    leaf,
    response.leaf_index,
    response.leaf_count,
    response.path.map(hash32),
    response.peaks.map(hash32),
    hash32(trustedRoot),
  );
  if (!ok) throw new MmrVerificationError("MMR path does not lead to the trusted root");

  const entries: VerifiedCheckpoint["entries"] = [];
  if (response.account) {
    for (const e of response.account.entries) {
      if (e.address !== response.account.address) {
        throw new MmrVerificationError("entry belongs to a different account");
      }
      const balance = BigInt(e.balance);
      const stateLeaf = await stateLeafHash(e.asset_id, e.address, balance);
      const valid = await verifyStateProof(
        stateLeaf,
        e.leaf_index,
        response.account.state_leaf_count,
        e.path.map(hash32),
        stateRoot,
      );
      if (!valid) {
        throw new MmrVerificationError(`state proof failed for asset ${e.asset_id}`);
      }
      entries.push({ assetId: e.asset_id, address: e.address, balance });
    }
  }

  return {
    ledgerSequence: response.ledger_sequence,
    ledgerHash: response.ledger_hash.toLowerCase(),
    contractStateRoot: response.contract_state_root.toLowerCase(),
    entries,
  };
}
