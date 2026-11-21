variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment (the standby mirrors the primary environment)"
  default     = "prod"
}

variable "aws_region" {
  type        = string
  description = "AWS region hosting the warm standby"
}

variable "vpc_id" {
  type        = string
  description = "VPC to place the standby ECS tasks and internal ALB in"
}

variable "private_subnet_ids" {
  type        = list(string)
  description = "At least two private subnet IDs across AZs"
}

variable "api_image" {
  type        = string
  description = "Container image URI for the read-only Tessera API"
}

variable "execution_role_arn" {
  type        = string
  description = "ECS task execution role ARN (from the iam module)"
}

variable "task_role_arn" {
  type        = string
  description = "ECS task role ARN (from the iam module)"
}

variable "replica_secret_arn" {
  type        = string
  description = "Secrets Manager ARN containing the read-replica DATABASE URL"
}

variable "allowed_cidr_blocks" {
  type        = list(string)
  description = "CIDRs allowed to reach the standby ALB (Cloudflare edge ranges / drill runner)"
  default     = ["0.0.0.0/0"]
}

variable "desired_count" {
  type        = number
  description = "Number of warm standby API tasks; keep >= 1 for fast RTO"
  default     = 1
}

variable "cpu" {
  type    = string
  default = "512"
}

variable "memory" {
  type    = string
  default = "1024"
}

variable "soroban_rpc_url" {
  type    = string
  default = "https://soroban-testnet.stellar.org"
}

variable "registry_id" {
  type = string
}

variable "dividend_id" {
  type = string
}

variable "read_source" {
  type = string
}

variable "log_retention_days" {
  type    = number
  default = 30
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
