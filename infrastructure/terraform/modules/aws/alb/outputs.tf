output "alb_id" {
  description = "ID of the ALB"
  value       = aws_lb.main.id
}

output "alb_arn" {
  description = "ARN of the ALB"
  value       = aws_lb.main.arn
}

output "alb_dns_name" {
  description = "DNS name of the ALB"
  value       = aws_lb.main.dns_name
}

output "alb_zone_id" {
  description = "Canonical hosted zone ID of the ALB"
  value       = aws_lb.main.zone_id
}

output "api_target_group_arn" {
  description = "ARN of the API Target Group"
  value       = aws_lb_target_group.api.arn
}

output "docs_target_group_arn" {
  description = "ARN of the Docs Target Group"
  value       = aws_lb_target_group.docs.arn
}
