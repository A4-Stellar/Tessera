# Warm standby API for disaster recovery (issue #393).
#
# The standby runs the same API image as production but against the replicated
# read-only Postgres replica. It is kept warm (>= 1 task) so a Cloudflare LB
# weight change is enough to serve traffic — no ECS service creation is needed
# during an incident, which is what makes the <15 min RTO achievable.

locals {
  name_prefix = "${var.project_name}-${var.environment}-dr"
  common_tags = merge(var.tags, {
    Project     = var.project_name
    Environment = var.environment
    Role        = "disaster-recovery-standby"
    ManagedBy   = "Terraform"
  })
}

resource "aws_ecs_cluster" "standby" {
  name = "${local.name_prefix}-cluster"

  setting {
    name  = "containerInsights"
    value = "enabled"
  }

  tags = local.common_tags
}

resource "aws_cloudwatch_log_group" "standby" {
  name              = "/ecs/${local.name_prefix}-api"
  retention_in_days = var.log_retention_days
  tags              = local.common_tags
}

# Security group for the internal ALB: only Cloudflare edge ranges (or the DR
# drill runner) may reach the standby directly.
resource "aws_security_group" "alb" {
  name        = "${local.name_prefix}-alb-sg"
  description = "Internal standby API ALB"
  vpc_id      = var.vpc_id

  dynamic "ingress" {
    for_each = var.allowed_cidr_blocks
    content {
      from_port   = 80
      to_port     = 80
      protocol    = "tcp"
      cidr_blocks = [ingress.value]
      description = "Standby API ingress"
    }
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = local.common_tags
}

resource "aws_security_group" "tasks" {
  name        = "${local.name_prefix}-tasks-sg"
  description = "Read-only standby API tasks"
  vpc_id      = var.vpc_id

  ingress {
    from_port       = 8080
    to_port         = 8080
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
    description     = "Traffic from the standby ALB only"
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = local.common_tags
}

resource "aws_lb" "standby" {
  name               = "${local.name_prefix}-alb"
  internal           = true
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = var.private_subnet_ids

  tags = local.common_tags
}

resource "aws_lb_target_group" "standby" {
  name        = "${local.name_prefix}-api"
  port        = 8080
  protocol    = "HTTP"
  vpc_id      = var.vpc_id
  target_type = "ip"

  health_check {
    path                = "/health"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }

  tags = local.common_tags
}

resource "aws_lb_listener" "standby" {
  load_balancer_arn = aws_lb.standby.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.standby.arn
  }
}

resource "aws_ecs_task_definition" "standby" {
  family                   = "${local.name_prefix}-api"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = var.cpu
  memory                   = var.memory
  execution_role_arn       = var.execution_role_arn
  task_role_arn            = var.task_role_arn

  container_definitions = jsonencode([
    {
      name      = "tessera-api"
      image     = var.api_image
      essential = true
      portMappings = [
        {
          containerPort = 8080
          hostPort      = 8080
          protocol      = "tcp"
        }
      ]
      environment = [
        { name = "PORT", value = "8080" },
        # Defence in depth: the process cannot accept writes even if the
        # connection string accidentally points at the primary.
        { name = "RWA_READ_ONLY", value = "true" },
        { name = "RWA_RPC_URL", value = var.soroban_rpc_url },
        { name = "RWA_REGISTRY_ID", value = var.registry_id },
        { name = "RWA_DIVIDEND_ID", value = var.dividend_id },
        { name = "RWA_READ_SOURCE", value = var.read_source },
        { name = "RUST_LOG", value = "tessera_api=info,tower_http=warn" }
      ]
      secrets = [
        { name = "RWA_DATABASE_URL", valueFrom = var.replica_secret_arn }
      ]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.standby.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "dr-standby"
        }
      }
      healthCheck = {
        # The distroless image has no shell or curl; the binary probes /health.
        command     = ["CMD", "/usr/local/bin/tessera-api", "healthcheck"]
        interval    = 30
        timeout     = 5
        retries     = 3
        startPeriod = 10
      }
    }
  ])

  tags = local.common_tags
}

resource "aws_ecs_service" "standby" {
  name            = "${local.name_prefix}-api-service"
  cluster         = aws_ecs_cluster.standby.id
  task_definition = aws_ecs_task_definition.standby.arn
  desired_count   = var.desired_count
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = var.private_subnet_ids
    security_groups  = [aws_security_group.tasks.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.standby.arn
    container_name   = "tessera-api"
    container_port   = 8080
  }

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  tags = local.common_tags
}
