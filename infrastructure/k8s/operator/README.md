# Tessera Kubernetes Operator (`TesseraOperator`)

Custom Kubernetes Operator automating the deployment, scaling, database migration, and health monitoring of Tessera indexers and docs clusters.

## Features

1. **Custom Resource Definitions (CRDs)**:
   - `TesseraIndexer` (`operator.tessera.io/v1alpha1`): Manages indexer replicas, autoscaling, and database migrations.
   - `TesseraDocs` (`operator.tessera.io/v1alpha1`): Manages documentation deployment and TLS ingress routing.

2. **Automated Schema Migration Preflight**:
   - Executes pre-rollout database migration jobs (`batch/v1 Job`) prior to updating indexer container images.
   - Holds rolling container updates while migrations are running to prevent schema mismatch downtime.
   - Aborts deployments if database migrations fail.

3. **Dynamic Pod Autoscaling**:
   - Calculates target replicas based on dual metrics:
     - **CPU Utilization**: dynamically scales up when average pod CPU exceeds `targetCpuUtilizationPercentage`.
     - **WebSocket Connections**: automatically distributes incoming client streaming sessions when connections per pod exceed `targetWebSocketConnectionsPerPod`.
   - Protects against flapping with scale-up and scale-down cooldown timers.

4. **Health Probing & Observability**:
   - Exposes `/healthz`, `/readyz`, and Prometheus `/metrics` endpoints.

## Running Tests

```bash
cd infrastructure/k8s/operator
go test -v ./controllers/...
```
