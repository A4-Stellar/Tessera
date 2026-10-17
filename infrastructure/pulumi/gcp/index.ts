// GCP cloud resources for Tessera (GKE, Cloud SQL, Memorystore) — Issue #308

import * as pulumi from "@pulumi/pulumi";
import * as gcp from "@pulumi/gcp";

export interface GcpOutputs {
    clusterId: pulumi.Output<string>;
    lbIpAddress: pulumi.Output<string>;
    dbEndpoint: pulumi.Output<string>;
    redisHost: pulumi.Output<string>;
}

export function gcpInfrastructure(): GcpOutputs {
    const config = new pulumi.Config("gcp");
    const project = config.require("project");
    const region  = config.get("region") ?? "us-central1";
    const appConfig = new pulumi.Config();

    // -----------------------------------------------------------------------
    // VPC
    // -----------------------------------------------------------------------
    const network = new gcp.compute.Network("tessera-vpc", {
        project,
        autoCreateSubnetworks: false,
    });

    const subnet = new gcp.compute.Subnetwork("tessera-subnet", {
        project,
        region,
        network: network.selfLink,
        ipCidrRange: "10.1.0.0/24",
        secondaryIpRanges: [
            { rangeName: "pods",     ipCidrRange: "10.2.0.0/16" },
            { rangeName: "services", ipCidrRange: "10.3.0.0/16" },
        ],
    });

    // -----------------------------------------------------------------------
    // GKE (Autopilot — no node management needed)
    // -----------------------------------------------------------------------
    const cluster = new gcp.container.Cluster("tessera-gke", {
        project,
        location: region,
        network: network.name,
        subnetwork: subnet.name,
        enableAutopilot: true,
        ipAllocationPolicy: {
            clusterSecondaryRangeName:  "pods",
            servicesSecondaryRangeName: "services",
        },
        privateClusterConfig: {
            enablePrivateNodes:   true,
            enablePrivateEndpoint: false,
            masterIpv4CidrBlock:  "172.16.0.0/28",
        },
    });

    // -----------------------------------------------------------------------
    // Cloud SQL — PostgreSQL 15
    // -----------------------------------------------------------------------
    const dbInstance = new gcp.sql.DatabaseInstance("tessera-pg", {
        project,
        region,
        databaseVersion: "POSTGRES_15",
        settings: {
            tier: "db-g1-small",
            diskAutoresize: true,
            ipConfiguration: {
                ipv4Enabled: false,
                privateNetwork: network.selfLink,
            },
            backupConfiguration: {
                enabled: true,
                startTime: "02:00",
            },
        },
        deletionProtection: false,
    });

    new gcp.sql.Database("tessera-db", {
        project,
        instance: dbInstance.name,
        name: "tessera",
    });

    new gcp.sql.User("tessera-admin", {
        project,
        instance: dbInstance.name,
        name: "tessera_admin",
        password: appConfig.requireSecret("gcpDbPassword"),
    });

    // -----------------------------------------------------------------------
    // Memorystore Redis
    // -----------------------------------------------------------------------
    const redis = new gcp.redis.Instance("tessera-redis", {
        project,
        region,
        tier: "STANDARD_HA",
        memorySizeGb: 1,
        authorizedNetwork: network.selfLink,
        connectMode: "PRIVATE_SERVICE_ACCESS",
        redisVersion: "REDIS_7_0",
        displayName: "tessera-redis",
    });

    // -----------------------------------------------------------------------
    // Global HTTPS Load Balancer — reserved static IP for Cloudflare CNAME
    // -----------------------------------------------------------------------
    const staticIp = new gcp.compute.GlobalAddress("tessera-lb-ip", { project });

    return {
        clusterId: cluster.id,
        lbIpAddress: staticIp.address,
        dbEndpoint: dbInstance.privateIpAddress,
        redisHost: redis.host,
    };
}
