import { describe, expect, it } from "vitest";
import vector from "./__fixtures__/mmr_vector.json";
import {
  bytesToHex,
  mmrLeafHash,
  hexToBytes,
  MmrProofResponse,
  MmrVerificationError,
  verifyCheckpointProof,
  auditMmrLeafHash,
  auditPayloadHash,
  verifyAuditMmrProof,
  AuditMmrProofResponse,
} from "./mmr";

/** The fixture is produced by `cross_language_vector` in api/src/indexer/mmr_checkpoint.rs. */
async function response(): Promise<MmrProofResponse> {
  const leaf = await mmrLeafHash(
    vector.ledger_sequence,
    hexToBytes(vector.ledger_hash),
    hexToBytes(vector.contract_state_root),
  );
  return {
    ledger_sequence: vector.ledger_sequence,
    ledger_hash: vector.ledger_hash,
    contract_state_root: vector.contract_state_root,
    leaf_hash: bytesToHex(leaf),
    leaf_index: vector.leaf_index,
    leaf_count: vector.leaf_count,
    path: [...vector.path],
    peaks: [...vector.peaks],
    root: vector.root,
    account: {
      address: "GALICE",
      state_leaf_count: vector.state_leaf_count,
      entries: [
        {
          asset_id: vector.alice.asset_id,
          address: "GALICE",
          balance: vector.alice.balance,
          leaf_index: vector.alice.leaf_index,
          path: [...vector.alice.path],
        },
      ],
    },
  };
}

describe("verifyCheckpointProof", () => {
  it("accepts the Rust-generated proof against the trusted root", async () => {
    const verified = await verifyCheckpointProof(await response(), vector.root);
    expect(verified.ledgerSequence).toBe(102);
    expect(verified.entries).toEqual([{ assetId: 1, address: "GALICE", balance: 602n }]);
  });

  it("rejects a different trusted root", async () => {
    const other = "00".repeat(32);
    await expect(verifyCheckpointProof(await response(), other)).rejects.toThrow(
      MmrVerificationError,
    );
  });

  it("rejects a forged balance", async () => {
    const r = await response();
    r.account!.entries[0].balance = "1000000";
    await expect(verifyCheckpointProof(r, vector.root)).rejects.toThrow(/state proof failed/);
  });

  it("rejects a forged state root even with a matching leaf_hash", async () => {
    const r = await response();
    r.contract_state_root = "11".repeat(32);
    r.leaf_hash = bytesToHex(
      await mmrLeafHash(r.ledger_sequence, hexToBytes(r.ledger_hash), hexToBytes(r.contract_state_root)),
    );
    await expect(verifyCheckpointProof(r, vector.root)).rejects.toThrow(/trusted root/);
  });

  it("rejects a shifted leaf index", async () => {
    const r = await response();
    r.leaf_index += 1;
    await expect(verifyCheckpointProof(r, vector.root)).rejects.toThrow(MmrVerificationError);
  });

  it("rejects a tampered path", async () => {
    const r = await response();
    r.path[0] = "ff".repeat(32);
    await expect(verifyCheckpointProof(r, vector.root)).rejects.toThrow(MmrVerificationError);
  });
});

describe("audit MMR proof format", () => {
  it("verifies the required leaf hash and rejects tampering", async () => {
    const payload = await auditPayloadHash({ action: "freeze", asset: 1 });
    const prev = new Uint8Array(32);
    const timestamp = "2026-11-21T15:53:10Z";
    const leaf = await auditMmrLeafHash(prev, timestamp, payload);

    const proof: AuditMmrProofResponse = {
      sequence: 1,
      timestamp,
      payload_hash: bytesToHex(payload),
      prev_peak_hash: bytesToHex(prev),
      leaf_index: 0,
      leaf_count: 1,
      leaf_hash: bytesToHex(leaf),
      path: [],
      peaks: [bytesToHex(leaf)],
      root: bytesToHex(leaf),
    };
    await expect(verifyAuditMmrProof(proof)).resolves.toBe(true);

    proof.timestamp = "2026-11-21T15:53:11Z";
    await expect(verifyAuditMmrProof(proof)).resolves.toBe(false);
  });

  it("verifies every mountain size in a property-style leaf-count sweep", async () => {
    for (let leafCount = 1; leafCount <= 64; leafCount++) {
      let prevPeakHash = new Uint8Array(32);
      const leaves: Uint8Array[] = [];
      const previousPeaks: Uint8Array[] = [];
      const levels: Uint8Array[][] = [];

      for (let i = 0; i < leafCount; i++) {
        const payload = await auditPayloadHash({ i });
        const timestamp = `2026-01-01T00:00:${String(i).padStart(2, "0")}Z`;
        previousPeaks.push(prevPeakHash);
        const leaf = await auditMmrLeafHash(prevPeakHash, timestamp, payload);
        leaves.push(leaf);
        if (!levels[0]) levels[0] = [];
        levels[0].push(leaf);
        let h = 0;
        while (levels[h].length % 2 === 0) {
          const len = levels[h].length;
          const parent = await sha256ForAudit(levels[h][len - 2], levels[h][len - 1]);
          if (!levels[h + 1]) levels[h + 1] = [];
          levels[h + 1].push(parent);
          h++;
        }
        const currentMountains: [number, number][] = [];
        let currentStart = 0;
        for (let h = 52; h >= 0; h--) {
          const size = 2 ** h;
          if (Math.floor((i + 1) / size) % 2 === 1) {
            currentMountains.push([currentStart, h]);
            currentStart += size;
          }
        }
        const currentPeaks = currentMountains.map(([start, h]) => levels[h][start / 2 ** h]);
        let currentRoot = currentPeaks[currentPeaks.length - 1];
        for (let peak = currentPeaks.length - 2; peak >= 0; peak--) {
          currentRoot = await sha256ForAudit(currentPeaks[peak], currentRoot);
        }
        prevPeakHash = currentRoot;
      }

      const mountains: [number, number][] = [];
      let start = 0;
      for (let h = 52; h >= 0; h--) {
        const size = 2 ** h;
        if (Math.floor(leafCount / size) % 2 === 1) {
          mountains.push([start, h]);
          start += size;
        }
      }
      const peaks = mountains.map(([start, h]) => levels[h][start / 2 ** h]);
      let bagged = peaks[peaks.length - 1];
      for (let i = peaks.length - 2; i >= 0; i--) {
        bagged = await sha256ForAudit(peaks[i], bagged);
      }

      for (let leafIndex = 0; leafIndex < leafCount; leafIndex++) {
        const pos = mountains.findIndex(([start, h]) =>
          leafIndex >= start && leafIndex < start + 2 ** h);
        const [start, height] = mountains[pos];
        const path: Uint8Array[] = [];
        let idx = leafIndex;
        for (let h = 0; h < height; h++) {
          path.push(levels[h][idx ^ 1]);
          idx >>= 1;
        }
        const payload = await auditPayloadHash({ i: leafIndex });
        const proof: AuditMmrProofResponse = {
          sequence: leafIndex + 1,
          timestamp: `2026-01-01T00:00:${String(leafIndex).padStart(2, "0")}Z`,
          payload_hash: bytesToHex(payload),
          prev_peak_hash: bytesToHex(previousPeaks[leafIndex]),
          leaf_index: leafIndex,
          leaf_count: leafCount,
          leaf_hash: bytesToHex(leaves[leafIndex]),
          path: path.map(bytesToHex),
          peaks: peaks.map(bytesToHex),
          root: bytesToHex(bagged),
        };
        await expect(verifyAuditMmrProof(proof)).resolves.toBe(true);
      }
    }
  });
});

async function sha256ForAudit(left: Uint8Array, right: Uint8Array): Promise<Uint8Array> {
  const out = new Uint8Array(left.length + right.length);
  out.set(left);
  out.set(right, left.length);
  return new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", out.buffer as ArrayBuffer));
}
