# Orchestrator

`harn orchestrator serve` is the long-running process entry point for
manifest-driven trigger ingestion and connector activation.

Today, the command:

- load `harn.toml` through the existing manifest loader
- boot the selected orchestrator role
- initialize the shared EventLog under `--state-dir`
- initialize the configured secret-provider chain
- resolve and register manifest triggers
- activate connectors for the manifest's providers
- bind an HTTP listener for `webhook` and `a2a-push` triggers
- write a state snapshot and stay up until shutdown

Current limitations:

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
  --cert certs/dev.pem \
  --key certs/dev-key.pem \
  --role single-tenant
```

Omit `--cert` and `--key` to serve plain HTTP. When both are present,
the listener serves HTTPS and terminates TLS with `rustls`.

On startup, the command logs the active secret-provider chain, loaded
triggers, registered connectors, and the actual bound listener URL. On
SIGTERM, it stops accepting new requests, lets in-flight requests drain,
appends lifecycle events to the EventLog, and persists a final
`orchestrator-state.json` snapshot under `--state-dir`.

## HTTP Listener

The orchestrator listener assembles routes from `[[triggers]]` entries
with `kind = "webhook"` or `kind = "a2a-push"`.

- If a trigger declares `path = "/github/issues"`, that path is used.
- Otherwise the route defaults to `/triggers/<id>`.
- `/healthz` and `/readyz` are reserved listener endpoints; use
  `GET /healthz` and `GET /readyz` for process health checks.

Accepted deliveries are normalized into `TriggerEvent` records and
appended to the shared `orchestrator.triggers.pending` queue in the
event log for downstream dispatch.

### Listener controls

Listener-wide controls live under `[orchestrator]` in `harn.toml`.

```toml
[orchestrator]
allowed_origins = ["https://app.example.com"]
max_body_bytes = 10485760
```

- `allowed_origins` defaults to `["*"]` semantics when omitted or empty.
  Requests with an `Origin` header outside the allowlist are rejected
  with `403 Forbidden`.
- `max_body_bytes` defaults to `10485760` bytes (10 MiB). Larger
  requests are rejected with `413 Payload Too Large`.

### Trigger examples

```toml
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
path = "/triggers/github-new-issue"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
secrets = { signing_secret = "github/webhook-secret" }

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
path = "/a2a/review"
match = { events = ["a2a.task.received"] }
handler = "a2a://reviewer.prod/triage"
```

GitHub webhook triggers verify the `X-Hub-Signature-256` HMAC against
`secrets.signing_secret` before enqueueing. Generic `provider = "webhook"`
triggers use the shared Standard Webhooks verifier. `a2a-push` routes
currently accept unsigned deliveries.
