# System reminders

A **system reminder** is a typed transcript event with declared
lifecycle: visibility, persistence policy, dedupe key, time-to-live in
turns, sub-agent propagation policy, and a provider-rendering hint.
Reminders are the primitive Harn uses to inject ambient, ephemeral,
turn-boundary nudges into a running agent — token-pressure warnings,
file-changed alerts, post-tool truncation hints, post-compact awareness
prompts — without abusing the `user` role or polluting the durable
message history.

R-01 ships the **schema and event envelope**. The rest of the lifecycle
(stdlib providers, hook return variants, the bridge `agent/inject_reminder`
notification, the in-Harn `transcript.inject_reminder` builtin, compaction
honoring TTL + `preserve_on_compact`, sub-agent propagation, and
capability-aware rendering) lands in R-02 through R-12 under epic
[#1815](https://github.com/burin-labs/harn/issues/1815).

## Event shape

A reminder rides on the canonical transcript event envelope
(`{id, kind, role, visibility, text, blocks}`), so consumers that key
off the generic event shape ignore reminders cleanly. The lifecycle
fields live under `reminder`, and the same payload is mirrored under
`metadata` so observers that already key off the generic transcript-event
metadata slot see reminder context without learning a second field.

```json
{
  "id": "0190abcd-…",
  "kind": "system_reminder",
  "role": "developer",
  "visibility": "public",
  "text": "Approaching context window cap.",
  "blocks": [
    {
      "type": "text",
      "text": "Approaching context window cap.",
      "visibility": "public"
    }
  ],
  "reminder": {
    "id": "0190abcd-…",
    "tags": ["token_pressure"],
    "dedupe_key": "token_pressure",
    "ttl_turns": 3,
    "preserve_on_compact": true,
    "propagate": "session",
    "role_hint": "developer",
    "source": "stdlib_provider",
    "body": "Approaching context window cap.",
    "fired_at_turn": 4
  },
  "metadata": { "/* mirrors reminder */": "" }
}
```

### Lifecycle fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string (uuid) | yes | Stable identifier for de-duplication, audit, and replay. |
| `tags` | list of strings | yes (may be empty) | Free-form tag set. Canonical built-in tags include `token_pressure`, `file_changed`, `memory`, `post_tool`, `post_compact`. |
| `dedupe_key` | string or `nil` | optional | When set, a newer reminder with the same key supersedes older ones in the same transcript. |
| `ttl_turns` | int or `nil` | optional | Reminder expires after this many agent turns. `nil` means "persist until removed or compacted away." |
| `preserve_on_compact` | bool | yes | Hint to [`transcript_compact`](./builtins.md): when `true`, reminder events survive compaction. |
| `propagate` | `"all" \| "session" \| "none"` | yes | Sub-agent inheritance policy. `all` rides every spawned sub-agent transcript; `session` stays inside the originating session tree; `none` is opaque to children. |
| `role_hint` | `"system" \| "developer" \| "user_block" \| "ephemeral_cache"` | yes | Preferred provider rendering slot. The final wire role is decided at render time by the capability-aware dispatcher (R-06). |
| `source` | `"stdlib_provider" \| "hook" \| "bridge" \| "in_pipeline"` | yes | Where the reminder originated. |
| `body` | string | yes | The reminder text. Mirrored into `event.text` and `event.blocks[0].text`. |
| `fired_at_turn` | int | yes | Turn index when the reminder was fired. Pipelines with no turn counter pass `0`. |

`visibility` on the outer event defaults to `"public"` — reminders are
meant to influence the next turn — but reminders are never folded into
the durable `messages` list. They ride on the event log only.

## Building a reminder event

Pipelines build reminder events with the `transcript_reminder_event`
builtin. The dict accepts any subset of the lifecycle fields above and
fills in protocol defaults for the rest.

```harn,ignore
let evt = transcript_reminder_event({
  body: "Approaching context window cap.",
  tags: ["token_pressure"],
  dedupe_key: "token_pressure",
  ttl_turns: 3,
  preserve_on_compact: true,
  propagate: "session",
  role_hint: "developer",
  source: "stdlib_provider",
  fired_at_turn: 4,
})
// R-02+: hand the typed event to the session-scoped injection
// pathway (stdlib provider, hook return variant, bridge notification,
// or pipeline-level `transcript.inject_reminder`). R-01 ships the
// shape; the injection sites land in subsequent tickets under
// epic #1815.
```

Defaults applied when fields are omitted:

- `propagate` → `"session"`
- `role_hint` → `"system"`
- `source` → `"in_pipeline"`
- `preserve_on_compact` → `false`
- `tags` → `[]`
- `fired_at_turn` → `0`

Hosts and stdlib reminder providers reuse the same shape — the typed
[`SystemReminder`](https://docs.rs/harn-vm) Rust struct serde-round-trips
into and out of this dict.

## Reading reminders off a transcript

Reminder events are returned by the generic `transcript_events` and
`transcript_events_by_kind` builtins:

```harn,ignore
let reminders = transcript_events_by_kind(transcript, "system_reminder")
for evt in reminders {
  println(evt.reminder.body)
}
```

## Cross-references

- Epic: [#1815 — System Reminders & Ambient Context Injection](https://github.com/burin-labs/harn/issues/1815).
- Foundation ticket: [#1816 — SystemReminder transcript event kind + lifecycle schema](https://github.com/burin-labs/harn/issues/1816).
- Capability flags driving the `role_hint` → wire-role dispatch:
  [#1665](https://github.com/burin-labs/harn/issues/1665).
- Hook return variants that will surface `Reminder{...}` alongside
  `Allow` / `Deny` / `Modify`: see
  [Hooks (tool, persona, session lifecycle)](./extensibility/hooks.md).
