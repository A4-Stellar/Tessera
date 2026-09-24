resource "google_compute_security_policy" "cloud_armor" {
  name        = "${var.project_name}-${var.environment}-armor-policy"
  description = "Cloud Armor WAF security policy with DDoS and OWASP protection"

  adaptive_protection_config {
    layer_7_ddos_defense_config {
      enable = var.enable_adaptive_protection
    }
  }

  # Default rule: Allow traffic
  rule {
    action   = "allow"
    priority = "2147483647"
    match {
      versioned_expr = "SRC_IPS_V1"
      config {
        src_ip_ranges = ["*"]
      }
    }
    description = "Default allow rule"
  }

  # Rule 1: IP Rate Limiting (Throttle after threshold)
  rule {
    action   = "throttle"
    priority = "1000"
    match {
      versioned_expr = "SRC_IPS_V1"
      config {
        src_ip_ranges = ["*"]
      }
    }
    rate_limit_options {
      conform_action = "allow"
      exceed_action  = "deny(429)"
      enforce_on_key = "IP"
      rate_limit_threshold {
        count        = var.rate_limit_count
        interval_sec = 60
      }
    }
    description = "Rate limiting per client IP"
  }

  # Rule 2: SQL Injection Protection (OWASP)
  rule {
    action   = "deny(403)"
    priority = "2000"
    match {
      expr {
        expression = "evaluatePreconfiguredExpr('sqli-v33-stable')"
      }
    }
    description = "OWASP SQL Injection filter"
  }

  # Rule 3: Cross-Site Scripting (XSS) Protection
  rule {
    action   = "deny(403)"
    priority = "3000"
    match {
      expr {
        expression = "evaluatePreconfiguredExpr('xss-v33-stable')"
      }
    }
    description = "OWASP Cross-Site Scripting filter"
  }

  # Rule 4: Protocol Attack & Scanner Detection
  rule {
    action   = "deny(403)"
    priority = "4000"
    match {
      expr {
        expression = "evaluatePreconfiguredExpr('scannerdetection-v33-stable') || evaluatePreconfiguredExpr('protocolattack-v33-stable')"
      }
    }
    description = "Scanner and protocol attack detection"
  }
}
