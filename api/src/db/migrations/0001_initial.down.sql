-- Down migration for 0001: drops every object created by `0001_initial.sql`.
--
-- Reverse order of creation so FK dependencies resolve cleanly. Indexes are
-- dropped implicitly with their tables; the IF EXISTS guards make this
-- idempotent and safe to run on databases where some objects never existed
-- (e.g. plain PostgreSQL, where the hypertable conversion was skipped).

DROP TABLE IF EXISTS snapshot_history;
DROP TABLE IF EXISTS events;
DROP TABLE IF EXISTS transactions;
DROP TABLE IF EXISTS holders;
DROP TABLE IF EXISTS assets;
