# HashiCorp Vault ACL Policy for Tessera API (Issue #423)
# Principle of least privilege: allows fetching short-lived (1h TTL) dynamic
# PostgreSQL and Redis credentials and renewing active leases via PUT /v1/sys/leases/renew.

path "database/creds/tessera-api-postgres" {
  capabilities = ["read"]
}

path "database/creds/tessera-api-redis" {
  capabilities = ["read"]
}

path "sys/leases/renew" {
  capabilities = ["update"]
}

path "sys/leases/revoke" {
  capabilities = ["update"]
}
