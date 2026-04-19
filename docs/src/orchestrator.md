# Orchestrator

`harn orchestrator serve` is the long-running process entry point for
manifest-driven trigger ingestion and connector activation.

Today, the command establishes the startup and shutdown scaffold:

- load `harn.toml` through the existing manifest loader
- boot the selected orchestrator role
- initialize the shared EventLog under `--state-dir`
- initialize the configured secret-provider chain
- resolve and register manifest triggers
- register and activate placeholder connectors for each manifest provider
- write a state snapshot and idle until shutdown

Current limitations:

- the HTTP listener is still a stub and logs
  `HTTP listener not yet implemented (see O-02 #179)`
- `multi-tenant` returns a clear not-implemented error that points at
  `O-12 #190`
- `inspect`, `replay`, `dlq`, and `queue` are placeholders for
  `O-08 #185`

## Command

```bash
harn orchestrator serve \
  --config harn.toml \
  --state-dir ./.harn/orchestrator \
  --bind 0.0.0.0:8080 \
  --role single-tenant
```

On startup, the command logs the active secret-provider chain, loaded
triggers, registered connectors, and the requested bind address. On
SIGTERM, it performs scaffolded graceful shutdown, appends lifecycle
events to the EventLog, and persists a final
`orchestrator-state.json` snapshot under `--state-dir`.
