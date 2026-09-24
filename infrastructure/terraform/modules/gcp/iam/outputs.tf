output "service_account_email" {
  description = "Email of the created Service Account"
  value       = google_service_account.cloud_run_sa.email
}

output "service_account_id" {
  description = "ID of the created Service Account"
  value       = google_service_account.cloud_run_sa.id
}
