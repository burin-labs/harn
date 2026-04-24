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
- expose `/metrics` and configurable process logs for production observability
- write a state snapshot and stay up until shutdown

Current limitations:

- Multi-tenant mode isolates listener ingress, tenant-scoped secrets, and
  trigger EventLog topics. Shared scheduler/provider objects remain
  orchestrator-global.

## Command

```bash
harn orchestrator serve \
  --config harn.toml \
  --state-dir ./.harn/orchestrator \
  --bind 0.0.0.0:8080 \
  --cert certs/dev.pem \
  --key certs/dev-key.pem \
  --pump-max-outstanding 64 \
  --log-format json \
  --role single-tenant
```

Omit `--cert` and `--key` to serve plain HTTP. When both are present,
the listener serves HTTPS and terminates TLS with `rustls`.

On startup, the command logs the active secret-provider chain, loaded
triggers, registered connectors, and the actual bound listener URL. On
SIGTERM, it stops accepting new requests, lets in-flight requests drain,
appends lifecycle events to the EventLog, and persists a final
`orchestrator-state.json` snapshot under `--state-dir`.

Topic pumps are bounded. `--pump-max-outstanding` controls how many
items each pump may admit at once; the same value can be configured in
`harn.toml` as:

```toml
[orchestrator]
pumps.max_outstanding = 64
```

When the inbox pump reaches that limit, it stops reading more
`trigger.inbox.envelopes` records until an admitted dispatch finishes.
The source cursor is advanced only after a record is admitted, and
shutdown waits for admitted work before reporting a truncated drain.

Startup no longer replays old `trigger.inbox` entries automatically.
If the previous process died after writing an inbox envelope but before
writing any matching `trigger.outbox` record, the restarted orchestrator
surfaces those envelopes instead of silently re-firing them:

- `harn orchestrator queue` shows a `stranded_envelopes=<count>` summary
  plus a `Stranded envelopes:` section with the event ids, bindings, and
  ages.
- `harn orchestrator queue ls` also reports worker queue depth, in-flight
  claims, response counts, and oldest-unclaimed age for each known
  `worker://` queue.
- `orchestrator.lifecycle` records a
  `startup_stranded_envelopes` event with a `count` payload.
- Recovery is explicit via `harn orchestrator recover`.

Worker queues use the same state dir EventLog as the rest of the orchestrator.
That means a producer manifest can enqueue `worker://triage` jobs while a
separate consumer manifest, running against the same EventLog backend, drains
the queue with `harn orchestrator queue drain triage`. See
[Worker dispatch](./orchestrator/worker-dispatch.md) for the full model.

`--manifest` is an alias for `--config`, and `--listen` is an alias for
`--bind`. Container deployments can also configure those through
`HARN_ORCHESTRATOR_MANIFEST`, `HARN_ORCHESTRATOR_LISTEN`,
`HARN_ORCHESTRATOR_STATE_DIR`, `HARN_ORCHESTRATOR_CERT`, and
`HARN_ORCHESTRATOR_KEY`.

The listener serves Prometheus metrics at `/metrics`. Logs default to compact
text; pass `--log-format json` for newline-delimited JSON on stdout plus a
rotating file at `<state-dir>/logs/orchestrator.log`. Set
`HARN_OTEL_ENDPOINT` to export OTLP traces. See
[Orchestrator observability](./orchestrator/observability.md) for metric names,
log configuration, and the dashboard example.

Use `--role multi-tenant` with `harn orchestrator tenant` to provision API-key
backed tenants, tenant-prefixed EventLog topics, and tenant-scoped secrets. See
[Multi-tenant orchestrator](./orchestrator/multi-tenant.md) for the tenant
lifecycle and isolation model.

Hot reload now supports four equivalent entrypoints:

- `SIGHUP` on Unix
- `harn orchestrator reload`
- authenticated `POST /admin/reload`
- `harn orchestrator serve --watch` for manifest file changes

Each path reparses `harn.toml`, re-collects manifest triggers, prepares
connector changes off to the side, then swaps the live binding set in
place. Requests already in flight keep the binding version they started
with; new requests route to the newest active binding version. If the
new manifest is invalid, a handler export no longer resolves, or a
connector cannot initialize/activate, the reload is rejected and the
existing bindings stay live.

The orchestrator records `reload_succeeded` / `reload_failed` events on
`orchestrator.manifest`, refreshes `orchestrator-state.json` after a
successful reload, and now includes `listener_url` in that snapshot so
local tooling can target the running admin API. See
[Hot reload](./orchestrator/hot-reload.md) for the full workflow and
operator-facing details.

## Recovery

Use `recover` to inspect or replay stranded inbox envelopes explicitly.

```bash
harn orchestrator recover \
  --config harn.toml \
  --state-dir ./.harn/orchestrator \
  --envelope-age 5m \
  --dry-run
```

`--envelope-age` is required so recovery stays scoped to envelopes older
than the threshold you choose. Supported suffixes are `ms`, `s`, `m`,
`h`, `d`, and `w`.

`--dry-run` lists candidates only. To actually replay them, rerun the
command without `--dry-run` and add `--yes`:

```bash
harn orchestrator recover \
  --config harn.toml \
  --state-dir ./.harn/orchestrator \
  --envelope-age 5m \
  --yes
```

Recovery reuses the normal `trigger_replay(...)` path, so replayed
envelopes still flow through the dispatcher's retry policy and DLQ
handling instead of using a special bypass path.

## HTTP Listener

The orchestrator listener assembles routes from `[[triggers]]` entries
with `kind = "webhook"` or `kind = "a2a-push"`.

- If a trigger declares `path = "/github/issues"`, that path is used.
- Otherwise the route defaults to `/triggers/<id>`.
- `/health`, `/healthz`, and `/readyz` are reserved listener endpoints;
  use `GET /health` for container health checks.

Accepted deliveries are normalized into `TriggerEvent` records and
appended to the shared `orchestrator.triggers.pending` queue in the
event log for downstream dispatch.

Hot reload uses the trigger registry's versioned manifest bindings. A
modified trigger id drains the old binding version, activates a new
version, and keeps terminated versions around for a short retention
window so operators can inspect the handoff without the registry
growing unbounded.

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

### Listener auth

Health probes stay public:

- `GET /health`
- `GET /healthz`
- `GET /readyz`

Webhook routes keep using their provider-specific signature checks.
`a2a-push` routes require either a bearer API key or a shared-secret
HMAC authorization header. The `/acp` WebSocket endpoint requires bearer
auth when `HARN_ORCHESTRATOR_API_KEYS` is configured.

Configure the auth material with environment variables:

```bash
export HARN_ORCHESTRATOR_API_KEYS="dev-key-1,dev-key-2"
export HARN_ORCHESTRATOR_HMAC_SECRET="replace-me"
```

Bearer requests use:

```text
Authorization: Bearer <api-key>
```

ACP browser or remote-IDE clients connect to `ws://<host>/acp` or
`wss://<host>/acp` and send one JSON-RPC ACP message per text frame. The
transport adds `_harn.eventId` to outbound frames; clients pass
`lastAckedEventId` to `session/load` after reconnect so the orchestrator can
replay missed frames from the retained worker or EventLog. See
[ACP over WebSocket](./acp/websocket.md).

HMAC requests use:

```text
Authorization: HMAC-SHA256 timestamp=<unix>,signature=<base64>
```

The canonical string is:

```text
METHOD
PATH
TIMESTAMP
SHA256(BODY)
```

`METHOD` is uppercased, `PATH` is the request path without the query
string, `TIMESTAMP` is a Unix epoch seconds value, and `SHA256(BODY)` is
the lowercase hex digest of the raw request body. Timestamps outside the
5-minute replay window are rejected with `401 Unauthorized`.

## Deployment

Release tags publish a distroless container image to
`ghcr.io/burin-labs/harn` for both `linux/amd64` and `linux/arm64`.

```bash
docker run \
  -p 8080:8080 \
  -v "$PWD/triggers.toml:/etc/harn/triggers.toml:ro" \
  -e HARN_ORCHESTRATOR_API_KEYS=xxx \
  -e HARN_ORCHESTRATOR_HMAC_SECRET=replace-me \
  -e RUST_LOG=info \
  ghcr.io/burin-labs/harn
```

The image runs as UID `10001` and stores orchestrator state under
`/var/lib/harn/state` by default. Override the startup contract with
environment variables instead of replacing the entrypoint:

- `HARN_ORCHESTRATOR_MANIFEST` defaults to `/etc/harn/triggers.toml`
- `HARN_ORCHESTRATOR_LISTEN` defaults to `0.0.0.0:8080`
- `HARN_ORCHESTRATOR_STATE_DIR` defaults to `/var/lib/harn/state`
- `HARN_ORCHESTRATOR_API_KEYS` supplies bearer credentials for
  authenticated `a2a-push` routes
- `HARN_ORCHESTRATOR_HMAC_SECRET` supplies the shared secret for
  canonical-request HMAC auth on `a2a-push` routes
- `HARN_SECRET_*`, provider API-key env vars, and deployment-specific
  `HARN_PROVIDER_*` values are passed through to connector/provider code
- `RUST_LOG` controls runtime log verbosity

The image healthcheck issues `GET /health` against the local listener, so
it works with Docker, BuildKit smoke tests, and most container platforms
without requiring curl inside the distroless runtime.

For managed cloud deploys, use `harn orchestrator deploy` to validate the
manifest, generate a provider deploy bundle, optionally build/push an image,
sync locally available secrets, and run the provider CLI:

```bash
harn orchestrator deploy --provider fly --manifest ./harn.toml --build
harn orchestrator deploy --provider render --manifest ./harn.toml --render-service srv-...
harn orchestrator deploy --provider railway --manifest ./harn.toml
```

Provider-specific templates and notes live under:

- [Render](./deploy/render.md)
- [Fly.io](./deploy/fly.md)
- [Railway](./deploy/railway.md)

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
without `[triggers.a2a_push]` require either
`Authorization: Bearer <api-key>` or a valid
`Authorization: HMAC-SHA256 ...` header before enqueueing. Routes with
`[triggers.a2a_push]` use the A2A push connector's JWT/JWKS verifier.
