# Linear connector

`LinearConnector` is Harn's built-in Linear integration for inbound webhook
deliveries plus outbound GraphQL calls.

The current landing covers the core connector contract from `#173`:

- inbound HMAC verification via `Linear-Signature`
- replay protection using `webhookTimestamp` with a 60s window plus a default
  15s grace period
- typed payload narrowing for `Issue`, `Comment`, `IssueLabel`, `Project`,
  `Cycle`, `Customer`, and `CustomerRequest` deliveries
- typed `Issue` update diffs exposed on `payload.changes`
- optional webhook-health monitoring with automatic re-enable after a healthy
  probe streak
- outbound GraphQL helpers through `std/connectors/linear`
- `harn connect linear` for programmatic webhook creation from manifest triggers

## Inbound webhook bindings

Configure Linear as a webhook trigger with `provider = "linear"`:

```toml
[[triggers]]
id = "linear-issues"
kind = "webhook"
provider = "linear"
path = "/hooks/linear"
match = { events = ["issue.update", "comment.create"] }
handler = "handlers::on_linear"
secrets = { signing_secret = "linear/webhook-secret" }
```

The connector verifies the raw request body against `Linear-Signature`, then
parses `webhookTimestamp` from the JSON payload and rejects stale deliveries.
The default replay tolerance is 75 seconds total:

- 60 seconds from Linear's recommended replay window
- 15 seconds of extra grace for clock skew

You can override the grace budget per trigger:

```toml
[[triggers]]
id = "linear-fast-window"
kind = "webhook"
provider = "linear"
path = "/hooks/linear"
match = { events = ["issue.update"] }
handler = "handlers::on_linear"
replay_grace_secs = 5
secrets = { signing_secret = "linear/webhook-secret" }
```

Successful deliveries normalize into `TriggerEvent` with:

- `kind` like `issue.update` or `comment.create`
- `dedupe_key` from `Linear-Delivery` when present
- `signature_status = { state: "verified" }`
- `provider_payload = LinearEventPayload`

For issue updates, `updatedFrom` becomes a typed `changes` array:

```harn
import "std/triggers"

fn on_linear(event: TriggerEvent) {
  let payload = event.provider_payload
  if payload.provider == "linear" && payload.event == "issue" {
    for change in payload.changes {
      println(change.field_name)
    }
  }
}
```

## Webhook health monitoring

Linear disables persistently failing webhooks and documents manual re-enable as
the fallback. Harn can optionally probe a health endpoint and call
`webhookUpdate(enabled: true)` once the service has recovered.

This is opt-in because the connector needs both the Linear webhook id and a
health URL it can probe:

```toml
[[triggers]]
id = "linear-issues"
kind = "webhook"
provider = "linear"
path = "/hooks/linear"
match = { events = ["issue.update"] }
handler = "handlers::on_linear"
secrets = {
  signing_secret = "linear/webhook-secret"
  access_token = "linear/access-token"
}

[triggers.monitor]
webhook_id = "790ce3f6-ea44-473d-bbd9-f3c73dc745a9"
health_url = "https://example.com/health"
probe_interval_secs = 60
success_threshold = 5
```

Notes:

- `webhook_id` is returned by `harn connect linear`
- `health_url` should be a public endpoint that returns `200 OK` when the
  orchestrator is healthy
- auth for the re-enable mutation can reuse the trigger-level `access_token` or
  `api_key`, or be overridden inside `[triggers.monitor]`
- `probe_interval_secs` defaults to 60 seconds
- `success_threshold` defaults to 5 consecutive successful probes

## Outbound GraphQL helpers

Import from `std/connectors/linear`:

```harn
import {
  configure,
  create_comment,
  graphql,
  issue_state_source,
  list_issues,
  search,
  update_issue,
  wait_until_issue_state,
} from "std/connectors/linear"
```

Shared configuration:

- `access_token` or `access_token_secret`
- `api_key` or `api_key_secret`
- optional `api_base_url`, defaulting to `https://api.linear.app/graphql`

Authentication follows Linear's documented header rules:

- personal API keys use `Authorization: <API_KEY>`
- OAuth tokens use `Authorization: Bearer <ACCESS_TOKEN>`

Example:

```harn
import {
  configure,
  create_comment,
  list_issues,
  update_issue,
} from "std/connectors/linear"

pipeline default() {
  configure({access_token_secret: "linear/access-token"})

  let issues = list_issues(
    {priority: {lte: 2}, state: {type: {eq: "started"}}},
    {first: 10},
  )
  let first = issues.nodes[0]
  update_issue(first.id, {title: first.title + " (triaged)"})
  create_comment(first.id, "Triaged by Harn.")
}
```

The generic `graphql(query, variables = nil, options = nil)` escape hatch
returns the raw GraphQL `data` plus response metadata. The connector also
surfaces complexity metadata from Linear's rate-limit headers, including the
observed complexity when the server reports `X-Complexity`.

## Monitor Helpers

`issue_state_source(issue_id, target_state, options = nil)` creates a
`std/monitors` source for waiting on a Linear issue state. It polls Linear's
GraphQL API and also wakes early from `issue` webhooks when the pushed issue
matches `issue_id` and the state `id`, `name`, or `type` matches
`target_state`.

Use `wait_until_issue_state(issue_id, target_state, options = nil)` for the
common case where the condition is simply reaching the target state.

## Webhook registration

`harn connect linear` creates a Linear webhook using the GraphQL
`webhookCreate` mutation and derives `resourceTypes` from the registered
Linear trigger events in the nearest `harn.toml`.

Team-scoped example:

```bash
harn connect linear \
  --url https://example.com/hooks/linear \
  --team-id 72b2a2dc-6f4f-4423-9d34-24b5bd10634a \
  --access-token-secret linear/access-token
```

Workspace-scoped example:

```bash
harn connect linear \
  --url https://example.com/hooks/linear \
  --all-public-teams \
  --api-key-secret linear/api-key
```

See [CLI reference](../cli-reference.md) for the full command surface.
