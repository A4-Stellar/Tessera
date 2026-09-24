output "network_id" {
  description = "ID of the VPC Network"
  value       = google_compute_network.vpc.id
}

output "network_name" {
  description = "Name of the VPC Network"
  value       = google_compute_network.vpc.name
}

output "subnet_id" {
  description = "ID of the Subnetwork"
  value       = google_compute_subnetwork.subnet.id
}

output "connector_id" {
  description = "ID of the Serverless VPC Access Connector"
  value       = var.enable_vpc_connector ? google_vpc_access_connector.connector[0].id : null
}
