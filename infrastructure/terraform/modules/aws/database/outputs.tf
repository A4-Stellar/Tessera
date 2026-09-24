output "postgresql_cluster_endpoint" {
  description = "Writer endpoint for Aurora PostgreSQL cluster"
  value       = var.enable_postgresql ? aws_rds_cluster.postgresql[0].endpoint : null
}

output "postgresql_cluster_reader_endpoint" {
  description = "Reader endpoint for Aurora PostgreSQL cluster"
  value       = var.enable_postgresql ? aws_rds_cluster.postgresql[0].reader_endpoint : null
}

output "postgresql_master_username" {
  description = "Master username for PostgreSQL"
  value       = var.enable_postgresql ? aws_rds_cluster.postgresql[0].master_username : null
}

output "postgresql_master_password" {
  description = "Master password for PostgreSQL (sensitive)"
  value       = var.enable_postgresql ? random_password.db_master_password.result : null
  sensitive   = true
}

output "redis_endpoint" {
  description = "Redis Serverless reader/writer endpoint"
  value       = var.enable_redis ? aws_elasticache_serverless_cache.redis[0].endpoint[0].address : null
}

output "redis_port" {
  description = "Redis Serverless port"
  value       = var.enable_redis ? aws_elasticache_serverless_cache.redis[0].endpoint[0].port : null
}
