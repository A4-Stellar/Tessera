# DB Subnet Group
resource "aws_db_subnet_group" "main" {
  name       = "${var.project_name}-${var.environment}-db-subnet-group"
  subnet_ids = var.private_subnet_ids

  tags = merge(var.tags, {
    Name = "${var.project_name}-${var.environment}-db-subnet-group"
  })
}

# Master Password Generation
resource "random_password" "db_master_password" {
  length  = 24
  special = false
}

# Aurora PostgreSQL Serverless v2 Cluster
resource "aws_rds_cluster" "postgresql" {
  count                   = var.enable_postgresql ? 1 : 0
  cluster_identifier      = "${var.project_name}-${var.environment}-pg-cluster"
  engine                  = "aurora-postgresql"
  engine_mode             = "provisioned"
  engine_version          = "16.1"
  database_name           = var.database_name
  master_username         = "tessera_admin"
  master_password         = random_password.db_master_password.result
  db_subnet_group_name    = aws_db_subnet_group.main.name
  vpc_security_group_ids  = [var.database_security_group_id]
  skip_final_snapshot     = var.environment != "prod"
  deletion_protection     = var.environment == "prod"
  storage_encrypted       = true

  serverlessv2_scaling_configuration {
    min_capacity = var.min_acu
    max_capacity = var.max_acu
  }

  tags = var.tags
}

resource "aws_rds_cluster_instance" "postgresql_instances" {
  count               = var.enable_postgresql ? var.instance_count : 0
  cluster_identifier  = aws_rds_cluster.postgresql[0].id
  instance_class      = "db.serverless"
  engine              = aws_rds_cluster.postgresql[0].engine
  engine_version      = aws_rds_cluster.postgresql[0].engine_version
  publicly_accessible = false

  tags = var.tags
}

# ElastiCache Redis Serverless
resource "aws_elasticache_serverless_cache" "redis" {
  count        = var.enable_redis ? 1 : 0
  engine       = "redis"
  name         = "${var.project_name}-${var.environment}-redis"
  description  = "Serverless Redis cache for ${var.project_name} ${var.environment}"
  subnet_ids   = var.private_subnet_ids
  security_group_ids = [var.database_security_group_id]

  cache_usage_limits {
    data_storage {
      maximum = 5
      unit    = "GB"
    }
    ecpu_per_second {
      maximum = 5000
    }
  }

  tags = var.tags
}
