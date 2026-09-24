output "api_service_id" {
  description = "ID of the Cloud Run API service"
  value       = google_cloud_run_v2_service.api.id
}

output "api_service_uri" {
  description = "URI of the Cloud Run API service"
  value       = google_cloud_run_v2_service.api.uri
}

output "api_service_name" {
  description = "Name of the Cloud Run API service"
  value       = google_cloud_run_v2_service.api.name
}

output "docs_service_id" {
  description = "ID of the Cloud Run Docs service"
  value       = google_cloud_run_v2_service.docs.id
}

output "docs_service_uri" {
  description = "URI of the Cloud Run Docs service"
  value       = google_cloud_run_v2_service.docs.uri
}

output "docs_service_name" {
  description = "Name of the Cloud Run Docs service"
  value       = google_cloud_run_v2_service.docs.name
}
