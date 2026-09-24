variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "gcp_region" {
  type        = string
  description = "GCP Region"
}

variable "vpc_network_id" {
  type        = string
  description = "VPC Network self_link or ID for private IP"
}

variable "enable_postgresql" {
  type        = bool
  description = "Enable Cloud SQL PostgreSQL"
  default     = true
}

variable "enable_redis" {
  type        = bool
  description = "Enable Cloud Memorystore Redis"
  default     = false
}

variable "database_name" {
  type        = string
  description = "PostgreSQL database name"
  default     = "tessera"
}

variable "db_tier" {
  type        = string
  description = "Cloud SQL machine tier"
  default     = "db-custom-1-3840"
}

variable "db_disk_size_gb" {
  type        = number
  description = "Database disk size in GB"
  default     = 20
}

variable "redis_memory_size_gb" {
  type        = number
  description = "Redis memory capacity in GB"
  default     = 1
}
