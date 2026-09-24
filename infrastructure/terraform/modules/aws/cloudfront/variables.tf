variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "alb_dns_name" {
  type        = string
  description = "DNS name of the ALB origin"
}

variable "alb_protocol_policy" {
  type        = string
  description = "Protocol policy to reach ALB (http-only, https-only, match-viewer)"
  default     = "http-only"
}

variable "waf_web_acl_arn" {
  type        = string
  description = "ARN of WAF WebACL to associate with CloudFront"
  default     = null
}

variable "domain_names" {
  type        = list(string)
  description = "Custom domain aliases (CNAMEs) for CloudFront"
  default     = []
}

variable "acm_certificate_arn" {
  type        = string
  description = "ACM Certificate ARN in us-east-1 for CloudFront custom domain"
  default     = null
}

variable "price_class" {
  type        = string
  description = "CloudFront Price Class"
  default     = "PriceClass_100"
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
