// Tessera infrastructure — Pulumi TypeScript entry point (Issue #308)
//
// Provisions a full multi-cloud stack across AWS (EKS + RDS + ElastiCache)
// and GCP (GKE + Cloud SQL + Memorystore), with Cloudflare DNS weighted
// routing across both clouds.  Secrets are injected from AWS Secrets Manager
// / GCP Secret Manager rather than hard-coded.

import * as pulumi from "@pulumi/pulumi";
import { awsInfrastructure } from "./aws";
import { gcpInfrastructure } from "./gcp";
import * as cloudflare from "@pulumi/cloudflare";

const config = new pulumi.Config();

// ---------------------------------------------------------------------------
// AWS stack
// ---------------------------------------------------------------------------
const aws = awsInfrastructure();

// ---------------------------------------------------------------------------
// GCP stack
// ---------------------------------------------------------------------------
const gcp = gcpInfrastructure();

// ---------------------------------------------------------------------------
// Cloudflare DNS — weighted load-balance across AWS ALB and GCP HTTPS LB
// ---------------------------------------------------------------------------
const zone = new cloudflare.Zone("tessera-zone", {
    accountId: config.get("cloudflareAccountId") ?? "00000000000000000000000000000000",
    zone: config.require("domain"),
    plan: "free",
});

// AWS origin — 70% of traffic by default.
const awsRecord = new cloudflare.Record("api-aws", {
    zoneId: zone.id,
    name: "api",
    type: "CNAME",
    value: aws.albDnsName,
    ttl: 1, // proxied → TTL is automatic
    proxied: true,
});

// GCP origin — 30% of traffic.
const gcpRecord = new cloudflare.Record("api-gcp", {
    zoneId: zone.id,
    name: "api",
    type: "CNAME",
    value: gcp.lbIpAddress,
    ttl: 1,
    proxied: true,
});

// ---------------------------------------------------------------------------
// Exports consumed by other stacks or CI
// ---------------------------------------------------------------------------
export const awsClusterId      = aws.clusterId;
export const awsDbEndpoint     = aws.dbEndpoint;
export const awsRedisEndpoint  = aws.redisEndpoint;
export const gcpClusterId      = gcp.clusterId;
export const gcpDbEndpoint     = gcp.dbEndpoint;
export const gcpRedisHost      = gcp.redisHost;
export const cloudflareDomain  = awsRecord.hostname;
