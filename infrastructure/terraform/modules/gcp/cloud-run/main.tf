# ----------------------------------------------------
# Cloud Run: Tessera API
# ----------------------------------------------------
resource "google_cloud_run_v2_service" "api" {
  name     = "${var.project_name}-${var.environment}-api"
  location = var.gcp_region
  ingress  = var.ingress_mode # INGRESS_TRAFFIC_ALL or INGRESS_TRAFFIC_INTERNAL_LOAD_BALANCER

  template {
    service_account = var.service_account_email

    scaling {
      min_instance_count = var.api_min_instances
      max_instance_count = var.api_max_instances
    }

    dynamic "vpc_access" {
      for_each = var.vpc_connector_id != null ? [1] : []
      content {
        connector = var.vpc_connector_id
        egress    = "PRIVATE_RANGES_ONLY"
      }
    }

    containers {
      image = var.api_image

      resources {
        limits = {
          cpu    = var.api_cpu
          memory = var.api_memory
        }
      }

      ports {
        container_port = 8080
      }

      env {
        name  = "PORT"
        value = "8080"
      }
      env {
        name  = "RWA_RPC_URL"
        value = var.soroban_rpc_url
      }
      env {
        name  = "RWA_REGISTRY_ID"
        value = var.registry_id
      }
      env {
        name  = "RWA_DIVIDEND_ID"
        value = var.dividend_id
      }
      env {
        name  = "RWA_READ_SOURCE"
        value = var.read_source
      }
      env {
        name  = "RUST_LOG"
        value = "tessera_api=info,tower_http=warn"
      }

      startup_probe {
        http_get {
          path = "/health"
          port = 8080
        }
        initial_delay_seconds = 5
        period_seconds        = 10
        failure_threshold     = 3
      }

      liveness_probe {
        http_get {
          path = "/health"
          port = 8080
        }
        period_seconds    = 15
        failure_threshold = 3
      }
    }
  }
}

# ----------------------------------------------------
# Cloud Run: Tessera Docs
# ----------------------------------------------------
resource "google_cloud_run_v2_service" "docs" {
  name     = "${var.project_name}-${var.environment}-docs"
  location = var.gcp_region
  ingress  = var.ingress_mode

  template {
    service_account = var.service_account_email

    scaling {
      min_instance_count = var.docs_min_instances
      max_instance_count = var.docs_max_instances
    }

    containers {
      image = var.docs_image

      resources {
        limits = {
          cpu    = var.docs_cpu
          memory = var.docs_memory
        }
      }

      ports {
        container_port = 3000
      }

      env {
        name  = "NODE_ENV"
        value = "production"
      }
      env {
        name  = "PORT"
        value = "3000"
      }
      env {
        name  = "NEXT_PUBLIC_API_BASE_URL"
        value = var.next_public_api_base_url
      }
    }
  }
}

# IAM Public Ingress (Invoker Role)
resource "google_cloud_run_service_iam_member" "api_public" {
  count    = var.allow_unauthenticated ? 1 : 0
  project  = google_cloud_run_v2_service.api.project
  location = google_cloud_run_v2_service.api.location
  service  = google_cloud_run_v2_service.api.name
  role     = "roles/run.invoker"
  member   = "allUsers"
}

resource "google_cloud_run_service_iam_member" "docs_public" {
  count    = var.allow_unauthenticated ? 1 : 0
  project  = google_cloud_run_v2_service.docs.project
  location = google_cloud_run_v2_service.docs.location
  service  = google_cloud_run_v2_service.docs.name
  role     = "roles/run.invoker"
  member   = "allUsers"
}
