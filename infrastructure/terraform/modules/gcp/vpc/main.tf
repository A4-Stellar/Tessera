resource "google_compute_network" "vpc" {
  name                    = "${var.project_name}-${var.environment}-vpc"
  auto_create_subnetworks = false
}

resource "google_compute_subnetwork" "subnet" {
  name          = "${var.project_name}-${var.environment}-subnet"
  ip_cidr_range = var.subnet_cidr
  region        = var.gcp_region
  network       = google_compute_network.vpc.id
}

# Serverless VPC Access Connector (for Cloud Run to talk to private Cloud SQL / Memorystore)
resource "google_vpc_access_connector" "connector" {
  count         = var.enable_vpc_connector ? 1 : 0
  name          = "${var.project_name}-${var.environment}-conn"
  region        = var.gcp_region
  ip_cidr_range = var.connector_cidr
  network       = google_compute_network.vpc.name
  min_instances = 2
  max_instances = 3
}
