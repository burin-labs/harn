# Trigger event schema

`TriggerEvent` is the normalized envelope every inbound trigger provider
converges on before dispatch. Connectors preserve provider-specific payload
fidelity inside `provider_payload`, but the orchestration layer always sees the
same outer shape:

```harn
import "std/triggers"

fn on_event(event: TriggerEvent) {
  let payload = event.provider_payload
  if payload.provider == "github" && payload.event == "issues" {
    println(payload.issue.title ?? "unknown")
  }

  let signature = event.signature_status
  if signature.state == "failed" {
    println(signature.reason)
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
payload is already narrowed into the six MVP event families (`issues`,
`pull_request`, `issue_comment`, `pull_request_review`, `push`, and
`workflow_run`) with event-specific top-level fields such as `issue`,
`pull_request`, `comment`, `review`, `commits`, and `workflow_run`. Slack's
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
