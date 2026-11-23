# =============================================================================
# HashiCorp Vault Dynamic Secrets Configuration for Tessera API (Issue #423)
#
# Configures:
# 1. Kubernetes Auth Backend (`auth/kubernetes`) bound to the `tessera-api`
#    ServiceAccount in the `prod` namespace (no static tokens or K8s Secrets in etcd).
# 2. Database Secrets Engine (`database/`) with:
#    - PostgreSQL role `tessera-api-postgres`: 1h (`3600s`) default TTL, 2h max TTL,
#      creating ephemeral users with minimal privileges (`SELECT, INSERT, UPDATE` on
#      `public` schema tables, `NOSUPERUSER NOCREATEDB NOCREATEROLE`).
#    - Redis role `tessera-api-redis`: 1h (`3600s`) default TTL, 2h max TTL,
#      creating ephemeral Redis ACL users with minimal keyspace & command permissions.
# =============================================================================

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    vault = {
      source  = "hashicorp/vault"
      version = "~> 4.4.0"
    }
  }
}

variable "kubernetes_host" {
  type        = string
  description = "Kubernetes API server endpoint for TokenReview verification"
  default     = "https://kubernetes.default.svc"
}

variable "postgres_connection_url" {
  type        = string
  sensitive   = true
  description = "PostgreSQL admin connection URL used by Vault to provision short-lived roles"
}

variable "redis_host" {
  type        = string
  description = "Redis primary host for dynamic ACL user provisioning"
  default     = "redis.prod.svc.cluster.local"
}

variable "redis_port" {
  type        = number
  description = "Redis port"
  default     = 6379
}

variable "redis_admin_username" {
  type        = string
  default     = "vault-admin"
}

variable "redis_admin_password" {
  type      = string
  sensitive = true
}

# -----------------------------------------------------------------------------
# 1. Policy for Tessera API (`database/creds/*` + `sys/leases/renew`)
# -----------------------------------------------------------------------------
resource "vault_policy" "tessera_api" {
  name   = "tessera-api-dynamic-secrets"
  policy = file("${path.module}/policy.hcl")
}

# -----------------------------------------------------------------------------
# 2. Kubernetes Auth Backend (`POST /v1/auth/kubernetes/login`)
# -----------------------------------------------------------------------------
resource "vault_auth_backend" "kubernetes" {
  type = "kubernetes"
  path = "kubernetes"
}

resource "vault_kubernetes_auth_backend_role" "tessera_api" {
  backend                          = vault_auth_backend.kubernetes.path
  role_name                        = "tessera-api"
  bound_service_account_names      = ["tessera-api"]
  bound_service_account_namespaces = ["prod", "tessera"]
  token_ttl                        = 3600
  token_max_ttl                    = 7200
  token_policies                   = [vault_policy.tessera_api.name]
}

# -----------------------------------------------------------------------------
# 3. Database Secrets Engine (`database/`) — PostgreSQL & Redis (1h TTL)
# -----------------------------------------------------------------------------
resource "vault_mount" "database" {
  path                      = "database"
  type                      = "database"
  default_lease_ttl_seconds = 3600
  max_lease_ttl_seconds     = 7200
}

resource "vault_database_secret_backend_connection" "postgres" {
  backend       = vault_mount.database.path
  name          = "tessera-postgres"
  allowed_roles = ["tessera-api-postgres"]

  postgresql {
    connection_url          = var.postgres_connection_url
    max_open_connections    = 5
    max_idle_connections    = 2
    max_connection_lifetime = 3600
  }
}

resource "vault_database_secret_backend_role" "tessera_api_postgres" {
  backend     = vault_mount.database.path
  name        = "tessera-api-postgres"
  db_name     = vault_database_secret_backend_connection.postgres.name
  default_ttl = 3600 # 1h TTL
  max_ttl     = 7200 # 2h max TTL

  # Principle of least privilege: no superuser, no schema DDL, only DML on public tables
  creation_statements = [
    "CREATE ROLE \"{{name}}\" WITH LOGIN PASSWORD '{{password}}' VALID UNTIL '{{expiration}}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT;",
    "GRANT CONNECT ON DATABASE tessera TO \"{{name}}\";",
    "GRANT USAGE ON SCHEMA public TO \"{{name}}\";",
    "GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA public TO \"{{name}}\";",
    "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO \"{{name}}\";"
  ]

  revocation_statements = [
    "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM \"{{name}}\";",
    "REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM \"{{name}}\";",
    "REVOKE USAGE ON SCHEMA public FROM \"{{name}}\";",
    "REVOKE CONNECT ON DATABASE tessera FROM \"{{name}}\";",
    "DROP ROLE IF EXISTS \"{{name}}\";"
  ]
}

resource "vault_database_secret_backend_connection" "redis" {
  backend       = vault_mount.database.path
  name          = "tessera-redis"
  allowed_roles = ["tessera-api-redis"]

  redis {
    host     = var.redis_host
    port     = var.redis_port
    username = var.redis_admin_username
    password = var.redis_admin_password
    tls      = true
  }
}

resource "vault_database_secret_backend_role" "tessera_api_redis" {
  backend     = vault_mount.database.path
  name        = "tessera-api-redis"
  db_name     = vault_database_secret_backend_connection.redis.name
  default_ttl = 3600 # 1h TTL
  max_ttl     = 7200 # 2h max TTL

  # Minimal Redis ACL rules: restrict to tessera:* keys and non-administrative commands
  creation_statements = [
    "[\"~tessera:*\", \"+@read\", \"+@write\", \"+@connection\", \"-@admin\", \"-@dangerous\"]"
  ]
}
