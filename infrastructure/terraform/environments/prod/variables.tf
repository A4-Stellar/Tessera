variable "project_name" {
  type        = string
  description = "Project name"
  default     = "tessera"
}

variable "aws_region" {
  type        = string
  description = "AWS Region"
  default     = "us-east-1"
}

variable "availability_zones" {
  type        = list(string)
  description = "Multi-AZ availability zones list"
  default     = ["us-east-1a", "us-east-1b", "us-east-1c"]
}

variable "alb_certificate_arn" {
  type        = string
  description = "ACM Certificate ARN for ALB"
}

variable "cloudfront_certificate_arn" {
  type        = string
  description = "ACM Certificate ARN in us-east-1 for CloudFront"
}

variable "custom_domain_names" {
  type        = list(string)
  description = "Custom domains for CloudFront (e.g. ['tessera.xyz', 'api.tessera.xyz'])"
  default     = []
}

variable "api_image" {
  type        = string
  description = "ECR/Docker image for Tessera API"
}

variable "docs_image" {
  type        = string
  description = "ECR/Docker image for Tessera Docs"
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
  description = "Read source Stellar account"
}

variable "next_public_api_base_url" {
  type        = string
  description = "Public API URL for Docs"
}
