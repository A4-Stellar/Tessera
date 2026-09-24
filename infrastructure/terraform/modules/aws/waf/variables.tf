variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "scope" {
  type        = string
  description = "WAF Scope (CLOUDFRONT or REGIONAL)"
  default     = "REGIONAL"
}

variable "rate_limit_per_ip" {
  type        = number
  description = "Maximum requests allowed per 5-minute window per IP"
  default     = 2000
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
