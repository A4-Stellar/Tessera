variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "private_subnet_ids" {
  type        = list(string)
  description = "Private subnet IDs for database placement"
}

variable "database_security_group_id" {
  type        = string
  description = "Security Group ID for Database"
}

variable "enable_postgresql" {
  type        = bool
  description = "Whether to provision Aurora PostgreSQL Serverless v2"
  default     = true
}

variable "enable_redis" {
  type        = bool
  description = "Whether to provision ElastiCache Redis Serverless"
  default     = false
}

variable "database_name" {
  type        = string
  description = "Initial PostgreSQL database name"
  default     = "tessera"
}

variable "min_acu" {
  type        = number
  description = "Minimum Aurora Capacity Units (ACUs)"
  default     = 0.5
}

variable "max_acu" {
  type        = number
  description = "Maximum Aurora Capacity Units (ACUs)"
  default     = 4.0
}

variable "instance_count" {
  type        = number
  description = "Number of database instances (e.g. 1 for dev, 2 for HA prod)"
  default     = 1
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
