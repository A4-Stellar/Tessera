// AWS cloud resources for Tessera (EKS, RDS Aurora, ElastiCache) — Issue #308

import * as pulumi from "@pulumi/pulumi";
import * as aws from "@pulumi/aws";

export interface AwsOutputs {
    clusterId: pulumi.Output<string>;
    albDnsName: pulumi.Output<string>;
    dbEndpoint: pulumi.Output<string>;
    redisEndpoint: pulumi.Output<string>;
}

export function awsInfrastructure(): AwsOutputs {
    const config = new pulumi.Config("aws");
    const region = config.get("region") ?? "us-east-1";
    const appConfig = new pulumi.Config();

    // -----------------------------------------------------------------------
    // VPC
    // -----------------------------------------------------------------------
    const vpc = new aws.ec2.Vpc("tessera-vpc", {
        cidrBlock: "10.0.0.0/16",
        enableDnsHostnames: true,
        enableDnsSupport: true,
        tags: { Name: "tessera-vpc" },
    });

    const publicSubnet1 = new aws.ec2.Subnet("public-1", {
        vpcId: vpc.id,
        cidrBlock: "10.0.1.0/24",
        availabilityZone: `${region}a`,
        mapPublicIpOnLaunch: true,
        tags: { Name: "tessera-public-1" },
    });

    const publicSubnet2 = new aws.ec2.Subnet("public-2", {
        vpcId: vpc.id,
        cidrBlock: "10.0.2.0/24",
        availabilityZone: `${region}b`,
        mapPublicIpOnLaunch: true,
        tags: { Name: "tessera-public-2" },
    });

    const privateSubnet1 = new aws.ec2.Subnet("private-1", {
        vpcId: vpc.id,
        cidrBlock: "10.0.10.0/24",
        availabilityZone: `${region}a`,
        tags: { Name: "tessera-private-1" },
    });

    const privateSubnet2 = new aws.ec2.Subnet("private-2", {
        vpcId: vpc.id,
        cidrBlock: "10.0.11.0/24",
        availabilityZone: `${region}b`,
        tags: { Name: "tessera-private-2" },
    });

    // -----------------------------------------------------------------------
    // EKS cluster
    // -----------------------------------------------------------------------
    const clusterRole = new aws.iam.Role("eks-cluster-role", {
        assumeRolePolicy: aws.iam.assumeRolePolicyForPrincipal({
            Service: "eks.amazonaws.com",
        }),
    });

    new aws.iam.RolePolicyAttachment("eks-cluster-policy", {
        role: clusterRole.name,
        policyArn: aws.iam.ManagedPolicy.AmazonEKSClusterPolicy,
    });

    const cluster = new aws.eks.Cluster("tessera-eks", {
        roleArn: clusterRole.arn,
        vpcConfig: {
            subnetIds: [privateSubnet1.id, privateSubnet2.id],
            endpointPrivateAccess: true,
            endpointPublicAccess: false,
        },
        tags: { Name: "tessera-eks" },
    });

    // -----------------------------------------------------------------------
    // RDS Aurora PostgreSQL (Serverless v2)
    // -----------------------------------------------------------------------
    const dbSubnetGroup = new aws.rds.SubnetGroup("tessera-db-subnets", {
        subnetIds: [privateSubnet1.id, privateSubnet2.id],
        tags: { Name: "tessera-db-subnet-group" },
    });

    const dbSg = new aws.ec2.SecurityGroup("rds-sg", {
        vpcId: vpc.id,
        ingress: [{
            fromPort: 5432,
            toPort: 5432,
            protocol: "tcp",
            cidrBlocks: ["10.0.0.0/16"],
        }],
        egress: [{ fromPort: 0, toPort: 0, protocol: "-1", cidrBlocks: ["0.0.0.0/0"] }],
    });

    const dbCluster = new aws.rds.Cluster("tessera-aurora", {
        engine: aws.rds.EngineType.AuroraPostgresql,
        engineMode: "provisioned",
        engineVersion: "15.4",
        databaseName: "tessera",
        masterUsername: "tessera_admin",
        masterPassword: appConfig.requireSecret("dbPassword"),
        dbSubnetGroupName: dbSubnetGroup.name,
        vpcSecurityGroupIds: [dbSg.id],
        storageEncrypted: true,
        serverlessv2ScalingConfiguration: {
            minCapacity: 0.5,
            maxCapacity: 16,
        },
        tags: { Name: "tessera-aurora" },
    });

    // -----------------------------------------------------------------------
    // ElastiCache Redis (Serverless)
    // -----------------------------------------------------------------------
    const redisSg = new aws.ec2.SecurityGroup("redis-sg", {
        vpcId: vpc.id,
        ingress: [{
            fromPort: 6379,
            toPort: 6379,
            protocol: "tcp",
            cidrBlocks: ["10.0.0.0/16"],
        }],
        egress: [{ fromPort: 0, toPort: 0, protocol: "-1", cidrBlocks: ["0.0.0.0/0"] }],
    });

    const redisServerless = new aws.elasticache.ServerlessCache("tessera-redis", {
        name: "tessera-redis",
        engine: "redis",
        subnetIds: [privateSubnet1.id, privateSubnet2.id],
        securityGroupIds: [redisSg.id],
        tags: { Name: "tessera-redis" },
    });

    // -----------------------------------------------------------------------
    // ALB (public-facing, forwards to EKS node-port)
    // -----------------------------------------------------------------------
    const albSg = new aws.ec2.SecurityGroup("alb-sg", {
        vpcId: vpc.id,
        ingress: [
            { fromPort: 443, toPort: 443, protocol: "tcp", cidrBlocks: ["0.0.0.0/0"] },
            { fromPort: 80, toPort: 80, protocol: "tcp", cidrBlocks: ["0.0.0.0/0"] },
        ],
        egress: [{ fromPort: 0, toPort: 0, protocol: "-1", cidrBlocks: ["0.0.0.0/0"] }],
    });

    const alb = new aws.lb.LoadBalancer("tessera-alb", {
        internal: false,
        loadBalancerType: "application",
        securityGroups: [albSg.id],
        subnets: [publicSubnet1.id, publicSubnet2.id],
        tags: { Name: "tessera-alb" },
    });

    return {
        clusterId: cluster.id,
        albDnsName: alb.dnsName,
        dbEndpoint: dbCluster.endpoint,
        redisEndpoint: redisServerless.endpoints.apply((endpoints): string => endpoints?.[0]?.address ?? ""),
    };
}
