variable "project_name" {
  type        = string
  description = "Project name prefix"
  default     = "tessera"
}

variable "environment" {
  type        = string
  description = "Target environment"
}

variable "gcp_region" {
  type        = string
  description = "GCP Region"
}

variable "subnet_cidr" {
  type        = string
  description = "Subnet CIDR block"
  default     = "10.10.0.0/24"
}

variable "enable_vpc_connector" {
  type        = bool
  description = "Whether to provision Serverless VPC Access connector"
  default     = true
}

variable "connector_cidr" {
  type        = string
  description = "CIDR range for Serverless VPC Access connector (/28 required)"
  default     = "10.10.8.0/28"
}
