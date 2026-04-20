# Trigger stdlib

The trigger stdlib exposes the live runtime registry to Harn scripts. Use it to
inspect installed bindings, register new bindings at runtime, fire synthetic
events for tests/manual invocations, replay a recorded event by id, and inspect
the current dead-letter queue (DLQ).

Import the shared types from `std/triggers` when you want typed handles and
payloads:

```harn
import "std/triggers"
```

## Builtins

### `trigger_list()`

Return the current live registry snapshot as `list<TriggerBinding>`.

Each binding includes:

- `id`
- `version`
- `source` (`"manifest"` or `"dynamic"`)
- `kind`
- `provider`
- `autonomy_tier`
- `handler_kind`
- `state`
- `metrics`

`metrics` is a typed `TriggerMetrics` record with counters for `received`,
`dispatched`, `failed`, `dlq`, `in_flight`, and the cost snapshot fields.

### `trigger_register(config)`

Register a trigger dynamically and return its `TriggerHandle`.

`TriggerConfig` uses the same broad shape as manifest-loaded bindings:

- `id`
- `kind`
- `provider`
- `autonomy_tier`
- `handler`
- `when`
- `when_budget`
- `retry`
- `match` or `events`
- `dedupe_key`
- `filter`
- `allow_cleartext`
- `budget`
- `manifest_path`
- `package_name`

Dynamic `trigger_register(...)` currently supports the legacy budget fields but
does not yet accept manifest-only flow-control tables such as `concurrency`,
`throttle`, `rate_limit`, `debounce`, `singleton`, `batch`, or keyed
`priority`.

The runtime currently accepts two handler forms:

- Local Harn closures / function references
- Remote URI strings with `a2a://...` or `worker://...`

`allow_cleartext` is optional and only applies to `a2a://...` handlers. Set it
to `true` when you intentionally want HTTP A2A discovery / dispatch, for
example when talking to a local `harn serve` process during development.

`retry` is optional. The current stdlib surface accepts:

- `{max: N, backoff: "svix"}`
- `{max: N, backoff: "immediate"}`

Example:

```harn
import "std/triggers"

fn handle_issue(event: TriggerEvent) -> dict {
  return {kind: event.kind, provider: event.provider}
}

let handle: TriggerHandle = trigger_register({
  id: "github-new-issue",
  kind: "issue.opened",
  provider: "github",
  autonomy_tier: "act_with_approval",
  handler: handle_issue,
  allow_cleartext: nil,
  when: nil,
  when_budget: nil,
  match: {events: ["issue.opened"]},
  events: nil,
  dedupe_key: nil,
  filter: nil,
  budget: nil,
  manifest_path: nil,
  package_name: nil,
})
```

### `trigger_fire(handle, event)`

Fire a synthetic `TriggerEvent` into a binding and return a
`DispatchHandle`.

The builtin accepts either:

- A `TriggerHandle` / `TriggerBinding` dict
- A plain trigger id string

If the `event` dict omits low-level envelope fields such as `id`,
`received_at`, `trace_id`, or `provider_payload`, the runtime fills them with
synthetic defaults.

Current behavior:

- Execution routes through the trigger dispatcher, so local handlers inherit
  dispatcher retries, lifecycle events, action-graph updates, and DLQ moves.
- `when` predicates execute before the handler and can still short-circuit a
  dispatch.
- `when_budget` accepts `{max_cost_usd, tokens_max, timeout}` and applies
  fail-closed per-predicate LLM cost governance.
- When a manifest-installed binding uses `batch = { ... }`, the selected leader
  event carries the coalesced member list in `event.batch`.
- `a2a://...` handlers return either the inline remote result or a pending task
  handle, depending on the peer response.
- `worker://...` handlers return an enqueue receipt in `DispatchHandle.result`
  with `{queue, job_event_id, response_topic}`.

### `trigger_replay(event_id)`

Replay a previously recorded event from the EventLog by id and return a
`DispatchHandle`.

Current replay behavior:

- Fetch the prior event from the `triggers.events` topic
- Re-dispatch it through the trigger dispatcher using the recorded binding
- Preserve `replay_of_event_id` on the returned `DispatchHandle`
- Resolve the pending stdlib DLQ entry when a replay succeeds

`trigger_replay(...)` is still not the full deterministic T-14 replay engine.
It replays the recorded trigger event through the current dispatcher/runtime
state rather than a sandboxed drift-detecting environment.

### `trigger_inspect_dlq()`

Return the current DLQ snapshot as `list<DlqEntry>`.

Each `DlqEntry` includes:

- The failed `event`
- Trigger identity (`binding_id`, `binding_version`)
- Current `state`
- Latest `error`
- `retry_history`

`retry_history` records every DLQ attempt, including replay attempts.

### `trigger_inspect_lifecycle(kind?)`

Return the trigger lifecycle stream as a list of `{kind, headers, payload}`
records. Pass a kind such as `predicate.evaluated`,
`predicate.budget_exceeded`, or `DispatchStarted` to filter on the runtime
side.

### `handler_context()`

Return the current dispatch context as `HandlerContext | nil`.

Inside a trigger handler, the returned record includes:

- `agent`
- `action`
- `trace_id`
- `replay_of_event_id`
- `autonomy_tier`
- `trigger_event`

Outside trigger dispatch, the builtin returns `nil`.

### `trust_record(agent, action, approver, outcome, tier)`

Append a manual `TrustRecord` to the trust graph. Scripts usually rely on the
dispatcher's automatic end-of-handler records, but this builtin is available for
control-plane events such as promotions, demotions, or manual audit entries.

### `trust_query(filters)`

Query historical trust records from Harn code.

Supported filter keys:

- `agent`
- `action`
- `since`
- `until`
- `tier`
- `outcome`

## Example

```harn
import "std/triggers"

fn fail_handler(event: TriggerEvent) -> any {
  throw("manual failure: " + event.kind)
}

let handle = trigger_register({
  id: "manual-dlq",
  kind: "issue.opened",
  provider: "github",
  handler: fail_handler,
  when: nil,
  when_budget: nil,
  retry: {max: 1, backoff: "immediate"},
  match: nil,
  events: ["issue.opened"],
  dedupe_key: nil,
  filter: nil,
  budget: nil,
  manifest_path: nil,
  package_name: nil,
})

let fired = trigger_fire(handle, {provider: "github", kind: "issue.opened"})
let dlq = trigger_inspect_dlq().filter({ entry -> entry.binding_id == handle.id })
let replay = trigger_replay(fired.event_id)

println(fired.status)                  // "dlq"
println(len(dlq[0].retry_history))     // 1
println(replay.replay_of_event_id)     // original event id
```

## Notes

- Dynamic registrations are runtime-local. `trigger_register(...)` updates the
  live registry in the current process; it does not rewrite `harn.toml`.
- `a2a://...` bindings default to HTTPS-only. Use `allow_cleartext: true` only
  for intentional local or otherwise trusted HTTP peers.
- `TriggerConfig.autonomy_tier` defaults to `act_auto` when omitted.
- `trigger_fire(...)` and `trigger_replay(...)` need an active EventLog to
  persist `triggers.events` and `triggers.dlq`. If the runtime did not already
  install one, the stdlib wrapper falls back to an in-memory log for the
  current thread.
- Predicate replay is deterministic for `llm_call(...)`: cached predicate
  responses are reused from the request cache plus the per-event
  `trigger.inbox` record rather than calling the live provider again.
- Every terminal dispatch appends one `TrustRecord` to `trust.graph` plus the
  per-agent topic `trust.graph.<agent_id>`.
- When `workflow_execute(...)` runs inside a replayed trigger dispatch, the
  runtime carries the replay pointer into run metadata so derived
  observability can render a `replay_chain` edge back to the original event.
