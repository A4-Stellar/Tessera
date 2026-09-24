# DNS Records
resource "cloudflare_record" "api" {
  zone_id = var.cloudflare_zone_id
  name    = var.api_subdomain
  value   = var.origin_target
  type    = "CNAME"
  proxied = true
  ttl     = 1
}

resource "cloudflare_record" "docs" {
  zone_id = var.cloudflare_zone_id
  name    = var.docs_subdomain
  value   = var.origin_target
  type    = "CNAME"
  proxied = true
  ttl     = 1
}

# SSL / TLS Settings
resource "cloudflare_zone_settings_override" "settings" {
  zone_id = var.cloudflare_zone_id

  settings {
    ssl                      = "strict"
    always_use_https         = "on"
    min_tls_version          = "1.2"
    tls_1_3                  = "on"
    automatic_https_rewrites = "on"
    brotli                   = "on"
    http3                    = "on"

    security_header {
      enabled            = true
      preload            = true
      max_age            = 31536000
      include_subdomains = true
      nosniff            = true
    }
  }
}

# Rate Limiting Ruleset (Cloudflare Ruleset Engine)
resource "cloudflare_ruleset" "api_rate_limit" {
  zone_id     = var.cloudflare_zone_id
  name        = "${var.api_subdomain}-rate-limit-ruleset"
  description = "Rate limiting for Tessera API"
  kind        = "zone"
  phase       = "http_ratelimit"

  rules {
    action      = "block"
    description = "Block excessive requests to API"
    enabled     = true
    expression  = "(http.host eq \"${var.api_subdomain}.${var.domain_name}\")"

    ratelimit {
      characteristics     = ["cf.colo.id", "ip.src"]
      period              = 60
      requests_per_period = var.rate_limit_threshold
      mitigation_timeout  = 60
      requests_to_origin  = true
    }
  }
}

# Page Rules / Cache Rules: Bypass Cache for API
resource "cloudflare_page_rule" "api_cache_bypass" {
  zone_id  = var.cloudflare_zone_id
  target   = "${var.api_subdomain}.${var.domain_name}/*"
  priority = 1

  actions {
    cache_level         = "bypass"
    disable_performance = false
  }
}

# Page Rules: Cache Static Docs Assets
resource "cloudflare_page_rule" "docs_cache" {
  zone_id  = var.cloudflare_zone_id
  target   = "${var.docs_subdomain}.${var.domain_name}/_next/static/*"
  priority = 2

  actions {
    cache_level    = "cache_everything"
    edge_cache_ttl = 2592000 # 30 days
  }
}
