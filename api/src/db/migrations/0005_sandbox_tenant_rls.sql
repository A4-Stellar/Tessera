-- Regulatory sandbox: tenant-isolated synthetic data (issue #468).
--
-- Every row carries a tenant_id and every table has FORCE ROW LEVEL SECURITY,
-- so even the table owner is restricted. A session can only see rows for the
-- tenant selected with:
--
--     SET LOCAL app.sandbox_tenant_id = '<tenant>';
--
-- `current_setting(..., true)` returns NULL/empty when unset, so an
-- unconfigured connection sees *zero* rows — fail closed, never fail open.
--
-- OPERATIONAL REQUIREMENT: the API's Postgres role must NOT have BYPASSRLS or
-- SUPERUSER. The seeder uses a separate role with BYPASSRLS only during the
-- one-off data load (see tests/e2e/docker-compose.sandbox.yml).

CREATE TABLE IF NOT EXISTS sandbox_assets (
    asset_id       BIGINT       NOT NULL,
    tenant_id      TEXT         NOT NULL,
    name           TEXT         NOT NULL,
    issuer         TEXT         NOT NULL,
    asset_type     TEXT         NOT NULL,
    jurisdiction   TEXT         NOT NULL,
    price_cents    BIGINT       NOT NULL CHECK (price_cents > 0),
    seeded_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, asset_id)
);

CREATE TABLE IF NOT EXISTS sandbox_holders (
    holder_id      BIGINT       NOT NULL,
    tenant_id      TEXT         NOT NULL,
    asset_id       BIGINT       NOT NULL,
    address        TEXT         NOT NULL,
    balance        BIGINT       NOT NULL CHECK (balance >= 0),
    jurisdiction   TEXT         NOT NULL,
    joined_day     INTEGER      NOT NULL DEFAULT 0,
    seeded_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, holder_id)
);

CREATE TABLE IF NOT EXISTS sandbox_dividends (
    dividend_id    BIGINT       NOT NULL,
    tenant_id      TEXT         NOT NULL,
    asset_id       BIGINT       NOT NULL,
    amount_cents   BIGINT       NOT NULL CHECK (amount_cents > 0),
    interval_days  INTEGER      NOT NULL CHECK (interval_days > 0),
    seeded_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, dividend_id)
);

-- Audit trail for reset requests. Tenant-scoped like every other table.
CREATE TABLE IF NOT EXISTS sandbox_resets (
    reset_id       BIGSERIAL    PRIMARY KEY,
    tenant_id      TEXT         NOT NULL,
    reset_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    duration_ms    INTEGER      NOT NULL DEFAULT 0,
    rows_removed   BIGINT       NOT NULL DEFAULT 0
);

-- Unscoped template populated once by the datagen seeder. `POST /sandbox/reset`
-- clones these rows into the caller's tenant with a single INSERT ... SELECT,
-- which is what keeps a 100k-holder reset under the 5 s budget.
CREATE TABLE IF NOT EXISTS sandbox_template_assets (
    asset_id       BIGINT       PRIMARY KEY,
    name           TEXT         NOT NULL,
    issuer         TEXT         NOT NULL,
    asset_type     TEXT         NOT NULL,
    jurisdiction   TEXT         NOT NULL,
    price_cents    BIGINT       NOT NULL
);

CREATE TABLE IF NOT EXISTS sandbox_template_holders (
    holder_id      BIGINT       PRIMARY KEY,
    asset_id       BIGINT       NOT NULL,
    address        TEXT         NOT NULL,
    balance        BIGINT       NOT NULL,
    jurisdiction   TEXT         NOT NULL,
    joined_day     INTEGER      NOT NULL
);

CREATE TABLE IF NOT EXISTS sandbox_template_dividends (
    dividend_id    BIGINT       PRIMARY KEY,
    asset_id       BIGINT       NOT NULL,
    amount_cents   BIGINT       NOT NULL,
    interval_days  INTEGER      NOT NULL
);

CREATE INDEX IF NOT EXISTS sandbox_holders_tenant_asset_idx
    ON sandbox_holders (tenant_id, asset_id);
CREATE INDEX IF NOT EXISTS sandbox_dividends_tenant_asset_idx
    ON sandbox_dividends (tenant_id, asset_id);

-- Expose the current tenant to queries without letting a client set arbitrary
-- session state through a bind parameter.
CREATE OR REPLACE FUNCTION tessera_sandbox_tenant() RETURNS TEXT
    LANGUAGE sql STABLE AS $$
        SELECT NULLIF(current_setting('app.sandbox_tenant_id', true), '');
    $$;

ALTER TABLE sandbox_assets     ENABLE ROW LEVEL SECURITY;
ALTER TABLE sandbox_holders    ENABLE ROW LEVEL SECURITY;
ALTER TABLE sandbox_dividends  ENABLE ROW LEVEL SECURITY;
ALTER TABLE sandbox_resets     ENABLE ROW LEVEL SECURITY;
ALTER TABLE sandbox_assets     FORCE  ROW LEVEL SECURITY;
ALTER TABLE sandbox_holders    FORCE  ROW LEVEL SECURITY;
ALTER TABLE sandbox_dividends  FORCE  ROW LEVEL SECURITY;
ALTER TABLE sandbox_resets     FORCE  ROW LEVEL SECURITY;

-- One policy per operation. `USING` filters reads/writes/deletes; `WITH CHECK`
-- prevents a tenant from inserting a row under someone else's tenant_id.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['sandbox_assets','sandbox_holders','sandbox_dividends','sandbox_resets'] LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_policies
            WHERE schemaname = current_schema()
              AND tablename = t
              AND policyname = 'tenant_isolation'
        ) THEN
            EXECUTE format(
                'CREATE POLICY tenant_isolation ON %I '
                'FOR ALL TO PUBLIC '
                'USING (tenant_id = tessera_sandbox_tenant()) '
                'WITH CHECK (tenant_id = tessera_sandbox_tenant())', t);
        END IF;
    END LOOP;
END $$;
