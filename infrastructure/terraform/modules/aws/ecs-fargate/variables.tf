variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "aws_region" {
  type        = string
  description = "AWS Region"
}

variable "private_subnet_ids" {
  type        = list(string)
  description = "Private subnet IDs for task placement"
}

variable "ecs_security_group_id" {
  type        = string
  description = "Security Group ID for ECS tasks"
}

variable "execution_role_arn" {
  type        = string
  description = "ECS Task Execution Role ARN"
}

variable "task_role_arn" {
  type        = string
  description = "ECS Task Role ARN"
}

variable "api_target_group_arn" {
  type        = string
  description = "ALB Target Group ARN for API"
}

variable "docs_target_group_arn" {
  type        = string
  description = "ALB Target Group ARN for Docs"
}

# Container Images
variable "api_image" {
  type        = string
  description = "Docker image URI for API"
}

variable "docs_image" {
  type        = string
  description = "Docker image URI for Docs"
}

# Resource Sizing
variable "api_cpu" {
  type        = string
  description = "CPU units for API task"
  default     = "256"
}

variable "api_memory" {
  type        = string
  description = "Memory (MB) for API task"
  default     = "512"
}

variable "docs_cpu" {
  type        = string
  description = "CPU units for Docs task"
  default     = "256"
}

variable "docs_memory" {
  type        = string
  description = "Memory (MB) for Docs task"
  default     = "512"
}

# Task Counts & Scaling
variable "api_desired_count" {
  type        = number
  description = "Desired number of API task instances"
  default     = 2
}

variable "api_min_count" {
  type        = number
  description = "Minimum API task instances"
  default     = 1
}

variable "api_max_count" {
  type        = number
  description = "Maximum API task instances"
  default     = 5
}

variable "docs_desired_count" {
  type        = number
  description = "Desired number of Docs task instances"
  default     = 2
}

# Application Configuration
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
  description = "Public API base URL for Docs"
  default     = "http://localhost:8080"
}

variable "log_retention_days" {
  type        = number
  description = "CloudWatch log retention in days"
  default     = 30
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
