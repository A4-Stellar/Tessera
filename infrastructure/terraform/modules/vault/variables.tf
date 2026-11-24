variable "database_mount_path" {
  description = "Mount path for Vault database engine"
  type        = string
  default     = "database"
}

variable "postgres_role_name" {
  description = "Role name for dynamic PostgreSQL user generation"
  type        = string
  default     = "api-readonly"
}

variable "postgres_host" {
  description = "PostgreSQL hostname"
  type        = string
}

variable "postgres_port" {
  description = "PostgreSQL port"
  type        = number
  default     = 5432
}

variable "postgres_database" {
  description = "PostgreSQL database name"
  type        = string
  default     = "tessera"
}

variable "postgres_vault_admin_username" {
  description = "Superuser username for Vault to manage DB users"
  type        = string
  default     = "vault_admin"
}

variable "postgres_vault_admin_password" {
  description = "Superuser password for Vault to manage DB users"
  type        = string
  sensitive   = true
}

variable "redis_mount_path" {
  description = "Mount path for Redis secrets engine"
  type        = string
  default     = "redis"
}

variable "kubernetes_host" {
  description = "Kubernetes API host for Vault auth"
  type        = string
  default     = "https://kubernetes.default.svc"
}

variable "kubernetes_ca_cert" {
  description = "PEM encoded CA cert for Kubernetes API"
  type        = string
  default     = ""
}

variable "kubernetes_role_name" {
  description = "Vault role for Kubernetes auth"
  type        = string
  default     = "tessera-api"
}
