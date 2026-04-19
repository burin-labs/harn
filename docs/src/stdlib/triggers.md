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
- `handler`
- `when`
- `retry`
- `match` or `events`
- `dedupe_key`
- `filter`
- `budget`
- `manifest_path`
- `package_name`

The runtime currently accepts two handler forms:

- Local Harn closures / function references
- Remote URI strings with `a2a://...` or `worker://...`

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
  handler: handle_issue,
  when: nil,
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
- `a2a://...` and `worker://...` handlers still return the dispatcher’s
  explicit `NotImplemented` failure path.

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
- `trigger_fire(...)` and `trigger_replay(...)` need an active EventLog to
  persist `triggers.events` and `triggers.dlq`. If the runtime did not already
  install one, the stdlib wrapper falls back to an in-memory log for the
  current thread.
- When `workflow_execute(...)` runs inside a replayed trigger dispatch, the
  runtime carries the replay pointer into run metadata so derived
  observability can render a `replay_chain` edge back to the original event.
