output "security_policy_id" {
  description = "ID of the Cloud Armor Security Policy"
  value       = google_compute_security_policy.cloud_armor.id
}

output "security_policy_name" {
  description = "Name of the Cloud Armor Security Policy"
  value       = google_compute_security_policy.cloud_armor.name
}
