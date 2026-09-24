variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "gcp_project_id" {
  type        = string
  description = "GCP Project ID"
}
