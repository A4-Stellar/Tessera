terraform {
  required_version = ">= 1.5.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project     = "Tessera"
      Environment = "prod"
      ManagedBy   = "Terraform"
    }
  }
}

# 1. Multi-AZ VPC
module "vpc" {
  source               = "../../modules/aws/vpc"
  project_name         = var.project_name
  environment          = "prod"
  vpc_cidr             = "10.100.0.0/16"
  availability_zones   = var.availability_zones
  public_subnet_cidrs  = ["10.100.1.0/24", "10.100.2.0/24", "10.100.3.0/24"]
  private_subnet_cidrs = ["10.100.10.0/24", "10.100.20.0/24", "10.100.30.0/24"]
  single_nat_gateway   = false # High availability across multiple AZs
}

# 2. IAM Roles
module "iam" {
  source       = "../../modules/aws/iam"
  project_name = var.project_name
  environment  = "prod"
}

# 3. Security Groups
module "security_groups" {
  source       = "../../modules/aws/security-groups"
  project_name = var.project_name
  environment  = "prod"
  vpc_id       = module.vpc.vpc_id
  vpc_cidr     = module.vpc.vpc_cidr_block
}

# 4. AWS WAF Protection
module "waf" {
  source            = "../../modules/aws/waf"
  project_name      = var.project_name
  environment       = "prod"
  scope             = "CLOUDFRONT"
  rate_limit_per_ip = 2000
}

# 5. Application Load Balancer
module "alb" {
  source                = "../../modules/aws/alb"
  project_name          = var.project_name
  environment           = "prod"
  vpc_id                = module.vpc.vpc_id
  public_subnet_ids     = module.vpc.public_subnet_ids
  alb_security_group_id = module.security_groups.alb_security_group_id
  certificate_arn       = var.alb_certificate_arn
}

# 6. ECS Fargate Cluster & Scalable Services
module "ecs" {
  source                    = "../../modules/aws/ecs-fargate"
  project_name              = var.project_name
  environment               = "prod"
  aws_region                = var.aws_region
  private_subnet_ids        = module.vpc.private_subnet_ids
  ecs_security_group_id     = module.security_groups.ecs_security_group_id
  execution_role_arn        = module.iam.ecs_task_execution_role_arn
  task_role_arn             = module.iam.ecs_task_role_arn
  api_target_group_arn      = module.alb.api_target_group_arn
  docs_target_group_arn     = module.alb.docs_target_group_arn
  api_image                 = var.api_image
  docs_image                = var.docs_image
  api_cpu                   = "512"
  api_memory                = "1024"
  api_desired_count         = 3
  api_min_count             = 2
  api_max_count             = 10
  docs_desired_count        = 2
  soroban_rpc_url           = var.soroban_rpc_url
  registry_id               = var.registry_id
  dividend_id               = var.dividend_id
  read_source               = var.read_source
  next_public_api_base_url  = var.next_public_api_base_url
}

# 7. CloudFront Global CDN Distribution
module "cloudfront" {
  source              = "../../modules/aws/cloudfront"
  project_name        = var.project_name
  environment         = "prod"
  alb_dns_name        = module.alb.alb_dns_name
  alb_protocol_policy = "https-only"
  waf_web_acl_arn     = module.waf.web_acl_arn
  domain_names        = var.custom_domain_names
  acm_certificate_arn = var.cloudfront_certificate_arn
  price_class         = "PriceClass_All"
}

# 8. Aurora Serverless v2 PostgreSQL & Redis
module "database" {
  source                     = "../../modules/aws/database"
  project_name               = var.project_name
  environment                = "prod"
  private_subnet_ids         = module.vpc.private_subnet_ids
  database_security_group_id = module.security_groups.database_security_group_id
  enable_postgresql          = true
  enable_redis               = true
  min_acu                    = 1.0
  max_acu                    = 16.0
  instance_count             = 2 # Multi-AZ HA
}
