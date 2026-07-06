# Trigger event schema

`TriggerEvent` is the normalized envelope every inbound trigger provider
converges on before dispatch. Connectors preserve provider-specific payload
fidelity inside `provider_payload`, but the orchestration layer always sees the
same outer shape:

```harn
import "std/triggers"

fn on_event(event: TriggerEvent) {
  const payload = event.provider_payload
  if payload.provider == "github" && payload.event == "issues" {
    log(payload.issue.title ?? "unknown")
  }

  const signature = event.signature_status
  if signature.state == "failed" {
    log(signature.reason)
  }
}
```

## Envelope fields

`TriggerEvent` carries:

- `id`: runtime-assigned event id.
- `provider`: provider identity such as `"github"`, `"slack"`, `"cron"`, or `"webhook"`.
- `kind`: provider-specific event kind.
- `received_at`: RFC3339 timestamp captured by the runtime.
- `occurred_at`: provider-reported RFC3339 timestamp when available.
- `dedupe_key`: delivery id or equivalent idempotency key.
- `trace_id`: trace correlation id propagated through dispatch.
- `tenant_id`: optional orchestrator-assigned tenant namespace.
- `headers`: redacted provider headers retained for audit/debugging.
- `provider_payload`: provider-tagged payload union.
- `signature_status`: typed verification result.

## Signature status

`signature_status` is a discriminated union:

- `{ state: "verified" }`
- `{ state: "unsigned" }`
- `{ state: "failed", reason: string }`

Unsigned events are valid for synthetic sources such as cron. Failed events can
still be logged for audit purposes even if the dispatcher rejects them.

## Provider payloads

The initial `std/triggers` payload aliases are intentionally small. Each
provider variant exposes a stable normalized surface plus `raw: dict`. GitHub's
payload is narrowed into thirteen event families covering the
[`harn-github-connector`](https://github.com/burin-labs/harn-github-connector)
v0.2.0 webhook contract — `issues`, `pull_request`, `issue_comment`,
`pull_request_review`, `push`, `workflow_run`, `deployment_status`,
`check_run`, `check_suite`, `status`, `merge_group`, `installation`,
and `installation_repositories` — with event-specific top-level fields
such as `issue`, `pull_request`, `comment`, `review`, `commits`,
`workflow_run`, `check_suite_id`, `merge_group_id`,
`repositories_removed`, plus the connector-promoted `topic`,
`repository`, and `repo` shared across every variant. Slack's
payload is narrowed into `Message` (`message.*`), `AppMention`,
`ReactionAdded`, `AppHomeOpened`, and `AssistantThreadStarted`. Notion's
payload is narrowed around the current connector landing:
`subscription.verification`, `page.content_updated`, `page.locked`,
`comment.created`, `data_source.schema_updated`, plus polled fallback events
surfaced through `payload.polled`. All providers still preserve the full outer
envelope in `raw`:

- `GitHubEventPayload`
- `SlackEventPayload`
- `LinearEventPayload`
- `NotionEventPayload`
- `CronEventPayload`
- `GenericWebhookPayload`
- `A2aPushPayload`
- `ExtensionProviderPayload`

The runtime registers these through a `ProviderCatalog`, so future connectors
can contribute new payload schemas without rewriting the top-level
`TriggerEvent` envelope.

## Header redaction

The runtime keeps delivery, event, timestamp, request-id, signature, and
user-agent headers by default. It redacts sensitive headers such as
`Authorization`, `Cookie`, and names containing `secret`, `token`, or `key`
unless they are explicitly allow-listed as safe metadata.
