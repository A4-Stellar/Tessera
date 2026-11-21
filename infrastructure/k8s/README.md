# Kubernetes workload identity and mTLS

- `spire/` installs the SPIRE server and agent DaemonSet for the `tessera.io`
  trust domain and registers `spiffe://tessera.io/ns/prod/sa/<service-name>`
  workload entries.
- `deployments/tessera-api/` is the API Helm chart. Set `mtls.enabled=true` to
  inject:
  - an init container that stages the SPIRE agent CLI,
  - a `spiffe-svid-writer` sidecar that refreshes the X.509 SVID every four
    minutes into `/var/run/spiffe.svid`,
  - an Envoy sidecar that requires a client certificate whose URI SAN matches
    `spiffe://tessera.io/ns/prod/sa/tessera-api` and validates it against the
    SPIFFE bundle.

Application traffic is plain HTTP inside the pod; the Service targets the
Envoy `mesh` port, so all cross-pod traffic is mTLS. The Rust API loads the
same SVID into `reqwest` client certificates and PostgreSQL TLS options and
reloads it on rotation (`api/src/security/mtls.rs`). Revocation terminates the
process within ten seconds, including requests already in flight.
