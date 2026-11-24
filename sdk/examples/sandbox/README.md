# Sandbox SDK examples

One example per Tessera SDK showing the **identical production API surface**
used against the regulatory sandbox. The only difference is the
`X-Sandbox-Tenant-ID` header, which selects the caller's RLS-isolated data.

| Language | Example | Run |
|---|---|---|
| TypeScript | `sandbox.ts` | `npx ts-node sandbox.ts` |
| Python | `sandbox.py` | `python sandbox.py` |
| Python analytics | `sandbox_analytics.py` | `python sandbox_analytics.py` |
| Go | `sandbox.go` | `go run sandbox.go` |
| Java | `SandboxExample.java` | `javac -cp <sdk jar> SandboxExample.java && java SandboxExample` |
| C# | `SandboxExample.cs` | `dotnet run` in a project referencing `Tessera.SDK` |
| Rust | `sandbox.rs` | `cargo run --example sandbox` |

Start the stack first:

```sh
tessera sandbox start            # API on http://localhost:8081
curl -H 'X-Sandbox-Tenant-ID: acme-1' http://localhost:8081/sandbox/status
curl -X POST -H 'X-Sandbox-Tenant-ID: acme-1' http://localhost:8081/sandbox/reset
```

Sandbox mode relaxes the token bucket to **1,000 req/s** but exposes the same
`/v1/*` routes as production. Never send a production `X-Tenant-ID` to the
sandbox: the two headers select different rate-limit namespaces.
