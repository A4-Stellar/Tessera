package controllers

import (
	"context"
	"fmt"
	"math"
	"time"

	"github.com/A4-Stellar/Tessera/infrastructure/k8s/operator/api/v1alpha1"
)

// ReconcileResult signals reconciliation follow-up action.
type ReconcileResult struct {
	Requeue            bool
	RequeueAfter       time.Duration
	BlockRollingUpdate bool
}

// ClusterMetrics reports live CPU, WebSocket connection, and RPC latency observations.
type ClusterMetrics struct {
	AverageCPUUtilization      int32
	ActiveWebSocketConnections int32
	RPCLatencyMillis           int32
}

// JobState represents the execution status of a Kubernetes schema migration job.
type JobState string

const (
	JobPending   JobState = "Pending"
	JobRunning   JobState = "Running"
	JobSucceeded JobState = "Succeeded"
	JobFailed    JobState = "Failed"
)

// K8sClient abstracts cluster resource operations for unit testing and controller runtime.
type K8sClient interface {
	GetMigrationJobStatus(ctx context.Context, namespace, jobName string) (JobState, error)
	CreateMigrationJob(ctx context.Context, namespace string, spec v1alpha1.DatabaseMigrationSpec) error
	GetDeploymentReplicas(ctx context.Context, namespace, name string) (int32, int32, error)
	UpdateDeploymentReplicas(ctx context.Context, namespace, name string, replicas int32) error
	UpdateDeploymentImage(ctx context.Context, namespace, name string, image string) error
	GetClusterMetrics(ctx context.Context, namespace, name string) (ClusterMetrics, error)
}

// IndexerReconciler manages the lifecycle of TesseraIndexer clusters.
type IndexerReconciler struct {
	Client K8sClient
}

// NewIndexerReconciler constructs a reconciler.
func NewIndexerReconciler(client K8sClient) *IndexerReconciler {
	return &IndexerReconciler{Client: client}
}

// Reconcile executes the state reconciliation loop for TesseraIndexer.
func (r *IndexerReconciler) Reconcile(ctx context.Context, indexer *v1alpha1.TesseraIndexer) (ReconcileResult, error) {
	now := time.Now().UTC()

	// 1. Preflight Database Schema Migration Check
	migration := indexer.Spec.DatabaseMigration
	if migration.Enabled && migration.SchemaHash != "" {
		if indexer.Status.LastMigrationHash != migration.SchemaHash {
			jobName := fmt.Sprintf("%s-migrate-%s", indexer.ObjectMeta.Name, migration.SchemaHash)
			status, err := r.Client.GetMigrationJobStatus(ctx, indexer.ObjectMeta.Namespace, jobName)
			if err != nil {
				// Job does not exist yet; create it
				if err := r.Client.CreateMigrationJob(ctx, indexer.ObjectMeta.Namespace, migration); err != nil {
					indexer.Status.Phase = v1alpha1.PhaseFailed
					r.setCondition(indexer, "MigrationJobFailed", "False", "Failed to launch migration job: "+err.Error())
					return ReconcileResult{Requeue: true}, err
				}
				indexer.Status.Phase = v1alpha1.PhaseMigrating
				r.setCondition(indexer, "DatabaseMigrationPending", "True", "Migration job initiated; rolling update held")
				return ReconcileResult{RequeueAfter: 5 * time.Second, BlockRollingUpdate: true}, nil
			}

			switch status {
			case JobRunning, JobPending:
				indexer.Status.Phase = v1alpha1.PhaseMigrating
				r.setCondition(indexer, "DatabaseMigrationPending", "True", "Schema migration in progress")
				// CRITICAL: Block rolling updates until migration completes
				return ReconcileResult{RequeueAfter: 5 * time.Second, BlockRollingUpdate: true}, nil
			case JobFailed:
				indexer.Status.Phase = v1alpha1.PhaseFailed
				r.setCondition(indexer, "DatabaseMigrationFailed", "True", "Preflight schema migration job failed; update aborted")
				return ReconcileResult{BlockRollingUpdate: true}, fmt.Errorf("migration job %s failed", jobName)
			case JobSucceeded:
				// Mark migration completed; unblock rolling update
				indexer.Status.LastMigrationHash = migration.SchemaHash
				indexer.Status.LastMigrationTime = &now
				r.setCondition(indexer, "DatabaseMigrationSucceeded", "True", "Schema migration completed successfully")
			}
		}
	}

	// 2. Deployment Image & Rolling Update Reconciliation
	if err := r.Client.UpdateDeploymentImage(ctx, indexer.ObjectMeta.Namespace, indexer.ObjectMeta.Name, indexer.Spec.Image); err != nil {
		return ReconcileResult{Requeue: true}, err
	}

	// 3. Collect Cluster Metrics & Evaluate Autoscaling
	metrics, err := r.Client.GetClusterMetrics(ctx, indexer.ObjectMeta.Namespace, indexer.ObjectMeta.Name)
	if err == nil {
		indexer.Status.AverageCPUUtilization = metrics.AverageCPUUtilization
		indexer.Status.ActiveWebSocketConnections = metrics.ActiveWebSocketConnections
	}

	currentReplicas, readyReplicas, err := r.Client.GetDeploymentReplicas(ctx, indexer.ObjectMeta.Namespace, indexer.ObjectMeta.Name)
	if err != nil {
		return ReconcileResult{Requeue: true}, err
	}
	indexer.Status.CurrentReplicas = currentReplicas
	indexer.Status.ReadyReplicas = readyReplicas

	// 4. Compute Autoscaling Decisions
	autoScale := indexer.Spec.AutoScaling
	desiredReplicas := indexer.Spec.Replicas

	if autoScale.Enabled && currentReplicas > 0 {
		desiredReplicas = r.ComputeDesiredReplicas(indexer, currentReplicas, metrics)
		if desiredReplicas != currentReplicas {
			cooldownExpired := r.checkCooldown(indexer, desiredReplicas, currentReplicas, now)
			if cooldownExpired {
				if err := r.Client.UpdateDeploymentReplicas(ctx, indexer.ObjectMeta.Namespace, indexer.ObjectMeta.Name, desiredReplicas); err != nil {
					return ReconcileResult{Requeue: true}, err
				}
				indexer.Status.Phase = v1alpha1.PhaseScaling
				indexer.Status.CurrentReplicas = desiredReplicas
				indexer.Status.LastScaleTime = &now
				r.setCondition(indexer, "Autoscaled", "True", fmt.Sprintf("Scaled replicas from %d to %d (CPU: %d%%, WS: %d, RPC: %dms)",
					currentReplicas, desiredReplicas, metrics.AverageCPUUtilization, metrics.ActiveWebSocketConnections, metrics.RPCLatencyMillis))
				return ReconcileResult{RequeueAfter: 15 * time.Second}, nil
			}
		}
	}

	// 5. Cluster Health & Phase Update
	if readyReplicas >= desiredReplicas && desiredReplicas > 0 {
		indexer.Status.Phase = v1alpha1.PhaseRunning
		r.setCondition(indexer, "ClusterHealthy", "True", "All indexer replicas are ready and healthy")
	} else {
		indexer.Status.Phase = v1alpha1.PhaseDeploying
		r.setCondition(indexer, "ClusterDeploying", "True", fmt.Sprintf("Waiting for ready replicas (%d/%d)", readyReplicas, desiredReplicas))
	}

	return ReconcileResult{RequeueAfter: 30 * time.Second}, nil
}

// ComputeDesiredReplicas calculates optimal replica counts from WebSocket client connections, RPC latency, and CPU utilization.
func (r *IndexerReconciler) ComputeDesiredReplicas(
	indexer *v1alpha1.TesseraIndexer,
	currentReplicas int32,
	metrics ClusterMetrics,
) int32 {
	autoScale := indexer.Spec.AutoScaling
	minReplicas := indexer.Spec.MinReplicas
	maxReplicas := indexer.Spec.MaxReplicas
	if minReplicas <= 0 {
		minReplicas = 1
	}
	if maxReplicas < minReplicas {
		maxReplicas = minReplicas
	}

	// Replica demand from CPU Utilization:
	// desiredCPU = ceil(currentReplicas * (currentCPU / targetCPU))
	var cpuDemand int32 = currentReplicas
	if autoScale.TargetCPUUtilizationPercentage > 0 && metrics.AverageCPUUtilization > 0 {
		ratio := float64(metrics.AverageCPUUtilization) / float64(autoScale.TargetCPUUtilizationPercentage)
		cpuDemand = int32(math.Ceil(float64(currentReplicas) * ratio))
	}

	// Replica demand from WebSocket Client Connections:
	// desiredWS = ceil(activeWsConnections / targetWsPerPod)
	var wsDemand int32 = currentReplicas
	if autoScale.TargetWebSocketConnectionsPerPod > 0 && metrics.ActiveWebSocketConnections > 0 {
		wsDemand = int32(math.Ceil(float64(metrics.ActiveWebSocketConnections) / float64(autoScale.TargetWebSocketConnectionsPerPod)))
	}

	// Replica demand from Soroban RPC Latency:
	// desiredRPC = ceil(currentReplicas * (currentRPCLatency / targetRPCLatency))
	var rpcDemand int32 = currentReplicas
	if autoScale.TargetRPCLatencyMillis > 0 && metrics.RPCLatencyMillis > 0 {
		ratio := float64(metrics.RPCLatencyMillis) / float64(autoScale.TargetRPCLatencyMillis)
		rpcDemand = int32(math.Ceil(float64(currentReplicas) * ratio))
	}

	// Take the maximum demand to satisfy CPU, WebSocket capacity, and RPC latency
	desired := cpuDemand
	if wsDemand > desired {
		desired = wsDemand
	}
	if rpcDemand > desired {
		desired = rpcDemand
	}

	// Clamp within [minReplicas, maxReplicas]
	if desired < minReplicas {
		desired = minReplicas
	}
	if desired > maxReplicas {
		desired = maxReplicas
	}

	return desired
}

func (r *IndexerReconciler) checkCooldown(
	indexer *v1alpha1.TesseraIndexer,
	desiredReplicas, currentReplicas int32,
	now time.Time,
) bool {
	if indexer.Status.LastScaleTime == nil {
		return true
	}
	elapsed := now.Sub(*indexer.Status.LastScaleTime)
	autoScale := indexer.Spec.AutoScaling

	if desiredReplicas > currentReplicas {
		// Scaling up
		cooldown := time.Duration(autoScale.ScaleUpCooldownSeconds) * time.Second
		if cooldown <= 0 {
			cooldown = 15 * time.Second
		}
		return elapsed >= cooldown
	}

	// Scaling down
	cooldown := time.Duration(autoScale.ScaleDownCooldownSeconds) * time.Second
	if cooldown <= 0 {
		cooldown = 60 * time.Second
	}
	return elapsed >= cooldown
}

func (r *IndexerReconciler) setCondition(indexer *v1alpha1.TesseraIndexer, condType, status, message string) {
	now := time.Now().UTC()
	for i, c := range indexer.Status.Conditions {
		if c.Type == condType {
			indexer.Status.Conditions[i].Status = status
			indexer.Status.Conditions[i].Message = message
			indexer.Status.Conditions[i].LastTransitionTime = now
			return
		}
	}
	indexer.Status.Conditions = append(indexer.Status.Conditions, v1alpha1.Condition{
		Type:               condType,
		Status:             status,
		LastTransitionTime: now,
		Reason:             condType,
		Message:            message,
	})
}
