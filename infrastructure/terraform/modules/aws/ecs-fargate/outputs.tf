output "cluster_id" {
  description = "ID of the ECS Cluster"
  value       = aws_ecs_cluster.main.id
}

output "cluster_name" {
  description = "Name of the ECS Cluster"
  value       = aws_ecs_cluster.main.name
}

output "api_service_name" {
  description = "Name of the API ECS Service"
  value       = aws_ecs_service.api.name
}

output "docs_service_name" {
  description = "Name of the Docs ECS Service"
  value       = aws_ecs_service.docs.name
}
