# System reminders

A **system reminder** is a typed transcript event with declared
lifecycle: visibility, persistence policy, dedupe key, time-to-live in
turns, sub-agent propagation policy, and a provider-rendering hint.
Reminders are the primitive Harn uses to inject ambient, ephemeral,
turn-boundary nudges into a running agent — token-pressure warnings,
file-changed alerts, post-tool truncation hints, post-compact awareness
prompts — without abusing the `user` role or polluting the durable
message history.

R-01 shipped the **schema and event envelope**. R-02 adds deterministic
in-Harn transcript transforms for injecting and clearing pending reminder
events, plus EventLog-backed dedupe and post-turn TTL expiry audit
records. R-03 lets tool, persona, step, and session hooks return reminder
effects that inject into the active session transcript. R-04 adds the
provider registry, four canonical stdlib providers, and
`register_reminder_provider(...)` for Harn-defined providers. R-05 adds
the bridge `session/remind` notification for host-injected reminder
events without routing them through user-role input. The rest of the
lifecycle (compaction honoring TTL + `preserve_on_compact`, sub-agent
propagation, and capability-aware rendering) lands in later tickets under epic
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

## Rendering pending reminders

`llm_call(...)` reads pending `system_reminder` events from the active
`session_id` transcript and renders them after `system_prompt_parts` and
the primary system prompt, but before `system_appendix` / `system_suffix`.
The final wire shape is capability-aware:

| Capability / hint | Rendering |
|---|---|
| `prefers_role_developer` | Separate `role: "developer"` messages. |
| Anthropic `role_hint: "user_block"` | A user content block containing `<system-reminder>...</system-reminder>`. |
| Anthropic `role_hint: "ephemeral_cache"` | Same user content block, with `cache_control: {type: "ephemeral"}` when prompt caching is supported. |
| `prefers_xml_scaffolding` | System-prompt text wrapped in `<system-reminder>` tags. |
| Fallback providers | Plain system-prompt text prefixed with `System reminder:`. |

Pipeline authors can pick a semantic `role_hint` once; provider
capabilities decide whether it becomes a developer message, a user content
block, XML system text, or plain system text.
`harn lint` emits `HARN-RMD-003` when a pipeline hardcodes
`role_hint: "user_block"` while also hardcoding a provider/model route
that cannot preserve that user-block shape.

## Injecting and clearing pending reminders

Use `transcript.inject_reminder(transcript, options)` when a Harn
pipeline wants to add a pending reminder to a transcript. It returns an
envelope instead of mutating the input:

```harn,ignore
let injected = transcript.inject_reminder(transcript(), {
  body: "Approaching context window cap.",
  tags: ["token_pressure"],
  dedupe_key: "token_pressure",
  ttl_turns: 3,
  preserve_on_compact: true,
  propagate: "session",
  role_hint: "developer",
})

let next_transcript = injected.transcript
let reminder_id = injected.reminder_id
```

The returned transcript has one additional `system_reminder` event and
the same durable message list as the input transcript. `body` is
required and must be non-empty. Optional `tags`, `dedupe_key`,
`ttl_turns`, `preserve_on_compact`, `propagate`, and `role_hint` fields
are validated; unknown option keys fail fast.

When `dedupe_key` is set, injection first removes any pending reminder
events with the same key from the input transcript. The new reminder is
then appended, and `deduped_count` reports how many older reminders were
replaced. When an active EventLog is installed, replacement also emits a
`transcript.reminder.deduped` record on
`transcript.reminder.lifecycle`.

Use `transcript.clear_reminders(transcript, selector)` to remove
pending reminders:

```harn,ignore
let cleared = transcript.clear_reminders(next_transcript, {
  tag: "token_pressure",
})
println(cleared.removed_count)
```

Selectors support `id`, `tag`, and `dedupe_key`. At least one selector
is required. If multiple selectors are present, a reminder must match
all of them to be removed. This builtin is also a pure transform and
returns `{transcript, removed_count}`.

Agent-session post-turn processing decrements finite `ttl_turns`
values. A reminder with `ttl_turns: 1` expires at the next post-turn
boundary, is removed from the session transcript events, and emits a
`transcript.reminder.expired` record on
`transcript.reminder.lifecycle` when an active EventLog is installed.
`transcript_compact(...)` applies the same TTL decrement at the
pre-compaction boundary before it rebuilds the transcript. It drops
expired reminders, dedupes matching `dedupe_key` values to the newest
event, preserves only reminders with `preserve_on_compact: true`, and
passes all surviving reminder payloads to custom compactors as their
second argument.
Hooks can inject reminders by returning `{reminder: {...}, then?: ...}`,
a bare reminder spec such as `{body: "Refresh context"}`, or a
session-hook effect list such as `[{reminder: {...}}]`. Bridge
notifications use the same reminder spec and provider-specific rendering
happens at the next LLM call.

## Reminder providers

`agent_loop(...)` enables stdlib reminder providers by default. Providers
observe lifecycle events and inject pending `system_reminder` events into
the active session transcript. Bare `llm_call(...)` does not fire
providers.

Canonical providers:

| Provider | Event | Reminder |
|---|---|---|
| `token_pressure` | `on_budget_threshold` | Fires near 70/85/95% of the context window; tag `token_pressure`, dedupe key `token_pressure`, `ttl_turns: 2`, and `preserve_on_compact: true` at the critical threshold. |
| `idle_nudge` | `session_idle` | Fires after the daemon idle interval reaches the configured threshold (default 60s); tag `idle`, `ttl_turns: 1`. |
| `tool_output_truncated` | `post_tool_use` | Fires when post-tool hooks compact or truncate output before it reaches the model; tag `truncation`, `ttl_turns: 1`. |
| `post_compact_recap` | `post_compact` | Fires after transcript compaction with the current recap; tag `recap`, `ttl_turns: 2`. |

Disable providers per loop with the `reminders.providers` opt-out list:

```harn,ignore
agent_loop(task, system, {
  reminders: {
    providers: ["-token_pressure", "-idle_nudge"],
  },
})
```

Provider-specific configuration lives under `reminders.config`:

```harn,ignore
agent_loop(task, system, {
  reminders: {
    config: {
      token_pressure: {context_window: 128000},
      idle_nudge: {idle_seconds: 120},
    },
  },
})
```

Register a Harn provider with `register_reminder_provider({id,
subscribes_to, evaluate})`. The `evaluate` closure receives
`{event, session, session_id, payload, options, config}` and returns a
reminder effect, a bare reminder spec, an effect list, or `nil`:

```harn,ignore
register_reminder_provider({
  id: "custom_echo",
  subscribes_to: ["session_idle"],
  evaluate: { ctx -> return {
    reminder: {
      body: "Custom reminder: " + ctx.payload.note,
      tags: ["custom"],
      dedupe_key: "custom_echo",
      ttl_turns: 2,
    },
  } },
})
```

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
- Transform builtins: [#1817 — `transcript.inject_reminder()` + `transcript.clear_reminders()`](https://github.com/burin-labs/harn/issues/1817).
- Capability flags driving the `role_hint` → wire-role dispatch:
  [#1665](https://github.com/burin-labs/harn/issues/1665).
- Hook return variants that surface reminders alongside `Allow` /
  `Deny` / `Modify`: see
  [Hooks (tool, persona, session lifecycle)](./extensibility/hooks.md).
