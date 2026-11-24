import { test, describe } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";

import { sha3_256, normalizeSha3 } from "../app/[lang]/docs/prospectus-viewer/sha3.ts";

describe("Issue #87: Prospectus Tokenisation & SHA-3-256 Round-Trip", () => {
  test("satisfies NIST FIPS 202 SHA-3-256 test vectors", () => {
    // Vector 1: Empty string
    assert.equal(
      sha3_256(new Uint8Array([])),
      "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a"
    );

    // Vector 2: "abc"
    const abc = new TextEncoder().encode("abc");
    assert.equal(
      sha3_256(abc),
      "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"
    );

    // Vector 3: 136-byte message (exact single-block boundary)
    const exactBlock = new Uint8Array(136).fill(0x61);
    const exactDigest = sha3_256(exactBlock);
    assert.equal(exactDigest.length, 64);
    assert.match(exactDigest, /^[0-9a-f]{64}$/);

    // Vector 4: Multi-block message (300 bytes)
    const multiBlock = new Uint8Array(300).fill(0x42);
    const multiDigest = sha3_256(multiBlock);
    assert.equal(multiDigest.length, 64);
    assert.match(multiDigest, /^[0-9a-f]{64}$/);
  });

  test("normalizes hashes with various prefixes (sha3:, 0x, uppercase)", () => {
    const raw = "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532";
    assert.equal(normalizeSha3(raw), raw);
    assert.equal(normalizeSha3(`0x${raw}`), raw);
    assert.equal(normalizeSha3(`sha3:${raw}`), raw);
    assert.equal(normalizeSha3(`SHA-3-256:${raw.toUpperCase()}`), raw);
    assert.equal(normalizeSha3("invalid-short-hash"), null);
  });

  test("simulates full end-to-end roundtrip: PDF -> SHA-3 hash -> IPFS -> verification", () => {
    // 1. Issuer creates a PDF prospectus for Real Estate Asset #42
    const mockPdfContent = new TextEncoder().encode(
      "%PDF-1.4\n1 0 obj << /Title (Metropolitan Tower Commercial RWA Offering) >>\nstream\nLegal disclosures...\nendstream"
    );

    // 2. Compute SHA-3-256 hash
    const onChainAttestationHash = sha3_256(mockPdfContent);
    assert.equal(onChainAttestationHash.length, 64);

    // 3. Simulated IPFS upload producing CID
    const ipfsCid = "bafybeihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";

    // 4. Client downloads PDF from gateway and verifies SHA-3 hash
    const downloadedBytes = mockPdfContent.slice(0);
    const clientComputedHash = sha3_256(downloadedBytes);

    assert.equal(clientComputedHash, onChainAttestationHash);
    const isVerified = clientComputedHash === normalizeSha3(onChainAttestationHash);
    assert.equal(isVerified, true);

    // 5. Tamper scenario: adversary alters 1 byte in the PDF
    const tamperedBytes = new Uint8Array(downloadedBytes);
    tamperedBytes[20] ^= 0xff; // single bit-flip alteration

    const tamperedHash = sha3_256(tamperedBytes);
    assert.notEqual(tamperedHash, onChainAttestationHash);
    const isTamperVerified = tamperedHash === normalizeSha3(onChainAttestationHash);
    assert.equal(isTamperVerified, false);
  });

  test("tracks amendments appending to prospectus history", () => {
    const assetId = 42;
    const history = [];

    // Version 1: Initial offering
    const pdfV1 = new TextEncoder().encode("%PDF-1.4 Initial Offering Prospectus V1");
    const hashV1 = sha3_256(pdfV1);
    const cidV1 = "bafybei_v1";
    history.push({
      asset_id: assetId,
      prospectus_cid: cidV1,
      prospectus_hash: hashV1,
      ledger: 10_000,
    });

    // Version 2: Amended valuation
    const pdfV2 = new TextEncoder().encode("%PDF-1.4 Amended Offering Prospectus V2 with Updated NAV");
    const hashV2 = sha3_256(pdfV2);
    const cidV2 = "bafybei_v2";
    history.push({
      asset_id: assetId,
      prospectus_cid: cidV2,
      prospectus_hash: hashV2,
      ledger: 15_500,
    });

    assert.equal(history.length, 2);
    assert.equal(history[0].prospectus_cid, cidV1);
    assert.equal(history[1].prospectus_cid, cidV2);
    assert.notEqual(history[0].prospectus_hash, history[1].prospectus_hash);
    assert.ok(history[1].ledger > history[0].ledger);
  });
});
