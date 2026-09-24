output "api_hostname" {
  description = "Fully qualified domain name for API"
  value       = "${var.api_subdomain}.${var.domain_name}"
}

output "docs_hostname" {
  description = "Fully qualified domain name for Docs"
  value       = "${var.docs_subdomain}.${var.domain_name}"
}
