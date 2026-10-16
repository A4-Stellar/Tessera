package controllers

import (
	"context"
	"fmt"
	"time"

	"github.com/A4-Stellar/Tessera/infrastructure/k8s/operator/api/v1alpha1"
)

// DocsK8sClient abstracts operations for documentation workloads.
type DocsK8sClient interface {
	ReconcileDocsDeployment(ctx context.Context, namespace, name, image string, replicas int32) (int32, error)
	ReconcileDocsServiceAndIngress(ctx context.Context, namespace, name, host string, tls bool) (string, error)
}

// DocsReconciler reconciles TesseraDocs custom resources.
type DocsReconciler struct {
	Client DocsK8sClient
}

// NewDocsReconciler creates a DocsReconciler.
func NewDocsReconciler(client DocsK8sClient) *DocsReconciler {
	return &DocsReconciler{Client: client}
}

// Reconcile reconciles the desired state of a TesseraDocs resource.
func (r *DocsReconciler) Reconcile(ctx context.Context, docs *v1alpha1.TesseraDocs) (ReconcileResult, error) {
	available, err := r.Client.ReconcileDocsDeployment(
		ctx,
		docs.ObjectMeta.Namespace,
		docs.ObjectMeta.Name,
		docs.Spec.Image,
		docs.Spec.Replicas,
	)
	if err != nil {
		docs.Status.Phase = v1alpha1.DocsPhaseError
		r.setCondition(docs, "DeploymentError", "False", err.Error())
		return ReconcileResult{Requeue: true}, err
	}

	endpoint, err := r.Client.ReconcileDocsServiceAndIngress(
		ctx,
		docs.ObjectMeta.Namespace,
		docs.ObjectMeta.Name,
		docs.Spec.IngressHost,
		docs.Spec.EnableTLS,
	)
	if err != nil {
		docs.Status.Phase = v1alpha1.DocsPhaseError
		r.setCondition(docs, "IngressError", "False", err.Error())
		return ReconcileResult{Requeue: true}, err
	}

	docs.Status.AvailableReplicas = available
	docs.Status.Endpoint = endpoint

	if available >= docs.Spec.Replicas && docs.Spec.Replicas > 0 {
		docs.Status.Phase = v1alpha1.DocsPhaseRunning
		r.setCondition(docs, "DocsAvailable", "True", fmt.Sprintf("Docs accessible at %s", endpoint))
	} else {
		docs.Status.Phase = v1alpha1.DocsPhasePending
		r.setCondition(docs, "DocsDeploying", "True", "Waiting for docs pods to become ready")
	}

	return ReconcileResult{RequeueAfter: 30 * time.Second}, nil
}

func (r *DocsReconciler) setCondition(docs *v1alpha1.TesseraDocs, condType, status, message string) {
	now := time.Now().UTC()
	for i, c := range docs.Status.Conditions {
		if c.Type == condType {
			docs.Status.Conditions[i].Status = status
			docs.Status.Conditions[i].Message = message
			docs.Status.Conditions[i].LastTransitionTime = now
			return
		}
	}
	docs.Status.Conditions = append(docs.Status.Conditions, v1alpha1.Condition{
		Type:               condType,
		Status:             status,
		LastTransitionTime: now,
		Reason:             condType,
		Message:            message,
	})
}
