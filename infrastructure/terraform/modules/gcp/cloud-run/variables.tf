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

variable "service_account_email" {
  type        = string
  description = "Service Account email for Cloud Run"
}

variable "api_image" {
  type        = string
  description = "Container image for Tessera API"
}

variable "docs_image" {
  type        = string
  description = "Container image for Tessera Docs"
}

variable "api_cpu" {
  type        = string
  description = "CPU limit for API"
  default     = "1000m"
}

variable "api_memory" {
  type        = string
  description = "Memory limit for API"
  default     = "512Mi"
}

variable "docs_cpu" {
  type        = string
  description = "CPU limit for Docs"
  default     = "1000m"
}

variable "docs_memory" {
  type        = string
  description = "Memory limit for Docs"
  default     = "512Mi"
}

variable "api_min_instances" {
  type        = number
  description = "Minimum instances for API"
  default     = 1
}

variable "api_max_instances" {
  type        = number
  description = "Maximum instances for API"
  default     = 10
}

variable "docs_min_instances" {
  type        = number
  description = "Minimum instances for Docs"
  default     = 1
}

variable "docs_max_instances" {
  type        = number
  description = "Maximum instances for Docs"
  default     = 5
}

variable "soroban_rpc_url" {
  type        = string
  description = "Soroban RPC URL"
  default     = "https://soroban-testnet.stellar.org"
}

variable "registry_id" {
  type        = string
  description = "Registry Contract ID"
}

variable "dividend_id" {
  type        = string
  description = "Dividend Contract ID"
}

variable "read_source" {
  type        = string
  description = "Read source account"
}

variable "next_public_api_base_url" {
  type        = string
  description = "Public API URL for Docs"
  default     = "http://localhost:8080"
}

variable "vpc_connector_id" {
  type        = string
  description = "VPC connector ID (optional)"
  default     = null
}

variable "ingress_mode" {
  type        = string
  description = "Ingress traffic mode"
  default     = "INGRESS_TRAFFIC_ALL"
}

variable "allow_unauthenticated" {
  type        = bool
  description = "Whether to allow unauthenticated public access"
  default     = true
}
