# System reminders

System reminders are typed, ephemeral transcript injections for a running
agent session. They let Harn add ambient context at turn boundaries, such
as token-pressure warnings, idle-session nudges, truncated-tool-output
notices, post-compact recaps, or host file-change alerts, without
pretending that context came from the user and without adding it to the
durable message list.

A reminder is always a structured `system_reminder` transcript event. It
has a stable lifecycle shape, can be deduped, can expire after a finite
number of turns, can choose whether it survives compaction, and can state
how far it should propagate to sub-agents. Rendering is provider-aware:
the same reminder can become developer-role content, system prompt text,
or an Anthropic-style `<system-reminder>` user content block depending on
the selected model route. Lifecycle events are observable through
EventLog records and host-facing ACP `session/update` metadata.

## What and why

Use reminders when the current agent should keep going, but needs fresh
ambient context before its next model turn. Triggers are the different
primitive: they start or schedule work. Reminders modify an already
running session.

Good reminder candidates are transient facts that should be visible now
but not preserved forever:

- context-window pressure
- a file changed while the agent was idle
- a tool result was truncated before the model saw it
- compaction replaced older turns with a recap
- workspace memory produced a temporary caution for the current task

Do not use reminders for durable user requirements, audit records,
long-lived memory, or background task dispatch. Put those in the normal
user/system prompt path, receipts, memory, or triggers.

## ReminderSpec shape

Every producer ultimately normalizes to the same `ReminderSpec` lifecycle
fields:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `body` | string | required | Reminder text rendered into the next model turn. Must be non-empty for strict producer APIs. |
| `tags` | list of strings | `[]` | Tags for querying, clearing, diagnostics, and provider conventions. |
| `dedupe_key` | string or `nil` | `nil` | Newer pending reminders with the same key replace older pending reminders. |
| `ttl_turns` | positive int or `nil` | `nil` | Finite reminders expire after this many post-turn lifecycle passes. `nil` persists until cleared or compacted away. |
| `preserve_on_compact` | bool | `false` | When `true`, compaction copies the reminder event into the compacted transcript. |
| `propagate` | `"all"`, `"session"`, or `"none"` | `"session"` | Controls sub-agent inheritance. |
| `role_hint` | `"system"`, `"developer"`, `"user_block"`, or `"ephemeral_cache"` | `"system"` | Preferred rendering slot; provider capabilities still decide the final wire shape. |
| `source` | `"stdlib_provider"`, `"hook"`, `"bridge"`, `"in_pipeline"`, or `"inherited"` | producer-specific | Origin marker used by audit and propagation. |
| `fired_at_turn` | int | producer-specific | Turn index when the reminder was created. Pipelines without a turn counter use `0`. |
| `id` | string | generated | Stable reminder id for audit, replay, and clearing. |
| `originating_agent_id` | string or `nil` | `nil` | Set when a reminder is inherited from a parent session. |

The transcript event uses the standard event envelope plus a typed
`reminder` payload. `metadata` mirrors that payload so generic transcript
observers can inspect the same fields without special-casing the
`reminder` slot.

```json
{
  "id": "0190abcd-...",
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
    "id": "0190abcd-...",
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
  "metadata": {
    "id": "0190abcd-...",
    "tags": ["token_pressure"],
    "dedupe_key": "token_pressure",
    "ttl_turns": 3,
    "preserve_on_compact": true,
    "propagate": "session",
    "role_hint": "developer",
    "source": "stdlib_provider",
    "body": "Approaching context window cap.",
    "fired_at_turn": 4
  }
}
```

`transcript_reminder_event({...})` is the lenient event builder for
pipeline code that already has a normalized reminder-like dict. The
stricter producer APIs below validate required fields and enum values.

## Producing reminders

### From a pipeline

Use `transcript.inject_reminder(transcript, options)` to append a pending
reminder event to a transcript. It is a pure transform: the input
transcript is unchanged and the returned `transcript` contains the new
event.

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
log(injected.reminder_id)
log(injected.deduped_count)
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
`transcript.reminder.lifecycle`; every successful injection emits
`transcript.reminder.injected`.

Use `transcript.clear_reminders(transcript, selector)` to remove
pending reminders:

```harn,ignore
let cleared = transcript.clear_reminders(next_transcript, {
  tag: "token_pressure",
})
log(cleared.removed_count)
```

Selectors support `id`, `tag`, and `dedupe_key`. At least one selector
is required. If multiple selectors are present, a reminder must match all
of them to be removed. This builtin is also a pure transform and returns
`{transcript, removed_count}`.

### From a hook

Tool, persona, step, and session hooks can return reminder effects from
events that support transcript mutation. A hook may return
`{reminder: {...}, then?: ...}`, a bare reminder spec, or a session-level
effect list.

```harn,ignore
register_tool_hook({
  pattern: "read_file",
  post: { ctx ->
    if ctx.result.truncated {
      return {
        reminder: {
          body: "The file read was truncated; inspect the specific range before editing.",
          tags: ["truncation"],
          dedupe_key: "read_file:truncated",
          ttl_turns: 1,
          propagate: "none",
        },
      }
    }
  },
})
```

Worker lifecycle events are observational and reject reminder effects with
`HARN-RMD-008`. Move mutating reminders to a session, tool, step, or
persona hook.

### From a provider

`agent_loop(...)` evaluates canonical reminder providers by default.
Register a custom provider with `register_reminder_provider({id,
subscribes_to, evaluate})`. The `evaluate` closure receives
`{event, session, session_id, payload, options, config}` and may return
`nil`, a bare reminder spec, a `{reminder: {...}}` effect, or a list of
effects.

```harn,ignore
register_reminder_provider({
  id: "workspace_guard",
  subscribes_to: ["session_idle"],
  evaluate: { ctx ->
    if ctx.config?.enabled == false {
      return nil
    }
    return {
      reminder: {
        body: "Workspace may have changed while idle; re-check touched files.",
        tags: ["workspace"],
        dedupe_key: "workspace:idled",
        ttl_turns: 1,
        propagate: "session",
      },
    }
  },
})

agent_loop(task, system, {
  reminders: {
    config: {
      workspace_guard: {enabled: true},
    },
  },
})
```

`clear_reminder_providers()` removes user-defined providers, mainly for
tests. Canonical stdlib providers remain available through `agent_loop`
unless disabled with reminder options.

### From a host bridge

Bridge and ACP-style hosts inject ambient context with the `session/remind`
JSON-RPC notification. This is distinct from `session/inject`, which is always
user-role input.

```json
{
  "jsonrpc": "2.0",
  "method": "session/remind",
  "params": {
    "body": "The workspace changed while you were idle; re-read src/lib.rs before editing.",
    "tags": ["workspace"],
    "dedupe_key": "workspace-change",
    "ttl_turns": 2,
    "role_hint": "system",
    "mode": "interrupt_immediate",
    "_meta": {
      "harn": {
        "origin": "file-watcher"
      }
    }
  }
}
```

`mode` accepts the same delivery values as queued user messages:
`interrupt_immediate`, `finish_step`, and `audit_only`. The mode
determines *which* agent-loop seam drains the reminder:

| Mode | Drained at | Behavior |
|---|---|---|
| `interrupt_immediate` | `turn_start`, `pre_tool_dispatch`, `post_tool_dispatch`, `turn_end`, `daemon_idle_pre`, `daemon_idle_post` | When it arrives at `pre_tool_dispatch` (between LLM return and tool dispatch), the pending tool batch is **skipped** and the reminder lands in the next iteration's prompt. At every other seam, the reminder is appended to the transcript and visible on the next prompt build. |
| `finish_step` | `turn_start`, `post_tool_dispatch`, `turn_end` | Drained at iteration boundaries only. Never short-circuits a tool batch. The model sees this reminder on the *next* prompt build — including the final iteration when the loop is about to terminate. |
| `audit_only` | `loop_exit` | Drained once the loop terminates, before finalize. The reminder lands in the transcript audit, but the model **never sees it** — no further LLM call runs after `loop_exit`. Use this for record-keeping and replay; use `finish_step` if the model must react to the reminder before the agent terminates (harn#2212). |

See [steering seams](concepts/steering-seams.md) for the full seam catalog.

Host-specific extension fields belong under `_meta`. Invalid reminder
payloads fail with `HARN-RMD-002`; unknown top-level reminder options
fail with `HARN-RMD-001`.

## Canonical stdlib providers

Canonical providers are enabled by default inside `agent_loop(...)`.
Bare `llm_call(...)` renders pending reminders from the selected session
but does not evaluate providers.

| Provider | Subscribes to | Config keys | Reminder behavior |
|---|---|---|---|
| `token_pressure` | `on_budget_threshold` | `context_window` | Fires at about 70%, 85%, and 95% context use. Uses tag and dedupe key `token_pressure`, `ttl_turns: 2`, `role_hint: "developer"`, `propagate: "session"`, and sets `preserve_on_compact: true` at the critical threshold. |
| `idle_nudge` | `session_idle` | `idle_seconds` or `seconds` | Fires when daemon idle wake interval reaches the threshold, defaulting to 60 seconds. Uses tag `idle`, dedupe key `idle_nudge`, `ttl_turns: 1`, and `propagate: "none"`. |
| `tool_output_truncated` | `post_tool_use` | none | Fires when tool output was truncated or compacted before the model saw it. Uses tag `truncation`, dedupe key `tool_output_truncated:<tool_name>`, `ttl_turns: 1`, and `propagate: "none"`. |
| `post_compact_recap` | `post_compact` | none | Fires after compaction archives messages. Uses tag `recap`, dedupe key `post_compact_recap`, `ttl_turns: 2`, and `propagate: "session"`. |
| `resume_continuity` | `worker_resumed` | none | Fires before the first resumed `agent_loop` turn. Uses tag and dedupe key `resume_continuity`, `ttl_turns: 1`, `preserve_on_compact: true`, and `propagate: "none"`; includes suspension reason, resume cause, optional resume input, and a pre-suspend digest for `continue_transcript: false`. |
| `project_facts` | `session_start`, `on_budget_threshold` | `namespace`, `scope`, `root`, `max_facts`, `min_confidence`, `kind_filter`, `relevance_query`, `refresh_ratio` | Recalls typed `harn.fact.v1` records via lexical (BM25) match against the session task and top-K by score (records that BM25 cannot score still surface, ranked by recency). Defaults: namespace `project/facts`, `max_facts: 5`, `min_confidence: 0.5`, query derived from the session task. Uses tag and dedupe key `project_facts`, `ttl_turns: 1`, and `propagate: "session"` so the same body refreshes on context-budget pressure and re-injects on the next session start. The `on_budget_threshold` refresh only fires once context usage crosses `refresh_ratio` (default 0.70) so per-turn recall stays bounded. Skipped silently when no schema-matched facts pass the filter. |

Disable every provider with `reminders: false` or
`reminders: {enabled: false}`. Disable selected providers by prefixing the
provider id with `-`:

```harn,ignore
agent_loop(task, system, {
  reminders: {
    providers: ["-token_pressure", "-idle_nudge"],
    config: {
      token_pressure: {context_window: 128000},
      idle_nudge: {idle_seconds: 120},
    },
  },
})
```

The provider list has a hard diagnostic guard: enabling more than eight
distinct providers raises `HARN-RMD-007`.

## Capability-aware rendering

`llm_call(...)` loads pending reminders from the active `session_id`
transcript, renders them after `system_prompt_parts` and the primary
system prompt, and before `system_appendix` / `system_suffix`.

Rendering follows provider capabilities first, then `role_hint`:

| Provider capability / reminder hint | Wire shape |
|---|---|
| `prefers_role_developer` | Separate `role: "developer"` message containing `System reminder:\n...`. This wins for all hints. |
| Anthropic wire format plus `role_hint: "user_block"` | Prepended user content block containing `<system-reminder>...</system-reminder>`. |
| Anthropic wire format plus `role_hint: "ephemeral_cache"` | Same user content block, with `cache_control: {"type": "ephemeral"}` when prompt caching is supported. |
| `prefers_xml_scaffolding` | System prompt text wrapped in `<system-reminder>` tags. |
| Fallback providers | Plain system prompt text prefixed with `System reminder:`. |

`role_hint` is a request, not a guarantee. Use provider-neutral
`"system"` or `"developer"` unless the script is intentionally targeting
an Anthropic-style user block. `HARN-RMD-003` reports hardcoded
`role_hint: "user_block"` with a provider route that cannot preserve that
shape.

## Compaction interaction

Reminders live in transcript events, not durable messages. Normal
agent-session post-turn processing decrements finite `ttl_turns`.
`ttl_turns: 1` expires at the next post-turn boundary and emits an expiry
lifecycle event when an EventLog is active.

`transcript_compact(...)` applies reminder lifecycle processing before it
rebuilds the transcript:

- finite TTLs decrement at the pre-compaction boundary
- expired reminders are dropped
- reminders sharing a `dedupe_key` collapse to the newest event
- only `preserve_on_compact: true` reminders are copied into the compacted
  transcript
- custom compactors receive surviving reminder payloads as their second
  argument

```harn,ignore
let compacted = transcript_compact(snapshot, {
  strategy: "custom",
  custom_compactor: { messages, reminders ->
    return transcript({
      messages: [
        {role: "system", content: "Summary plus " + str(len(reminders)) + " active reminders."},
      ],
    })
  },
})
```

Common gotcha: `preserve_on_compact: false` plus no finite `ttl_turns`
creates a reminder that can live forever during normal turns but vanish
at the next compaction. `HARN-RMD-004` flags this shape. Add a finite
TTL, or set `preserve_on_compact: true` when the reminder is meant to be
durable across compaction.

## Sub-agent propagation

When a parent agent hands work to a child session, Harn filters pending
reminders into the handoff envelope's `reminder_propagation` field. The
child seeds inherited reminders into its transcript before its first
turn. Inherited copies rewrite `source` to `"inherited"` and set
`originating_agent_id` to the session that first emitted the reminder.

| `propagate` | Behavior |
|---|---|
| `"all"` | Inherit into direct children and allow inherited copies to continue into deeper descendants. |
| `"session"` | Inherit into direct children from the originating session only; inherited copies are not re-forwarded. |
| `"none"` | Keep local to the current session. |

Use `"all"` sparingly for durable policy context. Use `"session"` for
task-scoped context that direct helpers need. Use `"none"` for local
observations such as truncated tool output or daemon idle nudges.

## Diagnostic codes

The reminder lifecycle diagnostic family is `HARN-RMD-NNN`. Use
`harn explain HARN-RMD-005` or the
[diagnostic catalog](./diagnostics.md#rmd--reminder-lifecycle) for the
current explanation and remediation.

| Code | Meaning |
|---|---|
| `HARN-RMD-001` | Unknown option key or invalid strict producer option shape. |
| `HARN-RMD-002` | Invalid bridge reminder payload shape. |
| `HARN-RMD-003` | `user_block` role hint is unsupported by the selected provider route. |
| `HARN-RMD-004` | Discardable reminder has no finite TTL. |
| `HARN-RMD-005` | Unknown `propagate` value. |
| `HARN-RMD-006` | Reminder provider returned a malformed reminder spec. |
| `HARN-RMD-007` | Too many reminder providers are enabled. |
| `HARN-RMD-008` | Hook event does not support reminder effects. |

## EventLog events

Reminder producers and lifecycle boundaries emit audit records on the
`transcript.reminder.lifecycle` topic when an EventLog is active. Each
payload carries `session_id`, `task_id`, and `agent_id` so host and
replay tooling can correlate reminder state with the active agent turn
and tool call. Use `event_log.subscribe(...)` with
`kind_prefix: "transcript.reminder."` to stream only reminder lifecycle
events.

| Event kind | Emitted when | Key payload fields |
|---|---|---|
| `transcript.reminder.injected` | A new reminder enters the pending queue. | `reminder_id`, `tags`, `dedupe_key`, `source`, `role_hint`, `ttl_turns`, `propagate` |
| `transcript.reminder.fired` | A pending reminder is rendered into the next provider request. | `reminder_id`, `turn_number`, `rendered_role` |
| `transcript.reminder.deduped` | A new reminder or compaction replaces older pending reminders with the same `dedupe_key`. | `replaced_id`, `replacing_id`, `dedupe_key` |
| `transcript.reminder.expired` | A reminder is removed by TTL expiry, compaction, or an explicit clear operation. | `reminder_id`, `reason` (`ttl`, `compaction`, or `cleared`) |
| `transcript.reminder.dropped` | A malformed or unrenderable reminder cannot be used for the next provider request. | `reminder_id`, `reason` (`invalid` or `capability_mismatch`) |
| `transcript.reminder.inherited` | A child session receives an inherited reminder. | `reminder_id`, `originating_agent_id`, `sub_agent_id`, `propagate` |
| `transcript.reminder.provider_evaluated` | A registered provider evaluated against a hook event. | `provider_id`, `event`, `fired` |

Rendered reminders also surface to ACP clients as `session/update`
notifications with `sessionUpdate: "reminder_emitted"` and
`_meta.harn.reminder` metadata. Tool-call hook reminders attach a compact
lifecycle report to `ToolCallReceipt.audit.reminders` when
`with_audit_log(...)` is active. The receipt report identifies the hook
event, tool call, reminder id, tags, dedupe key, source, role hint, TTL,
and dedupe count without copying the reminder body into the receipt.

## Cookbook

### Token-pressure thresholds

Use the canonical provider and override only the context window when the
route or harness cannot infer it.

```harn,ignore
agent_loop(task, system, {
  reminders: {
    config: {
      token_pressure: {context_window: 128000},
    },
  },
})
```

### Project facts at session start

Persist project facts with `store_fact` so the canonical `project_facts`
provider can recall them on the next session. Tune the namespace and
filters per loop when the defaults are too broad.

```harn,ignore
store_fact({
  kind: "Decision",
  claim: "Build cancels in-flight tool calls on SIGINT before clearing state.",
  confidence: 0.92,
  evidence: [{kind: "FileRange", ref: "crates/harn-vm/src/cancel.rs:1"}],
})

agent_loop(task, system, {
  reminders: {
    config: {
      project_facts: {
        max_facts: 8,
        min_confidence: 0.6,
        kind_filter: ["decision", "constraint"],
      },
    },
  },
})
```

### File-changed reminder from a hook

Use a dedupe key per file so repeated file-watcher events collapse into
one pending nudge.

```harn,ignore
register_session_hook("file_edited", { event ->
  let path = to_string(event?.path ?? "")
  return {
    reminder: {
      body: "File changed externally: " + path + ". Re-read it before editing.",
      tags: ["workspace", "file_changed"],
      dedupe_key: "file_changed:" + path,
      ttl_turns: 2,
      propagate: "session",
    },
  }
})
```

### Memory-derived reminder

Use `propagate: "all"` only when downstream agents need the same caution.

```harn,ignore
let memory_warning = "Customer prefers patch-sized PRs and explicit verification."
let injected = transcript.inject_reminder(transcript(), {
  body: memory_warning,
  tags: ["memory"],
  dedupe_key: "memory:customer-pr-style",
  ttl_turns: 4,
  preserve_on_compact: true,
  propagate: "all",
  role_hint: "developer",
})
```

### Clear stale reminders after the condition resolves

Clear by `dedupe_key` for singleton reminders and by `tag` for groups.

```harn,ignore
let cleared = transcript.clear_reminders(current_transcript, {
  dedupe_key: "workspace-change",
})
```

### Host-injected idle workspace nudge

Bridge hosts should use `session/remind`, not `session/inject`, when the
content is ambient context rather than user text.

```json
{
  "jsonrpc": "2.0",
  "method": "session/remind",
  "params": {
    "body": "Dependencies changed while the agent was idle; rerun the narrow test before continuing.",
    "tags": ["workspace", "deps"],
    "dedupe_key": "workspace:deps",
    "ttl_turns": 1,
    "propagate": "none",
    "mode": "finish_step"
  }
}
```

### Provider-specific ephemeral cache hint

Use `ephemeral_cache` only when the route supports Anthropic-style user
content blocks and prompt caching.

```harn,ignore
transcript.inject_reminder(transcript(), {
  body: "Large policy excerpt applies for this turn only.",
  tags: ["policy"],
  dedupe_key: "policy:turn",
  ttl_turns: 1,
  role_hint: "ephemeral_cache",
  propagate: "none",
})
```

### Custom compactor reads reminders

Custom compactors receive the surviving reminder payloads after TTL and
dedupe processing.

```harn,ignore
transcript_compact(snapshot, {
  strategy: "custom",
  custom_compactor: { messages, reminders ->
    for reminder in reminders {
      log(reminder.body)
    }
    return transcript({messages: messages})
  },
})
```

## Cross-references

- Epic: [#1815 -- System Reminders and Ambient Context Injection](https://github.com/burin-labs/harn/issues/1815)
- Provider capability flags: [#1665](https://github.com/burin-labs/harn/issues/1665)
- Host-neutral context envelope: [#1680](https://github.com/burin-labs/harn/issues/1680)
- Hook recipes and background context maintenance: [#1681](https://github.com/burin-labs/harn/issues/1681)
- Hooks reference: [Hooks (tool, persona, session lifecycle)](./extensibility/hooks.md)
- Bridge reminder injection: [Bridge protocol](./bridge-protocol.md#daemon-idleresume-notifications)
- Builtin reference: [Builtin functions](./builtins.md)
