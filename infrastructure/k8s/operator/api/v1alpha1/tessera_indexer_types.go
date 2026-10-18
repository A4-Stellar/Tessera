package v1alpha1

import (
	"time"
)

// Condition defines an observation of a cluster component state.
type Condition struct {
	Type               string    `json:"type"`
	Status             string    `json:"status"` // "True", "False", "Unknown"
	LastTransitionTime time.Time `json:"lastTransitionTime"`
	Reason             string    `json:"reason"`
	Message            string    `json:"message"`
}

// DatabaseMigrationSpec configures automated pre-deployment schema migrations.
type DatabaseMigrationSpec struct {
	// Enabled toggles automatic preflight schema migration jobs.
	Enabled bool `json:"enabled"`
	// MigrationImage is the container image executing schema migrations.
	MigrationImage string `json:"migrationImage"`
	// SchemaHash uniquely identifies the database schema version.
	SchemaHash string `json:"schemaHash"`
	// DatabaseSecretRef references the Kubernetes Secret containing POSTGRES_URL.
	DatabaseSecretRef string `json:"databaseSecretRef"`
	// TimeoutSeconds specifies migration job execution deadline.
	TimeoutSeconds int64 `json:"timeoutSeconds,omitempty"`
}

// AutoScalingSpec configures horizontal pod autoscaling based on WebSocket clients and CPU.
type AutoScalingSpec struct {
	// Enabled toggles dynamic replica autoscaling.
	Enabled bool `json:"enabled"`
	// TargetCPUUtilizationPercentage sets threshold CPU usage before scaling (e.g., 75).
	TargetCPUUtilizationPercentage int32 `json:"targetCpuUtilizationPercentage"`
	// TargetWebSocketConnectionsPerPod sets client connection limit per replica (e.g., 500).
	TargetWebSocketConnectionsPerPod int32 `json:"targetWebSocketConnectionsPerPod"`
	// TargetRPCLatencyMillis sets RPC latency threshold before scaling (e.g., 250).
	TargetRPCLatencyMillis int32 `json:"targetRpcLatencyMillis,omitempty"`
	// ScaleUpCooldownSeconds avoids rapid flap scaling when traffic surges.
	ScaleUpCooldownSeconds int32 `json:"scaleUpCooldownSeconds"`
	// ScaleDownCooldownSeconds ensures stabilization before pods terminate.
	ScaleDownCooldownSeconds int32 `json:"scaleDownCooldownSeconds"`
}

// TesseraIndexerSpec defines the desired state of TesseraIndexer.
type TesseraIndexerSpec struct {
	// Replicas is the desired number of indexer instances.
	Replicas int32 `json:"replicas"`
	// MinReplicas defines the lower bound for autoscaling.
	MinReplicas int32 `json:"minReplicas"`
	// MaxReplicas defines the upper bound for autoscaling.
	MaxReplicas int32 `json:"maxReplicas"`
	// Image is the container image for Tessera Indexer.
	Image string `json:"image"`
	// Environment specifies target deployment mode ("mainnet", "testnet", "futurenet").
	Environment string `json:"environment"`
	// DatabaseSecretRef references Postgres connection credentials.
	DatabaseSecretRef string `json:"databaseSecretRef"`
	// DatabaseMigration orchestrates pre-rollout migrations.
	DatabaseMigration DatabaseMigrationSpec `json:"databaseMigration"`
	// AutoScaling manages dynamic replica adjustment.
	AutoScaling AutoScalingSpec `json:"autoScaling"`
}

// IndexerPhase denotes the current lifecycle state of the indexer cluster.
type IndexerPhase string

const (
	PhasePending   IndexerPhase = "Pending"
	PhaseMigrating IndexerPhase = "Migrating"
	PhaseDeploying IndexerPhase = "Deploying"
	PhaseRunning   IndexerPhase = "Running"
	PhaseScaling   IndexerPhase = "Scaling"
	PhaseFailed    IndexerPhase = "Failed"
)

// TesseraIndexerStatus defines the observed state of TesseraIndexer.
type TesseraIndexerStatus struct {
	Phase                      IndexerPhase `json:"phase"`
	CurrentReplicas            int32        `json:"currentReplicas"`
	ReadyReplicas              int32        `json:"readyReplicas"`
	ActiveWebSocketConnections int32        `json:"activeWebSocketConnections"`
	AverageCPUUtilization      int32        `json:"averageCpuUtilization"`
	LastMigrationHash          string       `json:"lastMigrationHash,omitempty"`
	LastMigrationTime          *time.Time   `json:"lastMigrationTime,omitempty"`
	LastScaleTime              *time.Time   `json:"lastScaleTime,omitempty"`
	Conditions                 []Condition  `json:"conditions,omitempty"`
}

// TesseraIndexer is the Schema for the tesseraindexers API.
type TesseraIndexer struct {
	TypeMeta   TypeMeta             `json:"typeMeta"`
	ObjectMeta ObjectMeta           `json:"metadata"`
	Spec       TesseraIndexerSpec   `json:"spec"`
	Status     TesseraIndexerStatus `json:"status,omitempty"`
}

type TypeMeta struct {
	Kind       string `json:"kind"`
	APIVersion string `json:"apiVersion"`
}

type ObjectMeta struct {
	Name        string            `json:"name"`
	Namespace   string            `json:"namespace"`
	Labels      map[string]string `json:"labels,omitempty"`
	Annotations map[string]string `json:"annotations,omitempty"`
}
