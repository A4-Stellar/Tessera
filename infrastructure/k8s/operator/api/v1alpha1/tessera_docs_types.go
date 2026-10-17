package v1alpha1

// TesseraDocsSpec defines the desired state of TesseraDocs.
type TesseraDocsSpec struct {
	// Replicas is the number of documentation server pods.
	Replicas int32 `json:"replicas"`
	// Image is the container image for documentation frontend.
	Image string `json:"image"`
	// IngressHost is the routing hostname (e.g., "docs.tessera.network").
	IngressHost string `json:"ingressHost"`
	// EnableTLS enables TLS certificate termination.
	EnableTLS bool `json:"enableTls"`
}

// DocsPhase denotes the state of the docs site.
type DocsPhase string

const (
	DocsPhasePending DocsPhase = "Pending"
	DocsPhaseRunning DocsPhase = "Running"
	DocsPhaseError   DocsPhase = "Error"
)

// TesseraDocsStatus defines the observed state of TesseraDocs.
type TesseraDocsStatus struct {
	Phase             DocsPhase   `json:"phase"`
	AvailableReplicas int32       `json:"availableReplicas"`
	Endpoint          string      `json:"endpoint"`
	Conditions        []Condition `json:"conditions,omitempty"`
}

// TesseraDocs is the Schema for the tesseradocs API.
type TesseraDocs struct {
	TypeMeta   TypeMeta          `json:"typeMeta"`
	ObjectMeta ObjectMeta        `json:"metadata"`
	Spec       TesseraDocsSpec   `json:"spec"`
	Status     TesseraDocsStatus `json:"status,omitempty"`
}
