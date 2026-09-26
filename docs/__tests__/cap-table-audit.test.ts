import {
  AuditReport,
  buildAuditPdf,
  buildAuditRows,
  calculateAuditSummary,
  parseCapTableText,
  parseCsvCapTable,
  parseJsonCapTable,
  serializeAuditProofPayload,
} from "@/lib/cap-table-audit";

describe("cap-table audit helpers", () => {
  it("parses CSV aliases, quoted names, and KYC values", () => {
    const records = parseCsvCapTable(
      [
        "wallet,shares,kyc_status,shareholder_name",
        'GAAA,100.0000000,verified,"Ada, Holdings"',
        "GBBB,25.5,pending,Grace Ltd",
      ].join("\n"),
    );

    expect(records).toEqual([
      {
        address: "GAAA",
        balance: "100",
        kycVerified: true,
        holderName: "Ada, Holdings",
      },
      {
        address: "GBBB",
        balance: "25.5",
        kycVerified: false,
        holderName: "Grace Ltd",
      },
    ]);
  });

  it("parses JSON wrappers and common field aliases", () => {
    const records = parseJsonCapTable(
      JSON.stringify({
        capTable: [
          { stellar_address: "GAAA", token_balance: "42.00", verified: true },
          { account_id: "GBBB", quantity: 8, kyc: "approved" },
        ],
      }),
    );

    expect(records.map((record) => [record.address, record.balance, record.kycVerified])).toEqual([
      ["GAAA", "42", true],
      ["GBBB", "8", true],
    ]);
  });

  it("auto-detects JSON content", () => {
    const records = parseCapTableText('[{"address":"GAAA","balance":"5"}]', "upload.txt");
    expect(records[0]).toMatchObject({ address: "GAAA", balance: "5", kycVerified: null });
  });

  it("rejects duplicate holder addresses", () => {
    expect(() => parseCsvCapTable("address,balance\nGAAA,10\ngaaa,20")).toThrow(
      "Duplicate holder address",
    );
  });

  it("flags all discrepancy classes", () => {
    const legal = parseJsonCapTable(
      JSON.stringify([
        { address: "GAAA", balance: "100", kyc_verified: true },
        { address: "GBBB", balance: "50", kyc_verified: false },
        { address: "GCCC", balance: "25", kyc_verified: true },
      ]),
    );

    const rows = buildAuditRows(legal, [
      { address: "GAAA", balance: "100" },
      { address: "GBBB", balance: "40" },
      { address: "GDDD", balance: "10" },
    ]);

    expect(rows.find((row) => row.address === "GAAA")?.issues).toEqual([]);
    expect(rows.find((row) => row.address === "GBBB")?.issues).toEqual([
      "balance_mismatch",
      "missing_kyc",
    ]);
    expect(rows.find((row) => row.address === "GCCC")?.issues).toEqual(["missing_on_chain"]);
    expect(rows.find((row) => row.address === "GDDD")?.issues).toEqual([
      "unexpected_on_chain",
      "missing_kyc",
    ]);
    expect(rows.find((row) => row.address === "GBBB")?.delta).toBe("-10");
  });

  it("calculates exact decimal totals and unallocated shares", () => {
    const legal = parseJsonCapTable(
      JSON.stringify([
        { address: "GAAA", balance: "0.1", kyc_verified: true },
        { address: "GBBB", balance: "0.2", kyc_verified: true },
      ]),
    );
    const rows = buildAuditRows(legal, [
      { address: "GAAA", balance: "0.1" },
      { address: "GBBB", balance: "0.15" },
    ]);
    const summary = calculateAuditSummary(rows, "0.5");

    expect(summary.legalTotalShares).toBe("0.3");
    expect(summary.onChainTotalShares).toBe("0.25");
    expect(summary.unallocatedShares).toBe("0.25");
    expect(summary.excessOnChainShares).toBe("0");
    expect(summary.balanceMismatchCount).toBe(1);
  });

  it("serializes a deterministic timestamp-proof payload", () => {
    const rows = buildAuditRows(
      [{ address: "GAAA", balance: "10", kycVerified: true }],
      [{ address: "GAAA", balance: "10" }],
    );
    const summary = calculateAuditSummary(rows);
    const payload = serializeAuditProofPayload(
      "7",
      "2026-09-26T12:00:00.000Z",
      summary,
      rows,
    );

    expect(payload).toContain("tessera-cap-table-audit-v1");
    expect(payload).toContain("2026-09-26T12:00:00.000Z");
    expect(payload).toContain('"assetId":"7"');
  });

  it("generates a PDF embedding the timestamp proof and findings", () => {
    const rows = buildAuditRows(
      [{ address: "GAAA", balance: "10", kycVerified: true }],
      [{ address: "GAAA", balance: "9" }],
    );
    const summary = calculateAuditSummary(rows);
    const report: AuditReport = {
      assetId: "7",
      sourceFile: "legal.csv",
      generatedAt: "2026-09-26T12:00:00.000Z",
      timestampProof: "abc123",
      summary,
      rows,
    };

    const pdf = new TextDecoder().decode(buildAuditPdf(report));
    expect(pdf.startsWith("%PDF-1.4")).toBe(true);
    expect(pdf).toContain("Timestamp proof \\(SHA-256\\): abc123");
    expect(pdf).toContain("Balance mismatch");
    expect(pdf).toContain("startxref");
    expect(pdf.endsWith("%%EOF")).toBe(true);
  });
});
