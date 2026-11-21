output "cluster_id" {
  description = "ECS cluster id of the warm standby"
  value       = aws_ecs_cluster.standby.id
}

output "service_name" {
  description = "ECS service name to scale during a drill or failover"
  value       = aws_ecs_service.standby.name
}

output "standby_dns_name" {
  description = "Internal ALB DNS name to register as the Cloudflare LB standby pool"
  value       = aws_lb.standby.dns_name
}

output "target_group_arn" {
  description = "Target group used by the standby health check"
  value       = aws_lb_target_group.standby.arn
}

output "log_group" {
  description = "CloudWatch log group for standby API tasks"
  value       = aws_cloudwatch_log_group.standby.name
}
