variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "enable_adaptive_protection" {
  type        = bool
  description = "Enable Cloud Armor Adaptive Protection L7 DDoS defense"
  default     = true
}

variable "rate_limit_count" {
  type        = number
  description = "Requests allowed per client IP per minute"
  default     = 300
}
