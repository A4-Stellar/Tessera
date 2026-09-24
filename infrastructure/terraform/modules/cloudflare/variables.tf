variable "cloudflare_zone_id" {
  type        = string
  description = "Cloudflare Zone ID"
}

variable "domain_name" {
  type        = string
  description = "Root domain name (e.g. tessera.xyz)"
}

variable "api_subdomain" {
  type        = string
  description = "Subdomain for API (e.g. api or api-dev)"
  default     = "api"
}

variable "docs_subdomain" {
  type        = string
  description = "Subdomain for Docs (e.g. docs or docs-dev)"
  default     = "docs"
}

variable "origin_target" {
  type        = string
  description = "Target origin hostname (ALB DNS name or CloudFront domain)"
}

variable "rate_limit_threshold" {
  type        = number
  description = "Max requests allowed per 60s window per IP"
  default     = 600
}
