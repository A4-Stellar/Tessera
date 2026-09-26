# 🏗️ Tessera Infrastructure-as-Code (Terraform)

Production-grade Terraform modules for deploying **Tessera API** (`tessera-api`) and **Tessera Docs** (`tessera-docs`) across **AWS**, **GCP**, and **Cloudflare**.

---

## 📐 Architecture Overview

```
                        ┌─────────────────────────────────┐
                        │      Cloudflare Edge CDN        │
                        │  (Strict SSL, WAF, DNS, Cache)  │
                        └────────────────┬────────────────┘
                                         │
                   ┌─────────────────────┴─────────────────────┐
                   │                                           │
         ▼ (AWS Deployment)                          ▼ (GCP Deployment)
┌─────────────────────────────────┐         ┌─────────────────────────────────┐
│       AWS CloudFront CDN        │         │      Cloud Armor WAF Policy     │
│       + AWS WAFv2 WebACL        │         └────────────────┬────────────────┘
└────────────────┬────────────────┘                          │
                 ▼                                           ▼
┌─────────────────────────────────┐         ┌─────────────────────────────────┐
│   Application Load Balancer     │         │   HTTPS Load Balancer (NEGs)    │
│  (ACM TLS, Health Check Rules)  │         └────────────────┬────────────────┘
└────────┬───────────────┬────────┘                          │
         │               │                                   │
         ▼               ▼                                   ▼
┌─────────────────┐ ┌─────────────┐         ┌─────────────────────────────────┐
│   ECS Fargate   │ │ ECS Fargate │         │         Cloud Run v2            │
│  (tessera-api)  │ │(tessera-docs│         │ (tessera-api & tessera-docs)    │
│   [Port 8080]   │ │ [Port 3000] │         └────────────────┬────────────────┘
└────────┬────────┘ └─────────────┘                          │
         │ (Private VPC)                                     │ (VPC Connector)
         ▼                                                   ▼
┌─────────────────────────────────┐         ┌─────────────────────────────────┐
│      Aurora PostgreSQL v2       │         │       Cloud SQL PostgreSQL      │
│     + ElastiCache Redis         │         │      + Memorystore Redis        │
└─────────────────────────────────┘         └─────────────────────────────────┘
```

---

## 📂 Directory Layout

```
infrastructure/
├── README.md
├── cloudflare/                 # Cloudflare Workers edge shield (DDoS/rate-limit/JWT/cache)
│   ├── worker.js               # module worker: validation, rate limiting, edge JWT, SWR cache
│   ├── wrangler.toml           # bindings (RATE_LIMIT KV) and per-environment vars
│   ├── deploy.sh               # KV bootstrap + tests + `wrangler deploy`
│   ├── package.json
│   └── test/worker.test.js     # node:test unit suite (no dependencies)
└── terraform/
    ├── versions.tf               # Global Terraform and provider requirements
    ├── modules/
    │   ├── aws/
    │   │   ├── vpc/              # Multi-AZ VPC with public/private subnets and NAT GWs
    │   │   ├── iam/              # ECS task execution & task runtime IAM roles
    │   │   ├── security-groups/  # Least-privilege ALB, ECS, and DB security groups
    │   │   ├── alb/              # Application Load Balancer with SSL & health routes
    │   │   ├── ecs-fargate/      # ECS Cluster, Task Definitions, and Services
    │   │   ├── cloudfront/       # CloudFront Global CDN with custom cache policies
    │   │   ├── waf/              # AWS WAFv2 WebACL (Rate Limiting, OWASP Common Rules)
    │   │   └── database/         # Aurora Serverless v2 PostgreSQL & Redis Serverless
    │   ├── gcp/
    │   │   ├── vpc/              # VPC Network and Serverless VPC Access Connector
    │   │   ├── iam/              # Least-privilege Service Accounts & IAM bindings
    │   │   ├── cloud-run/        # Cloud Run v2 services for API & Docs
    │   │   ├── cloud-armor/      # Cloud Armor WAF with DDoS & OWASP protection
    │   │   └── database/         # Cloud SQL PostgreSQL & Memorystore Redis
    │   └── cloudflare/
    │       ├── main.tf           # DNS records, TLS Strict, WAF, and Caching rules
    │       ├── variables.tf
    │       └── outputs.tf
    └── environments/
        ├── dev/                  # Development environment (single NAT, cost-optimized)
        └── prod/                 # Production environment (multi-AZ HA, WAF, autoscaling)
```

---

## 🛡️ Cloudflare Edge Security Worker

`infrastructure/cloudflare/` contains a self-contained **Cloudflare Worker** that
runs in front of the API and enforces, at the edge:

- request header/method validation (allow-list forwarding + required headers),
- per-IP rate limiting with a sliding-window counter in Workers KV,
- JWT signature verification with WebCrypto (`HS*` secret or `RS*`/`ES*` JWKS),
- SQLi / path-traversal / scanner heuristics, and
- stale-while-revalidate caching for `GET /stats` and `GET /assets`.

Deploy it with `wrangler`:

```bash
cd infrastructure/cloudflare
export CLOUDFLARE_API_TOKEN=...
./deploy.sh            # creates the RATE_LIMIT KV namespace on first run, then deploys
```

See [`cloudflare/README.md`](cloudflare/README.md) for the full binding reference,
local development (`npm run dev`), and the unit-test suite (`npm test`).

---

## 🚀 Deployment Instructions

### Prerequisites

1. Install [Terraform](https://developer.hashicorp.com/terraform/install) `>= 1.5.0`
2. Configure Cloud Provider Credentials:
   - AWS: `aws configure` (or `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`)
   - GCP: `gcloud auth application-default login`
   - Cloudflare: `export CLOUDFLARE_API_TOKEN="<token>"`

---

### Deploying AWS (Production)

1. Navigate to the production environment:
   ```bash
   cd infrastructure/terraform/environments/prod
   ```

2. Copy and configure variables:
   ```bash
   cp terraform.tfvars.example terraform.tfvars
   # Fill in ECR image URLs, domain names, and contract IDs
   ```

3. Initialize Terraform providers and backend:
   ```bash
   terraform init
   ```

4. Preview infrastructure plan:
   ```bash
   terraform plan -out=tfplan
   ```

5. Apply infrastructure:
   ```bash
   terraform apply tfplan
   ```

---

### Deploying AWS (Development)

```bash
cd infrastructure/terraform/environments/dev
cp terraform.tfvars.example terraform.tfvars
terraform init
terraform apply
```

---

## 🔒 Security Best Practices Implemented

1. **Network Isolation**: All ECS tasks and database instances are provisioned in **private subnets** with no direct public IPs.
2. **Layer 7 Protection (WAF)**:
   - Automated IP rate limiting to mitigate denial-of-service attempts.
   - Managed OWASP rulesets blocking SQLi, XSS, and scanner attacks.
3. **Least-Privilege IAM**:
   - Execution role only holds permissions for pulling images and logging.
   - Application tasks run under dedicated non-root roles.
4. **Encryption Everywhere**:
   - TLS 1.3 enforced at CloudFront, Cloudflare, and ALB layers.
   - Storage encryption at rest with KMS keys for Aurora PostgreSQL and EBS.
