variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "tags" {
  type        = map(string)
  description = "Resource tags"
  default     = {}
}
