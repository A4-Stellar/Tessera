#!/usr/bin/env bash
# =============================================================================
# Idempotent Vault Dynamic Secrets Bootstrap Script (Issue #423)
#
# Configures:
# 1. `database` secrets engine with 1h (3600s) TTL PostgreSQL & Redis roles
# 2. `kubernetes` auth method bound to the `tessera-api` ServiceAccount
# =============================================================================
set -euo pipefail

VAULT_ADDR="${VAULT_ADDR:-http://127.0.0.1:8200}"
PG_CONN_URL="${PG_CONN_URL:-postgresql://vault_admin:vault_admin@postgres.prod.svc.cluster.local:5432/tessera?sslmode=require}"
REDIS_HOST="${REDIS_HOST:-redis.prod.svc.cluster.local}"
REDIS_PORT="${REDIS_PORT:-6379}"
REDIS_ADMIN_USER="${REDIS_ADMIN_USER:-vault-admin}"
REDIS_ADMIN_PASS="${REDIS_ADMIN_PASS:-changeme}"
K8S_HOST="${K8S_HOST:-https://kubernetes.default.svc}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "[vault-bootstrap] Writing policy tessera-api-dynamic-secrets..."
vault policy write tessera-api-dynamic-secrets "${SCRIPT_DIR}/policy.hcl"

echo "[vault-bootstrap] Enabling kubernetes auth backend..."
vault auth enable kubernetes 2>/dev/null || true

vault write auth/kubernetes/config \
  kubernetes_host="${K8S_HOST}"

vault write auth/kubernetes/role/tessera-api \
  bound_service_account_names="tessera-api" \
  bound_service_account_namespaces="prod,tessera" \
  policies="tessera-api-dynamic-secrets" \
  ttl="1h" \
  max_ttl="2h"

echo "[vault-bootstrap] Enabling database secrets engine..."
vault secrets enable -path=database database 2>/dev/null || true

echo "[vault-bootstrap] Configuring PostgreSQL dynamic role (1h TTL, minimal privileges)..."
vault write database/config/tessera-postgres \
  plugin_name="postgresql-database-plugin" \
  allowed_roles="tessera-api-postgres" \
  connection_url="${PG_CONN_URL}" \
  max_open_connections=5 \
  max_idle_connections=2 \
  max_connection_lifetime="1h"

vault write database/roles/tessera-api-postgres \
  db_name="tessera-postgres" \
  default_ttl="1h" \
  max_ttl="2h" \
  creation_statements="CREATE ROLE \"{{name}}\" WITH LOGIN PASSWORD '{{password}}' VALID UNTIL '{{expiration}}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT; GRANT CONNECT ON DATABASE tessera TO \"{{name}}\"; GRANT USAGE ON SCHEMA public TO \"{{name}}\"; GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA public TO \"{{name}}\"; GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO \"{{name}}\";" \
  revocation_statements="REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM \"{{name}}\"; REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM \"{{name}}\"; REVOKE USAGE ON SCHEMA public FROM \"{{name}}\"; REVOKE CONNECT ON DATABASE tessera FROM \"{{name}}\"; DROP ROLE IF EXISTS \"{{name}}\";"

echo "[vault-bootstrap] Configuring Redis dynamic AUTH role (1h TTL, minimal ACLs)..."
vault write database/config/tessera-redis \
  plugin_name="redis-database-plugin" \
  allowed_roles="tessera-api-redis" \
  host="${REDIS_HOST}" \
  port="${REDIS_PORT}" \
  username="${REDIS_ADMIN_USER}" \
  password="${REDIS_ADMIN_PASS}"

vault write database/roles/tessera-api-redis \
  db_name="tessera-redis" \
  default_ttl="1h" \
  max_ttl="2h" \
  creation_statements='["~tessera:*", "+@read", "+@write", "+@connection", "-@admin", "-@dangerous"]'

echo "[vault-bootstrap] Vault dynamic secrets configuration complete."
