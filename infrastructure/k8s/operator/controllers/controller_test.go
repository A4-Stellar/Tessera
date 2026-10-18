package controllers

import (
	"context"
	"errors"
	"testing"

	"github.com/A4-Stellar/Tessera/infrastructure/k8s/operator/api/v1alpha1"
)

type mockK8sClient struct {
	jobStatusMap        map[string]JobState
	createdJobs         []string
	deploymentReplicas  map[string]int32
	deploymentReady     map[string]int32
	deploymentImage     map[string]string
	metrics             ClusterMetrics
	failMigrationLaunch bool
}

func newMockK8sClient() *mockK8sClient {
	return &mockK8sClient{
		jobStatusMap:       make(map[string]JobState),
		deploymentReplicas: make(map[string]int32),
		deploymentReady:    make(map[string]int32),
		deploymentImage:    make(map[string]string),
	}
}

func (m *mockK8sClient) GetMigrationJobStatus(ctx context.Context, namespace, jobName string) (JobState, error) {
	st, ok := m.jobStatusMap[jobName]
	if !ok {
		return "", errors.New("job not found")
	}
	return st, nil
}

func (m *mockK8sClient) CreateMigrationJob(ctx context.Context, namespace string, spec v1alpha1.DatabaseMigrationSpec) error {
	if m.failMigrationLaunch {
		return errors.New("failed to schedule pod")
	}
	jobName := "tessera-indexer-migrate-" + spec.SchemaHash
	m.createdJobs = append(m.createdJobs, jobName)
	m.jobStatusMap[jobName] = JobRunning
	return nil
}

func (m *mockK8sClient) GetDeploymentReplicas(ctx context.Context, namespace, name string) (int32, int32, error) {
	return m.deploymentReplicas[name], m.deploymentReady[name], nil
}

func (m *mockK8sClient) UpdateDeploymentReplicas(ctx context.Context, namespace, name string, replicas int32) error {
	m.deploymentReplicas[name] = replicas
	return nil
}

func (m *mockK8sClient) UpdateDeploymentImage(ctx context.Context, namespace, name string, image string) error {
	m.deploymentImage[name] = image
	return nil
}

func (m *mockK8sClient) GetClusterMetrics(ctx context.Context, namespace, name string) (ClusterMetrics, error) {
	return m.metrics, nil
}

func TestDatabaseMigrationPreflight_BlocksRollingUpdate(t *testing.T) {
	mockClient := newMockK8sClient()
	reconciler := NewIndexerReconciler(mockClient)

	indexer := &v1alpha1.TesseraIndexer{
		ObjectMeta: v1alpha1.ObjectMeta{
			Name:      "tessera-indexer",
			Namespace: "default",
		},
		Spec: v1alpha1.TesseraIndexerSpec{
			Replicas: 3,
			Image:    "ghcr.io/a4-stellar/tessera-indexer:v2.0.0",
			DatabaseMigration: v1alpha1.DatabaseMigrationSpec{
				Enabled:        true,
				MigrationImage: "ghcr.io/a4-stellar/tessera-migrate:v2.0.0",
				SchemaHash:     "sha256-abc12345",
			},
		},
	}

	// 1. First reconcile: migration job should be created and rolling update blocked
	res, err := reconciler.Reconcile(context.Background(), indexer)
	if err != nil {
		t.Fatalf("unexpected error on first reconcile: %v", err)
	}
	if !res.BlockRollingUpdate {
		t.Fatalf("expected BlockRollingUpdate=true while migration runs")
	}
	if indexer.Status.Phase != v1alpha1.PhaseMigrating {
		t.Fatalf("expected phase Migrating, got %s", indexer.Status.Phase)
	}
	if len(mockClient.createdJobs) != 1 {
		t.Fatalf("expected 1 migration job created, got %d", len(mockClient.createdJobs))
	}

	// 2. Second reconcile: Job still running
	res, err = reconciler.Reconcile(context.Background(), indexer)
	if err != nil {
		t.Fatalf("unexpected error on second reconcile: %v", err)
	}
	if !res.BlockRollingUpdate {
		t.Fatalf("expected BlockRollingUpdate=true while job still running")
	}

	// 3. Third reconcile: Job completes with Succeeded
	mockClient.jobStatusMap["tessera-indexer-migrate-sha256-abc12345"] = JobSucceeded
	mockClient.deploymentReplicas["tessera-indexer"] = 3
	mockClient.deploymentReady["tessera-indexer"] = 3

	res, err = reconciler.Reconcile(context.Background(), indexer)
	if err != nil {
		t.Fatalf("unexpected error when migration succeeds: %v", err)
	}
	if res.BlockRollingUpdate {
		t.Fatalf("expected BlockRollingUpdate=false after migration success")
	}
	if indexer.Status.LastMigrationHash != "sha256-abc12345" {
		t.Fatalf("expected LastMigrationHash to be updated, got %s", indexer.Status.LastMigrationHash)
	}
	if indexer.Status.Phase != v1alpha1.PhaseRunning {
		t.Fatalf("expected phase Running, got %s", indexer.Status.Phase)
	}
}

func TestDatabaseMigrationFailure_HaltsDeployment(t *testing.T) {
	mockClient := newMockK8sClient()
	mockClient.jobStatusMap["tessera-indexer-migrate-fail-hash"] = JobFailed
	reconciler := NewIndexerReconciler(mockClient)

	indexer := &v1alpha1.TesseraIndexer{
		ObjectMeta: v1alpha1.ObjectMeta{
			Name:      "tessera-indexer",
			Namespace: "default",
		},
		Spec: v1alpha1.TesseraIndexerSpec{
			Replicas: 3,
			Image:    "ghcr.io/a4-stellar/tessera-indexer:v2.0.0",
			DatabaseMigration: v1alpha1.DatabaseMigrationSpec{
				Enabled:    true,
				SchemaHash: "fail-hash",
			},
		},
	}

	res, err := reconciler.Reconcile(context.Background(), indexer)
	if err == nil {
		t.Fatalf("expected error when migration fails")
	}
	if !res.BlockRollingUpdate {
		t.Fatalf("expected BlockRollingUpdate=true on migration failure")
	}
	if indexer.Status.Phase != v1alpha1.PhaseFailed {
		t.Fatalf("expected PhaseFailed, got %s", indexer.Status.Phase)
	}
}

func TestAutoscaling_WebSocketConnectionsAndCPU(t *testing.T) {
	reconciler := NewIndexerReconciler(newMockK8sClient())

	indexer := &v1alpha1.TesseraIndexer{
		Spec: v1alpha1.TesseraIndexerSpec{
			Replicas:    2,
			MinReplicas: 2,
			MaxReplicas: 10,
			AutoScaling: v1alpha1.AutoScalingSpec{
				Enabled:                          true,
				TargetCPUUtilizationPercentage:   70,
				TargetWebSocketConnectionsPerPod: 500,
			},
		},
	}

	// Case 1: WebSocket traffic surge (2200 active connections / 500 = ceil(4.4) = 5 pods)
	metrics := ClusterMetrics{
		AverageCPUUtilization:      50,
		ActiveWebSocketConnections: 2200,
	}
	desired := reconciler.ComputeDesiredReplicas(indexer, 2, metrics)
	if desired != 5 {
		t.Fatalf("expected 5 replicas for 2200 WS connections, got %d", desired)
	}

	// Case 2: CPU load spike (90% / 70% * 3 pods = ceil(3.85) = 4 pods)
	metricsCPU := ClusterMetrics{
		AverageCPUUtilization:      90,
		ActiveWebSocketConnections: 100,
	}
	desiredCPU := reconciler.ComputeDesiredReplicas(indexer, 3, metricsCPU)
	if desiredCPU != 4 {
		t.Fatalf("expected 4 replicas for 90%% CPU spike, got %d", desiredCPU)
	}

	// Case 3: Combined demand picks the maximum
	metricsCombined := ClusterMetrics{
		AverageCPUUtilization:      90,   // demand: ceil(3 * 90 / 70) = 4
		ActiveWebSocketConnections: 3100, // demand: ceil(3100 / 500) = 7
	}
	desiredCombined := reconciler.ComputeDesiredReplicas(indexer, 3, metricsCombined)
	if desiredCombined != 7 {
		t.Fatalf("expected max demand 7 replicas, got %d", desiredCombined)
	}

	// Case 4: Clamped by MaxReplicas (demand 12 -> clamped to 10)
	metricsOverload := ClusterMetrics{
		AverageCPUUtilization:      99,
		ActiveWebSocketConnections: 6000,
	}
	desiredOverload := reconciler.ComputeDesiredReplicas(indexer, 3, metricsOverload)
	if desiredOverload != 10 {
		t.Fatalf("expected maxReplicas clamp to 10, got %d", desiredOverload)
	}
}

func TestAutoscaling_RPCLatency(t *testing.T) {
	reconciler := NewIndexerReconciler(newMockK8sClient())

	indexer := &v1alpha1.TesseraIndexer{
		Spec: v1alpha1.TesseraIndexerSpec{
			Replicas:    2,
			MinReplicas: 2,
			MaxReplicas: 8,
			AutoScaling: v1alpha1.AutoScalingSpec{
				Enabled:                          true,
				TargetCPUUtilizationPercentage:   80,
				TargetWebSocketConnectionsPerPod: 1000,
				TargetRPCLatencyMillis:           100, // target p99 latency < 100ms
			},
		},
	}

	// RPC Latency Spike: 250ms latency with 2 pods -> ceil(2 * 250 / 100) = 5 pods
	metrics := ClusterMetrics{
		AverageCPUUtilization:      40,
		ActiveWebSocketConnections: 200,
		RPCLatencyMillis:           250,
	}
	desired := reconciler.ComputeDesiredReplicas(indexer, 2, metrics)
	if desired != 5 {
		t.Fatalf("expected 5 replicas for 250ms RPC latency, got %d", desired)
	}
}

type mockDocsClient struct {
	available int32
}

func (m *mockDocsClient) ReconcileDocsDeployment(ctx context.Context, namespace, name, image string, replicas int32) (int32, error) {
	return m.available, nil
}

func (m *mockDocsClient) ReconcileDocsServiceAndIngress(ctx context.Context, namespace, name, host string, tls bool) (string, error) {
	return "https://" + host, nil
}

func TestDocsController_Reconciliation(t *testing.T) {
	client := &mockDocsClient{available: 2}
	reconciler := NewDocsReconciler(client)

	docs := &v1alpha1.TesseraDocs{
		ObjectMeta: v1alpha1.ObjectMeta{
			Name:      "tessera-docs",
			Namespace: "default",
		},
		Spec: v1alpha1.TesseraDocsSpec{
			Replicas:    2,
			Image:       "ghcr.io/a4-stellar/tessera-docs:latest",
			IngressHost: "docs.tessera.network",
			EnableTLS:   true,
		},
	}

	res, err := reconciler.Reconcile(context.Background(), docs)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if res.Requeue {
		t.Fatalf("expected no immediate requeue")
	}
	if docs.Status.Phase != v1alpha1.DocsPhaseRunning {
		t.Fatalf("expected DocsPhaseRunning, got %s", docs.Status.Phase)
	}
	if docs.Status.Endpoint != "https://docs.tessera.network" {
		t.Fatalf("unexpected endpoint: %s", docs.Status.Endpoint)
	}
}
