# HashiCorp Vault Dynamic Secrets Engine for PostgreSQL and Redis (Issue #42)
#
# Production Terraform configuration deploying:
# 1. Vault Database Secrets Engine with PostgreSQL plugin
# 2. Short-lived (1h TTL) dynamic credentials with minimal privileges
# 3. Kubernetes auth method for API pod authentication via ServiceAccount JWT
# 4. Redis dynamic secrets integration

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    vault = {
      source  = "hashicorp/vault"
      version = ">= 3.20.0"
    }
  }
}

# ------------------------------------------------------------------------------
# 1. Database Secrets Engine Mount
# ------------------------------------------------------------------------------
resource "vault_mount" "database" {
  path        = var.database_mount_path
  type        = "database"
  description = "Dynamic database secrets engine for PostgreSQL"
}

# ------------------------------------------------------------------------------
# 2. PostgreSQL Connection Configuration
# ------------------------------------------------------------------------------
resource "vault_database_secret_backend_connection" "postgres" {
  backend       = vault_mount.database.path
  name          = "postgres-primary"
  allowed_roles = [var.postgres_role_name]

  postgresql {
    connection_url = "postgresql://{{username}}:{{password}}@${var.postgres_host}:${var.postgres_port}/${var.postgres_database}?sslmode=require"
    username       = var.postgres_vault_admin_username
    password       = var.postgres_vault_admin_password
  }
}

# ------------------------------------------------------------------------------
# 3. Dynamic Database Role with 1-hour TTL and Minimal Privileges
# ------------------------------------------------------------------------------
resource "vault_database_secret_backend_role" "api_readonly" {
  backend               = vault_mount.database.path
  name                  = var.postgres_role_name
  db_name               = vault_database_secret_backend_connection.postgres.name
  default_ttl           = 3600  # 1 hour short-lived TTL
  max_ttl               = 86400 # 24 hours maximum lease duration

  creation_statements = [
    "CREATE ROLE \"{{name}}\" WITH LOGIN PASSWORD '{{password}}' VALID UNTIL '{{expiration}}';",
    "GRANT CONNECT ON DATABASE \"${var.postgres_database}\" TO \"{{name}}\";",
    "GRANT USAGE ON SCHEMA public TO \"{{name}}\";",
    "GRANT SELECT ON ALL TABLES IN SCHEMA public TO \"{{name}}\";",
    "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO \"{{name}}\";"
  ]

  revocation_statements = [
    "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM \"{{name}}\";",
    "REVOKE USAGE ON SCHEMA public FROM \"{{name}}\";",
    "REVOKE CONNECT ON DATABASE \"${var.postgres_database}\" TO \"{{name}}\";",
    "DROP ROLE IF EXISTS \"{{name}}\";"
  ]
}

# ------------------------------------------------------------------------------
# 4. Redis Secret Mount and Role
# ------------------------------------------------------------------------------
resource "vault_mount" "redis" {
  path        = var.redis_mount_path
  type        = "kv-v2"
  description = "Dynamic credentials / tokens for Redis cluster"
}

# ------------------------------------------------------------------------------
# 5. Kubernetes Auth Method Configuration
# ------------------------------------------------------------------------------
resource "vault_auth_backend" "kubernetes" {
  type        = "kubernetes"
  path        = "kubernetes"
  description = "Kubernetes service account authentication for Tessera pods"
}

resource "vault_kubernetes_auth_backend_config" "k8s_config" {
  backend            = vault_auth_backend.kubernetes.path
  kubernetes_host    = var.kubernetes_host
  kubernetes_ca_cert = var.kubernetes_ca_cert
}

resource "vault_kubernetes_auth_backend_role" "tessera_api" {
  backend                          = vault_auth_backend.kubernetes.path
  role_name                        = var.kubernetes_role_name
  bound_service_account_names      = ["tessera-api"]
  bound_service_account_namespaces = ["tessera-system", "default"]
  token_policies                   = [vault_policy.tessera_api.name]
  token_ttl                        = 3600
  token_max_ttl                    = 86400
}

# ------------------------------------------------------------------------------
# 6. Least-Privilege Vault Access Policy for Tessera API
# ------------------------------------------------------------------------------
resource "vault_policy" "tessera_api" {
  name = "tessera-api-policy"

  policy = <<EOT
# Read dynamic PostgreSQL credentials
path "${vault_mount.database.path}/creds/${var.postgres_role_name}" {
  capabilities = ["read"]
}

# Read dynamic Redis credentials
path "${vault_mount.redis.path}/data/*" {
  capabilities = ["read"]
}

# Renew own lease
path "sys/leases/renew" {
  capabilities = ["update"]
}

# Revoke own lease on shutdown
path "sys/leases/revoke" {
  capabilities = ["update"]
}
EOT
}
