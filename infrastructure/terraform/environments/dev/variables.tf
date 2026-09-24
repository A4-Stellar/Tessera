variable "project_name" {
  type        = string
  description = "Project name"
  default     = "tessera"
}

variable "aws_region" {
  type        = string
  description = "AWS region"
  default     = "us-east-1"
}

variable "availability_zones" {
  type        = list(string)
  description = "AZs"
  default     = ["us-east-1a", "us-east-1b"]
}

variable "certificate_arn" {
  type        = string
  description = "ACM Certificate ARN for ALB (optional)"
  default     = null
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
  default     = "CBX5SMLTXX6JP4HA5GQIO2V6QM7WCUGL2GZ6D4U773HMRI6RXISKPUR3"
}

variable "dividend_id" {
  type        = string
  description = "Dividend Contract ID"
  default     = "CAR4XY3CEBQWFOL27JEWFW34KXSIZA7RFKDQMEIV7ZU723RWY37I2SYX"
}

variable "read_source" {
  type        = string
  description = "Read source Stellar account"
  default     = "GAIQGTOBTTLLDJ4SWGGESM7UWJ2DI4K3ZNHUSHPDKJL2IE5FKY3BSRAA"
}

variable "next_public_api_base_url" {
  type        = string
  description = "API base URL for docs"
  default     = "http://localhost:8080"
}
