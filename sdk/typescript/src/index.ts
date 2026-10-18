export * from "./mmr";

export class TesseraClient {
    constructor(private baseUrl: string) {}

    async getAssets() {
        const res = await fetch(`${this.baseUrl}/v1/assets`);
        return res.json();
    }

    /** Current MMR root and leaf count (issue #318). */
    async getMmrRoot(): Promise<{ leaf_count: number; root: string; latest_ledger_sequence: number | null }> {
        const res = await fetch(`${this.baseUrl}/v1/proofs/mmr`);
        return res.json();
    }

    /** Checkpoint proof for `ledgerSeq`; verify it with `verifyCheckpointProof`. */
    async getMmrProof(ledgerSeq: number, account?: string): Promise<import("./mmr").MmrProofResponse> {
        const query = account ? `?account=${encodeURIComponent(account)}` : "";
        const res = await fetch(`${this.baseUrl}/v1/proofs/mmr/${ledgerSeq}${query}`);
        if (!res.ok) throw new Error(`proof request failed: ${res.status}`);
        return res.json();
    }
}
