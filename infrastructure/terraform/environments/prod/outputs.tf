output "cloudfront_domain_name" {
  description = "CloudFront Distribution Domain Name"
  value       = module.cloudfront.distribution_domain_name
}

output "alb_dns_name" {
  description = "Application Load Balancer DNS Name"
  value       = module.alb.alb_dns_name
}

output "ecs_cluster_name" {
  description = "ECS Cluster Name"
  value       = module.ecs.cluster_name
}

output "database_endpoint" {
  description = "Aurora Serverless PostgreSQL Writer Endpoint"
  value       = module.database.postgresql_cluster_endpoint
}

output "dr_standby_dns_name" {
  description = "Warm standby API DNS name registered in the Cloudflare LB pool (#393)"
  value       = module.dr_standby.standby_dns_name
}

output "dr_standby_service" {
  description = "Standby ECS service scaled by the weekly DR drill (#393)"
  value       = module.dr_standby.service_name
}
