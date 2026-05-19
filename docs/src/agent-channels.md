# Agent channels

Agent channels are a typed, durable pub/sub primitive for orchestrating
multiple agents (and the triggers that watch them). One agent calls
`emit_channel(name, payload)`; any number of subscribers — declared as
`channel.emit` triggers — react. Channel events land on the active
EventLog, so they survive process restarts, feed the replay oracle, and
show up in the action graph alongside webhook and cron events.

This page covers the channel surface end to end. For a one-screen LLM
quickref see the "Durable agent channels" section in
`docs/llm/harn-quickref.md`. For runnable patterns see
[Channel cookbook](./cookbooks/channels.md).

## When to use channels

Channels are one of four ways to wire agents to events in Harn. Pick the
narrowest one that fits.

| Goal | Use | Why |
|---|---|---|
| Hand off a request to one specific agent | Handoffs (`handoff(...)`, `@handoff`) | Single producer, single typed receiver. No fan-out, no buffering. |
| React to an external system (GitHub, Slack, cron, Kafka) | Triggers with a provider source | The trigger system already owns signature checks, dedupe, replay, and DLQ for external traffic. |
| Park one running agent until a specific external event arrives | Suspend/resume with `agent_await_resumption(reason, conditions)` | The waiting agent keeps its transcript; you only need a resume condition, not a long-lived subscriber. |
| Emit a typed event to N subscribers — including triggers that may not exist yet | **Channels** (`emit_channel(...)` + `channel.emit` trigger) | One producer, many consumers, durable journal, replay-compatible. |
| Inject a periodic prompt into a running agent loop | **Channels + `batch` + `ReminderInject`** | Aggregate N emits into one reminder, dropped onto the target session at its next turn boundary. |

Two anti-patterns worth calling out:

- **Channels are not in-process queues.** They write to the durable
  EventLog and fire the trigger dispatcher. If you want a fast
  intra-pipeline mailbox, use `channel(...)` / `send` / `receive` (the
  concurrency channel primitive — see `docs/src/concurrency.md`).
- **Channels are not RPC.** There is no return value beyond the emit
  receipt. If you need an answer back, the consumer emits a second
  channel and the original producer subscribes to it (recipe 3 below).

## Emit a channel

`emit_channel(name, payload, options?)` appends one channel event and
returns a receipt:

```harn,ignore
let receipt = emit_channel("pr.merged", {
  repo: "burin-labs/harn",
  number: 1984,
  sha: "3e949640",
})
println(receipt.event_id)
println(receipt.name_resolved)  // "tenant:default:pr.merged"
println(receipt.emitted_at.signature.starts_with("sha256:"))
```

The receipt shape is:

```text
{
  event_id: string,           // log id, unique per emit
  id: string,                 // idempotency id (= options.id when set)
  name_resolved: string,      // "<scope>:<scope_id>:<name>"
  scope: "session" | "pipeline" | "tenant",
  scope_id: string,
  emitted_at: {at_ms, at, algorithm, key_id, signature},
  emitted_by: string,
  retention: "process" | "durable",
  duplicate: bool,            // true when options.id collided with a prior emit
}
```

### Scopes

A bare name resolves to **tenant** scope: `emit_channel("pr.merged",
payload)` is `tenant:<current-tenant-or-default>:pr.merged`. Add a
prefix to pick a different scope:

| Prefix | Resolved name | When |
|---|---|---|
| (none) | `tenant:<current>:<name>` | Default. Cross-session, cross-pipeline within a tenant. |
| `session:<name>` | `session:<current-session>:<name>` | In-process, single agent session. Cleared with `reset_channel_state()`. |
| `pipeline:<name>` | `pipeline:<current-pipeline>:<name>` | Scoped to one pipeline run. Requires an active pipeline context. |
| `tenant:<tenant_id>:<name>` | `tenant:<tenant_id>:<name>` | Explicit tenant — must match the active tenant or fail `HARN-CHN-002`. |
| `org:<org_id>:<name>` | — | Reserved. Currently fails `HARN-CHN-002` until org grants ship. |

Scope id can also be passed as an option:

```harn,ignore
emit_channel("worker.ready", {worker: "lint"}, {
  scope: "session",
  session_id: "review-bot-123",
  id: "worker-ready-lint",  // idempotency key
  ttl: 10m,                 // event-log retention hint
})
```

Cross-scope isolation is automatic: distinct `tenant_id`, `session_id`,
or `pipeline_id` values resolve to distinct topics, so a reader against
the wrong scope id sees an empty view.

### Idempotency

Setting `options.id` makes the emit idempotent. Re-emitting the same
(`name_resolved`, `id`) pair is a no-op on the journal; the receipt is
returned with `duplicate: true` and the original `event_id`. This is
load-bearing for replay determinism (CH-07): a replayed pipeline that
re-emits the same event sees the same receipt.

### Signed timestamps

Every emit carries `emitted_at = {at_ms, at, algorithm, key_id,
signature}`. The signature is HMAC-SHA-256 over the canonical material
`(at_ms, event_id, name_resolved, scope, scope_id, emitted_by)`, with a
per-process salt. The replay oracle uses this to detect timestamp
tampering during cross-run comparisons.

### Reading events back

`channel_events(name, options?)` returns the stored events oldest-first
for tests, diagnostics, and local orchestration inspection:

```harn,ignore
let events = channel_events("pr.merged", {limit: 10})
for event in events {
  println(event.payload.repo)
}
```

`from_cursor` (or `cursor`) resumes after a specific event id; `limit`
caps the returned size. This is **not** a subscription — for live
delivery, register a trigger (next section) or subscribe to the
EventLog topic directly with `event_log.subscribe`.

## Subscribe via triggers

A `channel.emit` trigger reacts to one or more channel names. Inside
the trigger, `match.events` lists the channel selectors:

```harn,ignore
import { trigger_register } from "std/triggers"

trigger_register({
  id: "release-on-pr-merge",
  kind: "channel.emit",
  provider: "channel",
  handler: { event ->
    println("PR merged: " + to_string(event.provider_payload.payload.number))
  },
  match: {events: ["channel:pr.merged"]},
})
```

Selector forms:

- `"channel:<name>"` — bare name; subscribes within the current scope
  (tenant by default).
- `"channel:session:<name>"`, `"channel:pipeline:<name>"`,
  `"channel:tenant:<tenant_id>:<name>"` — explicit scope match.

Trigger handlers can be:

- A closure `event -> any`.
- A handler-variant dict like `SpawnToPool({...})`, `ReminderInject({...})`,
  or `InterruptAndSuspend({...})`.
- An `a2a://` or `worker://` URI (forwards the event to a remote worker).

See `docs/src/triggers.md` for the full trigger surface; see
`docs/llm/harn-triggers-quickref.md` for the manifest equivalent.

### Filtering

A `when` predicate runs before dispatch:

```harn,ignore
trigger_register({
  id: "release-on-prod-merge",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:pr.merged"]},
  when: { event -> event.provider_payload.payload.target_branch == "main" },
  handler: { event -> kick_release(event) },
})
```

The trigger event's `provider_payload.payload` is the channel emit's
`payload` argument verbatim.

## Batching: aggregate N emits before firing

`batch { count, window, key, expire_action }` turns a trigger into a
fire-after-N-events aggregator. The trigger only dispatches once
`count` events have arrived (or the window expires, depending on
`expire_action`).

```harn,ignore
trigger_register({
  id: "release-on-3-merges",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:pr.merged"]},
  batch: {count: 3, window: "1h", key: "repo", expire_action: "fire_partial"},
  handler: { event ->
    let merged = event.batch
    println("Cutting release with " + to_string(len(merged)) + " merged PRs")
  },
})
```

Batch options:

| Option | Type | Default | Notes |
|---|---|---|---|
| `count` | positive int | required | Fire after this many events. |
| `window` | duration string (`"10m"`, `"1h"`) | required | Time budget; counter resets on every fire. |
| `key` | string dotted JSON path | none | Each distinct value at this path keeps an independent counter. Missing paths fall back to one shared bucket. |
| `expire_action` | `"fire_partial"` \| `"discard"` | `"fire_partial"` | When the window elapses with fewer than `count` events, fire with the partial buffer or drop it silently. |

On dispatch the handler receives a `TriggerEvent` whose `event.batch`
is the full list of constituent channel events; `event` itself is the
*last* event that closed the batch. Replay reconstructs the batch from
the recorded `constituent_event_ids` on the match receipt.

State is per-process and lives in a thread-local registry keyed by
`(binding, partition_key)`. The buffer is capped at 1024 events per
partition; overflow drops the oldest entries with a structured
`triggers.aggregation.buffer_overflow` warning.

Window expiration is driven by two mechanisms:

1. **Implicit sweep** — every channel emit first drains expired buffers
   before fanning the new event out, so wall-clock advance is enough
   when emits keep arriving.
2. **Explicit flush** — `flush_trigger_aggregations()` drains every
   expired buffer immediately. Tests pair this with `mock_time(ms)` /
   `advance_time(ms)` for deterministic coverage.

Diagnostic codes:

| Code | When |
|---|---|
| `HARN-CHN-005` | Malformed `batch` config (missing `count`/`window`, non-positive count, unknown `expire_action`, bad `key` type). |

## Periodic reminders: `ReminderInject`

`ReminderInject` (#1876) is a handler variant that injects a typed
[`system_reminder`](./system-reminders.md) into the target session's
transcript at the next turn boundary — no spawn, no resume, no signal.
Pair it with `batch` to land a single reminder per N emits, useful for
"reflect every 30 tool calls" or "after 3 build failures, ask the agent
to step back."

```harn,ignore
import { ReminderInject } from "std/triggers"

trigger_register({
  id: "reflect-after-30-tools",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:tool_call.completed"]},
  batch: {count: 30, window: "1h"},
  handler: ReminderInject({
    target: "current",
    body: "You've used 30 tools without a checkpoint. Take a turn to reflect on progress and adjust the plan.",
    tags: ["reflection_nudge"],
    ttl_turns: 1,
    dedupe_key: "reflection_nudge",
  }),
})
```

`ReminderInject` options:

| Option | Notes |
|---|---|
| `target` | `"current"` walks the owning session, `"parent"` walks its parent in the session lineage, any other string is a literal session id, and a closure `event -> string?` lets the trigger pick a target dynamically. |
| `body` | A `.harn.prompt` template rendered against `{{ event }}` (full trigger event), `{{ match }}` (`matched_at`), and `{{ batch }}` (when batching is in effect). |
| `tags`, `ttl_turns`, `dedupe_key`, `propagate`, `role_hint`, `preserve_on_compact` | Mirror `transcript.inject_reminder` semantics — see [system reminders](./system-reminders.md). |

Missing target sessions are dropped gracefully with a
`triggers.reminder_inject.audit` audit entry rather than failing
dispatch.

## Guardrails

Channel emits run through a pluggable guardrail middleware layer before
the durable journal append (CH-11). Each guardrail returns one of:

- `allow` — silent passthrough.
- `warn` — emit proceeds and a `channel_guardrail_warning` lifecycle
  audit is recorded.
- `block` — emit is dropped, the caller receives a synthetic
  `{blocked: true, block_reason, guardrail_fired}` receipt, and a
  `channel_guardrail_blocked` audit is persisted to both the
  `lifecycle.channel.audit` topic and the in-process lifecycle audit
  log.

Worst verdict wins; a `block` short-circuits remaining guardrails.

Register guardrails with `channel_guardrail_register(config)`:

```harn,ignore
import { prompt_injection_scanner, register_guardrail } from "std/channel_guardrails"

// Built-in: heuristic prompt-injection signature scan.
let pi_id = prompt_injection_scanner({})

// Custom: any closure returning nil / "allow" | "warn" | "block" / {verdict, reason?}.
let custom_id = register_guardrail({
  id: "block-secrets-in-payload",
  applies_to: ["channel:learnings.*"],
  evaluate: { payload, context ->
    if to_string(payload).contains("sk-") {
      return {verdict: "block", reason: "looks like an API key"}
    }
    return "allow"
  },
})
```

The registry is thread-local so peer pipelines on other threads cannot
poison each other; `reset_channel_state()` clears it.

## Observability

Channels produce three observable streams. Pick the one that matches the
consumer:

| Topic | Purpose | Schema |
|---|---|---|
| `lifecycle.channel.audit` | Durable replay/audit. Receipts for every emit + every match + every guardrail verdict. | `harn.channel_emit_receipt.v1`, `harn.channel_match_receipt.v1`, `harn.channel_guardrail_audit.v1` |
| `transcript.channel.lifecycle` | Live transcript rendering. One event per emit and per match, summarized for UI. | `transcript.channel.emit`, `transcript.channel.match` |
| Per-channel `channels.<scope>.<scope_id>.<name>` | The events themselves, for `event_log.subscribe(...)`. | The `StoredChannelEvent` envelope. |

OTel spans wrap every emit (`channel.emit <resolved_name>`) and every
match (`channel.match <resolved_name>`). Match spans link back to the
emit span via trace/span ids propagated on the trigger event headers,
which also carries through `batch` aggregation so a batched match links
to all constituent emits.

The replay oracle uses the `lifecycle.channel.audit` topic to detect:

- `HARN-REP-CHN-001` — replay matched an `event_id` that has no recorded receipt.
- `HARN-REP-CHN-002` — payload hash drift on a replayed emit.
- `HARN-REP-CHN-003` — batched match constituent ids differ across runs.

See `docs/src/observability/replay-benchmarks.md` for the oracle
interface.

## Diagnostic codes

| Code | When |
|---|---|
| `HARN-CHN-001` | `pipeline:` scope used outside any pipeline context. |
| `HARN-CHN-002` | Cross-tenant emit without a grant, or `org:` scope (disabled in v1). |
| `HARN-CHN-003` | Malformed channel name, scope prefix, or scope id. |
| `HARN-CHN-004` | Scope ambiguous — explicit `options.session_id` or `options.pipeline_id` conflicts with the active runtime context. |
| `HARN-CHN-005` | Malformed `batch` config on a `trigger_register` call. |

## Design rationale

Channels close two gaps that no major durable-execution platform owns
end to end:

| System | Pub/sub between agents? | Fire-after-N-events? | Periodic reminders? |
|---|---|---|---|
| Inngest | Yes (events) | **Yes (`step.waitForEvent` + count)** | No |
| Temporal | Signals (per-workflow) | No first-class primitive | No |
| Restate | Awakeables (per-invocation) | No | No |
| A2A | Push notifications | No | No |
| **Harn** | **Yes** | **Yes** | **Yes (`ReminderInject`)** |

The wager is that agent orchestration needs all three — typed pub/sub,
declarative batching, and turn-boundary reminder injection — and that
hosting them in one primitive (durable channels) keeps the trust
boundary (Harn owns the journal; hosts own UX) and the replay model
(`lifecycle.channel.audit` is the byte-comparable spec) intact.

Cross-references:

- [Channel cookbook](./cookbooks/channels.md) — five end-to-end recipes.
- [System reminders](./system-reminders.md) — the reminder primitive
  `ReminderInject` injects into.
- [Triggers](./triggers.md) — the general trigger surface that channel
  subscriptions plug into.
- Issue [#1870](https://github.com/burin-labs/harn/issues/1870) — the
  epic this primitive lands under.
