-- EigenDA data-availability commitments (issue #469).
--
-- One row per (asset, indexed ledger) whose state snapshot was dispersed.
-- The aggregate BLS signature and the operator stake snapshot are kept as a
-- JSON proof blob so the Soroban verifier can re-check the stake-weighted
-- quorum without trusting the indexer.

CREATE TABLE IF NOT EXISTS eigenda_da_proofs (
    asset_id           BIGINT       NOT NULL,
    ledger_sequence    BIGINT       NOT NULL,
    batch_header_hash  TEXT         NOT NULL,
    blob_index         BIGINT       NOT NULL,
    blob_hash          TEXT         NOT NULL,
    mmr_root           TEXT         NOT NULL,
    quorum_threshold   INTEGER      NOT NULL CHECK (quorum_threshold BETWEEN 1 AND 100),
    status             TEXT         NOT NULL,
    proof              JSONB        NOT NULL DEFAULT '{}'::jsonb,
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    confirmed_at       TIMESTAMPTZ,
    PRIMARY KEY (asset_id, ledger_sequence)
);

CREATE INDEX IF NOT EXISTS eigenda_da_proofs_header_idx
    ON eigenda_da_proofs (batch_header_hash, blob_index);

COMMENT ON COLUMN eigenda_da_proofs.batch_header_hash IS
    '32-byte EigenDA batch header hash, hex encoded (0x-prefixed).';
COMMENT ON COLUMN eigenda_da_proofs.proof IS
    'EigenDA retrieval proof: aggregate BLS signature, signer bitmap and stake weights.';
