output "alertmanager_integration_url" {
  description = "Mount as /etc/alertmanager/secrets/grafana-oncall-url for the grafana-oncall receiver"
  value       = grafana_oncall_integration.alertmanager.link
  sensitive   = true
}

output "escalation_chain_id" {
  value = grafana_oncall_escalation_chain.p1.id
}
