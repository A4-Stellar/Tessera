# SPIFFE workload identity (issue #394)

These manifests establish the `tessera.io` trust domain in the `prod`
namespace:

- `server.yaml` — SPIRE server, SQLite datastore, `k8s_psat` node attestation,
  and a `k8sbundle` notifier that publishes `spire-bundle`.
- `agent-daemonset.yaml` — one SPIRE agent per node, reachable by workloads at
  `/run/spire/agent/agent.sock` on the host.
- `registration.yaml` — a one-shot Job creating workload entries for
  `tessera-api`, `tessera-indexer`, `tessera-notary`, and `tessera-worker`.

Apply in order:

```sh
kubectl apply -f infrastructure/k8s/spire/server.yaml
kubectl apply -f infrastructure/k8s/spire/agent-daemonset.yaml
kubectl apply -f infrastructure/k8s/spire/registration.yaml
```

Each registration produces an ID of the form
`spiffe://tessera.io/ns/prod/sa/<service-name>`. Service accounts in the chart
must use those exact names.

The API Helm chart (`../deployments/tessera-api`) can inject an SVID-writer
sidecar and an Envoy mTLS sidecar with `mtls.enabled=true`. The writer fetches
the X.509 SVID every four minutes, normalizes the SPIRE CLI output to
`svid.pem`, `svid.key`, `bundle.pem`, and `spiffe_id`, and the Rust API
(`api/src/security/mtls.rs`) reloads it on change. If the SVID disappears or
belongs to the wrong workload ID, all API processes terminate within ten
seconds.
