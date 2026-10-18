# Tessera Pulumi Infrastructure as Code (TypeScript)

Cloud-native multi-cloud deployment scripts for **Tessera** using [Pulumi](https://www.pulumi.com/) and TypeScript.

This stack provisions production-grade infrastructure across **AWS** and **GCP**, with **Cloudflare** weighted global DNS traffic distribution and automated secret management via AWS Secrets Manager and GCP Secret Manager.

---

## Architecture Overview

```
                        ┌─────────────────────────────────┐
                        │      Cloudflare Edge DNS        │
                        │    (Weighted CNAME Routing)     │
                        └───────────────┬─────────────────┘
                                        │
                 ┌──────────────────────┴──────────────────────┐
                 │ (70% Traffic)                               │ (30% Traffic)
                 ▼                                             ▼
       ┌───────────────────┐                         ┌───────────────────┐
       │     AWS Origin    │                         │     GCP Origin    │
       │  Application LB   │                         │  HTTPS Load Bal.  │
       └─────────┬─────────┘                         └─────────┬─────────┘
                 ▼                                             ▼
       ┌───────────────────┐                         ┌───────────────────┐
       │   AWS EKS Cluster │                         │   GCP GKE Cluster │
       │ (Tessera Pods)    │                         │  (Autopilot Pods) │
       └─────────┬─────────┘                         └─────────┬─────────┘
                 ▼                                             ▼
  ┌─────────────────────────────┐               ┌─────────────────────────────┐
  │ - Aurora PostgreSQL v2      │               │ - Cloud SQL PostgreSQL 15   │
  │ - ElastiCache Serverless    │               │ - Memorystore Redis         │
  │ - AWS Secrets Manager       │               │ - GCP Secret Manager        │
  └─────────────────────────────┘               └─────────────────────────────┘
```

---

## Provisioned Resources

### 1. AWS Stack (`infrastructure/pulumi/aws/index.ts`)
- **Networking**: Custom VPC (`10.0.0.0/16`), 2 public subnets (`10.0.1.0/24`, `10.0.2.0/24`), 2 private subnets (`10.0.10.0/24`, `10.0.11.0/24`).
- **Compute**: Managed AWS EKS Kubernetes Cluster with OIDC authentication provider.
- **Database**: Amazon Aurora PostgreSQL Serverless v2 cluster with automated backups and encryption.
- **Cache**: Amazon ElastiCache Serverless Redis cluster.
- **Secrets Management**: AWS Secrets Manager storing database credentials (`tessera/db/credentials`) and API signing keys.
- **Load Balancing**: AWS Application Load Balancer (ALB) with health checks.

### 2. GCP Stack (`infrastructure/pulumi/gcp/index.ts`)
- **Networking**: Custom VPC network with secondary IP ranges for pods (`10.2.0.0/16`) and services (`10.3.0.0/16`).
- **Compute**: GCP GKE Autopilot cluster with private node endpoint configuration.
- **Database**: Cloud SQL PostgreSQL 15 instance with automatic storage increase.
- **Cache**: Cloud Memorystore Redis instance.
- **Secrets Management**: GCP Secret Manager storing DB secrets and Soroban RPC credentials.
- **Load Balancing**: Global external HTTPS Load Balancer with static IP allocation.

### 3. Cloudflare DNS (`infrastructure/pulumi/index.ts`)
- Managed DNS zone with weighted proxied CNAME records directing global client traffic to AWS ALB and GCP HTTPS LB.

---

## Prerequisites

1. **Pulumi CLI**: Install from [pulumi.com](https://www.pulumi.com/docs/install/)
2. **Cloud Credentials**:
   - AWS: `aws configure` (or `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`)
   - GCP: `gcloud auth application-default login` (or `GOOGLE_CREDENTIALS`)
   - Cloudflare: `CLOUDFLARE_API_TOKEN`
3. **Node.js**: v18+

---

## Quickstart: Zero-Manual-Step Deployment

### 1. Install Dependencies & Build
```bash
cd infrastructure/pulumi
npm install
npm run build
```

### 2. Configure Stack & Secrets
Create and select your Pulumi stack:
```bash
pulumi stack init prod
```

Configure regions and domain:
```bash
pulumi config set aws:region us-east-1
pulumi config set gcp:project my-gcp-project-id
pulumi config set gcp:region us-central1
pulumi config set domain tessera.finance
pulumi config set cloudflareAccountId <your-cloudflare-account-id>
```

Set required secrets (automatically encrypted by Pulumi state KMS):
```bash
pulumi config set --secret dbPassword "SuperSecurePassword123!"
pulumi config set --secret cloudflare:apiToken "<your-cloudflare-token>"
```

### 3. Deploy (`pulumi up`)
Execute zero-manual-step automated provisioning:
```bash
pulumi up --yes
```

Upon completion, Pulumi exports cluster IDs, DB endpoints, and Redis hosts:
- `awsClusterId`
- `awsDbEndpoint`
- `awsRedisEndpoint`
- `gcpClusterId`
- `gcpDbEndpoint`
- `gcpRedisHost`
- `cloudflareDomain`

---

## Teardown: Clean Deletion (`pulumi destroy`)

To cleanly decommission all provisioned cloud resources across AWS, GCP, and Cloudflare with zero lingering costs:

```bash
pulumi destroy --yes
```

To remove the stack state:
```bash
pulumi stack rm prod --yes
```
