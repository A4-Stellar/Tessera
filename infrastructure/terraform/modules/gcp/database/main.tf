# Random password for DB user
resource "random_password" "db_password" {
  length  = 24
  special = false
}

# Cloud SQL PostgreSQL Instance
resource "google_sql_database_instance" "postgres" {
  count            = var.enable_postgresql ? 1 : 0
  name             = "${var.project_name}-${var.environment}-pg"
  database_version = "POSTGRES_16"
  region           = var.gcp_region

  deletion_protection = var.environment == "prod"

  settings {
    tier              = var.db_tier
    availability_type = var.environment == "prod" ? "REGIONAL" : "ZONAL"
    disk_size         = var.db_disk_size_gb
    disk_type         = "PD_SSD"

    backup_configuration {
      enabled    = true
      start_time = "02:00"
    }

    ip_configuration {
      ipv4_enabled    = false
      private_network = var.vpc_network_id
    }
  }
}

resource "google_sql_database" "database" {
  count    = var.enable_postgresql ? 1 : 0
  name     = var.database_name
  instance = google_sql_database_instance.postgres[0].name
}

resource "google_sql_user" "users" {
  count    = var.enable_postgresql ? 1 : 0
  name     = "tessera_admin"
  instance = google_sql_database_instance.postgres[0].name
  password = random_password.db_password.result
}

# Cloud Memorystore Redis
resource "google_redis_instance" "cache" {
  count              = var.enable_redis ? 1 : 0
  name               = "${var.project_name}-${var.environment}-redis"
  tier               = var.environment == "prod" ? "STANDARD_HA" : "BASIC"
  memory_size_gb     = var.redis_memory_size_gb
  region             = var.gcp_region
  authorized_network = var.vpc_network_id
  redis_version      = "REDIS_7_0"
  display_name       = "Tessera Redis Cache"
}
