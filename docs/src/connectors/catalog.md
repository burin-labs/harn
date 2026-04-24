# Connector catalog

This catalog is the entry point for choosing a connector, wiring its trigger
manifest, and finding a ready-to-customize example. It reflects the current
transition plan:

- Cron, generic webhook, A2A push, and stream ingress are core runtime
  providers.
- GitHub, Slack, Linear, and Notion still have Rust compatibility providers,
  but new provider business logic should move to pure-Harn packages.
- Community connectors are Harn packages that export connector contract v1 and
  pass `harn connector check`.

For an LLM-sized version of this page, use
[`docs/llm/harn-triggers-quickref.md`](../../llm/harn-triggers-quickref.md).
That file is generated from the live provider catalog and checked by CI.

## Built-in runtime providers

| Provider | Use when | Signature | Required secrets | Recipes |
|---|---|---|---|---|
| `cron` | Run scheduled local workflows. | None. | None. | Daily report, enrichment pass, health check. |
| `webhook` | Accept generic Standard Webhooks-style HTTP events. | HMAC-SHA256, `webhook-signature`, `webhook-timestamp`, `webhook-id`. | `webhook/signing_secret`. | Stripe/Square-style handlers, HMAC-gated callbacks. |
| `a2a-push` | Accept A2A task/update pushes from another orchestrator. | Transport auth is handled by the A2A layer. | None at catalog level. | Multi-orchestrator fanout, remote reviewer dispatch. |
| `kafka`, `nats`, `pulsar`, `postgres-cdc`, `email`, `websocket` | Consume stream-shaped ingress through the shared stream connector. | Provider-specific transport configuration. | None at catalog level. | Fan-in, windowing, classifier routing. |

### Cron daily report

```toml
[[triggers]]
id = "daily-digest"
kind = "cron"
provider = "cron"
schedule = "0 9 * * 1-5"
timezone = "America/Los_Angeles"
handler = "send_daily_digest"
```

See `examples/triggers/cron-daily-digest`.

### Generic webhook with HMAC

```toml
[[triggers]]
id = "stripe-webhook"
kind = "webhook"
provider = "webhook"
match = { path = "/hooks/stripe", events = ["invoice.payment_succeeded"] }
handler = "handlers::on_webhook"
dedupe_key = "event.dedupe_key"
secrets = { signing_secret = "webhook/stripe-signing-secret" }
```

See `examples/triggers/webhook-generic-hmac`.

### A2A fanout

```toml
[[triggers]]
id = "reviewer-fanout"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["task.completed"] }
handler = "route_review_result"
```

See `examples/triggers/a2a-reviewer-fanout`.

## First-party pure-Harn packages

Each first-party connector repo should publish:

- repository URL and package install command
- `harn connector check` command
- required secrets and provider scopes
- supported trigger/event types
- mocked fixtures so CI does not need live provider credentials

| Provider | Package repo | Install | Contract check | Required secrets/scopes | Supported trigger/event types |
|---|---|---|---|---|---|
| GitHub | <https://github.com/burin-labs/harn-github-connector> | `harn add github.com/burin-labs/harn-github-connector@v0.1.0` | `harn connector check . --provider github` | Webhook secret; for outbound, GitHub App id, installation id, and private key. App permissions depend on methods: issues, pull requests, contents/metadata, checks, deployments. | `issues`, `pull_request`, `issue_comment`, `pull_request_review`, `push`, `workflow_run`, `deployment_status`, `check_run`; outbound REST/GraphQL escape hatches. |
| Slack | <https://github.com/burin-labs/harn-slack-connector> | `harn add github.com/burin-labs/harn-slack-connector@v0.1.0` | `harn connector check . --provider slack` | Signing secret; for outbound, bot token. Typical scopes: `app_mentions:read`, `channels:history`, `reactions:read`, `chat:write`, `reactions:write`, `users:read`, `files:write`. | URL verification, `message`, `app_mention`, `reaction_added`, `app_home_opened`, `assistant_thread_started`; outbound Web API calls. |
| Linear | <https://github.com/burin-labs/harn-linear-connector> | `harn add github.com/burin-labs/harn-linear-connector@v0.1.0` | `harn connector check . --provider linear` | Webhook signing secret; optional API key/access token for outbound GraphQL. | `Issue`, `Comment`, `IssueLabel`, `Project`, `Cycle`, `Customer`, `CustomerRequest`; outbound GraphQL. |
| Notion | <https://github.com/burin-labs/harn-notion-connector> | `harn add github.com/burin-labs/harn-notion-connector@v0.1.0` | `harn connector check . --provider notion --run-poll-tick` | Webhook verification token; outbound API token. Notion integration capabilities depend on pages/databases/comments used. | Webhook events such as subscription verification, page updates, comments, data source schema updates; `poll_tick` database/page watchers; outbound Notion API via `notion-sdk-harn`. |
| GitLab | <https://github.com/burin-labs/harn-gitlab-connector> | `harn add github.com/burin-labs/harn-gitlab-connector@v0.1.0` | `harn connector check . --provider gitlab` | Webhook signing secret (plain shared-secret `X-Gitlab-Token`, not HMAC); for outbound, an OAuth2 access token, PAT, or project/group access token with `api` scope. | `push`, `tag_push`, `merge_request`, `note`, `issue`, `pipeline`; outbound REST (notes, MR update/changes/approve, commit status, repository files), GraphQL passthrough, and OAuth2 helpers. |

Direct GitHub installs are the MVP path. Registry names such as
`@burin/notion-connector` should be used once the hosted first-party index is
available.

### GitHub stale PR nudger

```toml
[[triggers]]
id = "github-stale-pr-nudger"
kind = "cron"
provider = "cron"
schedule = "0 15 * * 1-5"
handler = "nudge_stale_prs"
```

See `examples/triggers/github-stale-pr-nudger`.

### Slack keyword router

```toml
[[triggers]]
id = "slack-keyword-router"
kind = "webhook"
provider = "slack"
match = { path = "/hooks/slack", events = ["message", "app_mention"] }
handler = "route_message"
secrets = { signing_secret = "slack/signing-secret" }
```

See `examples/triggers/slack-keyword-router`.

### Linear SLA breach alert

```toml
[[triggers]]
id = "linear-sla-breach"
kind = "cron"
provider = "cron"
schedule = "*/30 * * * *"
handler = "scan_for_sla_breaches"
```

See `examples/triggers/linear-sla-breach`.

### Notion database watcher

```toml
[[triggers]]
id = "notion-database-watcher"
kind = "poll"
provider = "notion"
handler = "on_database_change"
poll = { interval = "5m", state_key = "notion:database:watcher", max_batch_size = 50 }
secrets = { verification_token = "notion/verification-token" }
```

See `examples/triggers/notion-database-watcher`.

## Community connector discovery

A community connector is any Harn package that:

1. Declares `connector_contract = "v1"` in package or registry metadata.
2. Provides a `[[providers]]` manifest entry with `connector = { harn = ... }`.
3. Exports `provider_id`, `kinds`, `payload_schema`, and the relevant
   `normalize_inbound`, `poll_tick`, or `call` exports.
4. Ships deterministic `[connector_contract]` fixtures and passes
   `harn connector check .`.

Minimal package shape:

```toml
[package]
name = "harn-acme-connector"
version = "0.1.0"
connector_contract = "v1"

[exports]
default = "src/lib.harn"

[[providers]]
id = "acme"
connector = { harn = "src/lib.harn" }

[connector_contract]
version = 1

[[connector_contract.fixtures]]
provider = "acme"
name = "sample webhook"
kind = "webhook"
headers = { "content-type" = "application/json" }
body_json = { id = "evt-1", type = "thing.created" }
expect_type = "event"
expect_kind = "acme.thing.created"
expect_event_count = 1
```

Run:

```sh
harn connector check .
harn package check .
harn install --locked --offline
```

Use live credentials only in provider-specific integration tests. Catalog
examples and contract fixtures should run against mocked connector fixtures so
Harn CI, connector repo CI, and local authoring all stay deterministic.

## Authoring a connector

Start with [Connector authoring](./authoring.md), then add:

- a `harn.toml` package manifest with `connector_contract = "v1"`
- contract fixtures that cover normal event, ack-first response, reject, and
  poll cases where relevant
- a `README.md` with install, setup, required secrets/scopes, supported events,
  and `harn connector check` command
- mocked tests for outbound `call(...)` methods
- a small recipe example under `examples/triggers/` when the connector is
  first-party or broadly useful

The Harn-side contract is deliberately small. Keep provider SDKs or generated
API clients in separate packages when that makes the connector easier to test
and reuse, as with `notion-sdk-harn`.
