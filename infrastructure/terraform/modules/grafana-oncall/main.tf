# Grafana OnCall escalation for Tessera P1 alerts (issue #474).
#
# Alertmanager sends `priority="P1"` alerts to the integration below
# (infrastructure/monitoring/alertmanager.yaml). Each alert group then walks
# this escalation chain:
#
#   0  trigger_webhook               triage bot runs the runbook's automated steps
#   1  notify_whole_channel          notify(channel): the incident Slack channel
#   2  wait 5 min
#   3  notify_on_call_from_schedule  notify(on-call engineer)
#   4  wait 15 min
#   5  notify_persons                notify(manager)
#
# Acknowledging or resolving the alert group stops the chain, so a successful
# automated fix that resolves the alert in the first five minutes never pages
# anyone.

terraform {
  required_version = ">= 1.5.0"

  required_providers {
    grafana = {
      source  = "grafana/grafana"
      version = ">= 3.7.0, < 4.0.0"
    }
  }
}

data "grafana_oncall_slack_channel" "incidents" {
  name = var.slack_channel_name
}

data "grafana_oncall_schedule" "primary" {
  name = var.oncall_schedule_name
}

data "grafana_oncall_user" "managers" {
  for_each = toset(var.manager_usernames)
  username = each.value
}

resource "grafana_oncall_outgoing_webhook" "triage_bot" {
  name                  = "${var.name_prefix}-triage-bot"
  url                   = var.triage_bot_url
  http_method           = "POST"
  trigger_type          = "escalation"
  forward_whole_payload = true
  authorization_header  = "Bearer ${var.triage_webhook_token}"
}

resource "grafana_oncall_escalation_chain" "p1" {
  name = "${var.name_prefix}-p1"
}

resource "grafana_oncall_escalation" "triage" {
  escalation_chain_id = grafana_oncall_escalation_chain.p1.id
  position            = 0
  type                = "trigger_webhook"
  action_to_trigger   = grafana_oncall_outgoing_webhook.triage_bot.id
}

resource "grafana_oncall_escalation" "notify_channel" {
  escalation_chain_id = grafana_oncall_escalation_chain.p1.id
  position            = 1
  type                = "notify_whole_channel"
}

resource "grafana_oncall_escalation" "wait_before_oncall" {
  escalation_chain_id = grafana_oncall_escalation_chain.p1.id
  position            = 2
  type                = "wait"
  duration            = var.oncall_delay_seconds
}

resource "grafana_oncall_escalation" "notify_oncall" {
  escalation_chain_id          = grafana_oncall_escalation_chain.p1.id
  position                     = 3
  type                         = "notify_on_call_from_schedule"
  notify_on_call_from_schedule = data.grafana_oncall_schedule.primary.id
  important                    = true
}

resource "grafana_oncall_escalation" "wait_before_manager" {
  escalation_chain_id = grafana_oncall_escalation_chain.p1.id
  position            = 4
  type                = "wait"
  duration            = var.manager_delay_seconds
}

resource "grafana_oncall_escalation" "notify_manager" {
  escalation_chain_id = grafana_oncall_escalation_chain.p1.id
  position            = 5
  type                = "notify_persons"
  persons_to_notify   = [for u in data.grafana_oncall_user.managers : u.id]
  important           = true
}

resource "grafana_oncall_integration" "alertmanager" {
  name = "${var.name_prefix}-alertmanager"
  type = "alertmanager"

  default_route {
    escalation_chain_id = grafana_oncall_escalation_chain.p1.id

    slack {
      channel_id = data.grafana_oncall_slack_channel.incidents.slack_id
      enabled    = true
    }
  }
}
