output "postgresql_instance_name" {
  description = "Name of the Cloud SQL instance"
  value       = var.enable_postgresql ? google_sql_database_instance.postgres[0].name : null
}

output "postgresql_private_ip" {
  description = "Private IP of the PostgreSQL instance"
  value       = var.enable_postgresql ? google_sql_database_instance.postgres[0].private_ip_address : null
}

output "postgresql_user" {
  description = "Username for PostgreSQL"
  value       = var.enable_postgresql ? google_sql_user.users[0].name : null
}

output "postgresql_password" {
  description = "Password for PostgreSQL user (sensitive)"
  value       = var.enable_postgresql ? random_password.db_password.result : null
  sensitive   = true
}

output "redis_host" {
  description = "Host IP of the Memorystore Redis instance"
  value       = var.enable_redis ? google_redis_instance.cache[0].host : null
}

output "redis_port" {
  description = "Port of the Memorystore Redis instance"
  value       = var.enable_redis ? google_redis_instance.cache[0].port : null
}
