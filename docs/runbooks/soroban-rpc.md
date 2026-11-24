# Soroban RPC unavailable

Check all configured providers, fail over to a healthy provider, and verify the network passphrase before replaying missed ledgers.

## Automated triage (P1)

The triage bot ([automated-triage.md](automated-triage.md)) runs
`check_postgres_connections` and then `restart_indexer_pod`, so the poller
reconnects to the configured providers. Provider failover and replay stay
manual.
