variable "name_prefix" {
  type        = string
  description = "Prefix for OnCall object names, e.g. tessera-prod"
}

variable "slack_channel_name" {
  type        = string
  description = "Slack channel that receives the alert group and the triage bot report (without #)"
  default     = "tessera-incidents"
}

variable "oncall_schedule_name" {
  type        = string
  description = "Grafana OnCall schedule whose current on-call engineer is paged"
}

variable "manager_usernames" {
  type        = list(string)
  description = "Grafana usernames notified when the on-call engineer has not acknowledged"

  validation {
    condition     = length(var.manager_usernames) > 0
    error_message = "At least one manager must be notified at the last escalation step."
  }
}

variable "triage_bot_url" {
  type        = string
  description = "Triage bot endpoint, e.g. https://triage.internal.tessera.xyz/webhook/oncall"

  validation {
    condition     = can(regex("^https://.+/webhook/oncall$", var.triage_bot_url))
    error_message = "triage_bot_url must be an https URL ending in /webhook/oncall."
  }
}

variable "triage_webhook_token" {
  type        = string
  description = "Bearer token shared with the triage bot (TRIAGE_WEBHOOK_TOKEN)"
  sensitive   = true

  validation {
    condition     = length(var.triage_webhook_token) >= 16
    error_message = "triage_webhook_token must be at least 16 characters."
  }
}

variable "oncall_delay_seconds" {
  type        = number
  description = "Wait between notifying the channel and paging the on-call engineer"
  default     = 300

  validation {
    # Durations Grafana OnCall accepts for a wait step.
    condition     = contains([60, 300, 900, 1800, 3600], var.oncall_delay_seconds)
    error_message = "oncall_delay_seconds must be one of 60, 300, 900, 1800, 3600."
  }
}

variable "manager_delay_seconds" {
  type        = number
  description = "Wait between paging the on-call engineer and notifying the manager"
  default     = 900

  validation {
    condition     = contains([60, 300, 900, 1800, 3600], var.manager_delay_seconds)
    error_message = "manager_delay_seconds must be one of 60, 300, 900, 1800, 3600."
  }
}
