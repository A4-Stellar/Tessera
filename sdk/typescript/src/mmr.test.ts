import { describe, expect, it } from "vitest";
import vector from "./__fixtures__/mmr_vector.json";
import {
  bytesToHex,
  mmrLeafHash,
  hexToBytes,
  MmrProofResponse,
  MmrVerificationError,
  verifyCheckpointProof,
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
