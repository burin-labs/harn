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
- `match` or `events`
- `dedupe_key`
- `filter`
- `budget`
- `manifest_path`
- `package_name`

The runtime currently accepts two handler forms:

- Local Harn closures / function references
- Remote URI strings with `a2a://...` or `worker://...`

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

Current shallow-path behavior:

- Local handlers execute immediately in-process.
- `when` predicates execute before the handler and can return a skipped result.
- `a2a://...` and `worker://...` handlers are not dispatched yet from this
  manual stdlib path; they currently surface a DLQ-style failure until the
  full dispatcher lands.

### `trigger_replay(event_id)`

Replay a previously recorded event from the EventLog by id and return a
`DispatchHandle`.

This is intentionally the shallow path for now:

- Fetch the prior event from the `triggers.events` topic
- Re-dispatch it through the current binding

`trigger_replay(...)` is not the full deterministic T-14 replay engine yet.
The implementation is marked `TODO(T-14)` accordingly.

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
