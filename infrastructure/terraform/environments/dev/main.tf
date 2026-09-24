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
      Environment = "dev"
      ManagedBy   = "Terraform"
    }
  }
}

# 1. VPC (Single NAT Gateway for cost optimization in dev)
module "vpc" {
  source               = "../../modules/aws/vpc"
  project_name         = var.project_name
  environment          = "dev"
  vpc_cidr             = "10.0.0.0/16"
  availability_zones   = var.availability_zones
  public_subnet_cidrs  = ["10.0.1.0/24", "10.0.2.0/24"]
  private_subnet_cidrs = ["10.0.10.0/24", "10.0.20.0/24"]
  single_nat_gateway   = true
}

# 2. IAM Roles
module "iam" {
  source       = "../../modules/aws/iam"
  project_name = var.project_name
  environment  = "dev"
}

# 3. Security Groups
module "security_groups" {
  source       = "../../modules/aws/security-groups"
  project_name = var.project_name
  environment  = "dev"
  vpc_id       = module.vpc.vpc_id
  vpc_cidr     = module.vpc.vpc_cidr_block
}

# 4. Application Load Balancer
module "alb" {
  source                = "../../modules/aws/alb"
  project_name          = var.project_name
  environment           = "dev"
  vpc_id                = module.vpc.vpc_id
  public_subnet_ids     = module.vpc.public_subnet_ids
  alb_security_group_id = module.security_groups.alb_security_group_id
  certificate_arn       = var.certificate_arn
}

# 5. ECS Fargate Cluster & Services
module "ecs" {
  source                    = "../../modules/aws/ecs-fargate"
  project_name              = var.project_name
  environment               = "dev"
  aws_region                = var.aws_region
  private_subnet_ids        = module.vpc.private_subnet_ids
  ecs_security_group_id     = module.security_groups.ecs_security_group_id
  execution_role_arn        = module.iam.ecs_task_execution_role_arn
  task_role_arn             = module.iam.ecs_task_role_arn
  api_target_group_arn      = module.alb.api_target_group_arn
  docs_target_group_arn     = module.alb.docs_target_group_arn
  api_image                 = var.api_image
  docs_image                = var.docs_image
  api_desired_count         = 1
  api_min_count             = 1
  api_max_count             = 2
  docs_desired_count        = 1
  soroban_rpc_url           = var.soroban_rpc_url
  registry_id               = var.registry_id
  dividend_id               = var.dividend_id
  read_source               = var.read_source
  next_public_api_base_url  = var.next_public_api_base_url
}
