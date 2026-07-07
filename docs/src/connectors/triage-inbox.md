# Triage inbox envelopes

`std/triage` defines the portable inbox event Harn emits for dashboard surfaces
such as Burin Home's Start My Day feed. Connector payloads remain available for
audit, but hosts render cards from normalized fields so they do not need
Slack-, Notion-, or GitHub-specific branching.

```harn
import { triage_emit, triage_start_my_day } from "std/triage"

const feed = triage_start_my_day(
  [github_event, slack_event, notion_event],
  {emit: true, topic: "triage.inbox.start_my_day"},
)

for event in feed.events {
  log(event.summary)
}
```

## Envelope

`triage_normalize(input)` returns `harn.triage_event.v1`:

| Field | Purpose |
|---|---|
| `provider` | Source provider such as `github`, `slack`, or `notion` |
| `source_kind` | Provider event kind, for example `issues.opened` or `page.content_updated` |
| `source_url` | Mandatory source URL or deep link used for provenance and host navigation |
| `source_timestamp` | Provider-reported source time when available |
| `actors` | Provider-neutral actor list with `kind`, `id`, optional display name, and URL |
| `summary` | Short host-renderable card title/body |
| `why_it_matters` | Human-readable reason the item deserves attention |
| `proposed_action` | Suggested next step; not an executed write |
| `urgency`, `priority`, `confidence` | Host sorting and ranking signals |
| `related_refs` | Source-linked references, always including the source item |
| `dedupe_key` | Stable source-derived key for duplicate detection |
| `privacy` | Redaction and sensitivity flags |
| `action_intents` | Host-owned commands such as dismiss, snooze, or convert-to-task |
| `raw_payload` | Provider payload kept separate from normalized render fields |

`source_url` is required. Slack payloads can satisfy it with a `slack://` deep
link derived from team, channel, and timestamp fields. Notion payloads use a
page URL when present and otherwise fall back to a Notion entity URL.

## Dedupe

`triage_dedupe_key(provider, source_kind, source_url, source_id?)` hashes
provider-neutral source provenance, not webhook delivery ids. A redelivered
GitHub issue webhook with a different delivery id therefore maps to the same
triage event key when the issue URL and source id are unchanged.

Use `triage_dedupe_events(events)` before rendering or emitting a feed:

```harn
import { triage_dedupe_events } from "std/triage"

const _unique = triage_dedupe_events([github_delivery, github_delivery_retry])
```

## EventLog receipts

`triage_emit(event, {topic?})` validates the envelope, appends it as
`kind = "triage_event"` to `triage.inbox.events` by default, and returns
`harn.triage_event_emit_receipt.v1` with the EventLog id. The payload itself is
the normalized event, so Harn receipt/provenance builders can serialize the
same shape without a dashboard-specific API resource.

## Action boundary

Action intents describe possible host actions only. They do not mutate Slack,
Notion, GitHub, or host task state by themselves. Any non-navigation intent must
carry `requires_approval: true`; `triage_validate` rejects write-like intents
that omit that gate. This keeps dismiss, snooze, and convert-to-task operations
separate from connector normalization.
