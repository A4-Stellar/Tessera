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
