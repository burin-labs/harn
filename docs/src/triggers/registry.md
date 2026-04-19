# Trigger registry

The trigger registry is the runtime-owned binding table that turns
validated `[[triggers]]` manifest entries into live, versioned trigger
bindings inside a VM thread.

## Ownership model

- The registry is thread-local, following the same pattern as the
  runtime hook table. Each VM thread owns its own bindings and does not
  share `Rc<VmClosure>` values across threads.
- Cross-thread coordination is pushed down to the event-log layer. The
  trigger registry only tracks the bindings that the current VM can
  execute.
- Manifest parsing and validation still live in `harn-cli`. Once
  handlers and predicates resolve, the CLI passes a compact binding spec
  into `harn-vm`, which owns lifecycle and metrics.

## Binding shape

Each live binding stores:

- logical trigger id
- monotonically increasing version
- provider and trigger kind
- resolved handler target (`local`, `a2a`, or `worker`)
- optional resolved `when` predicate
- lifecycle state: `registering`, `active`, `draining`, `terminated`
- metrics snapshot: `received`, `dispatched`, `failed`, `dlq`,
  `in_flight`, and last-received timestamp
- manifest provenance for diagnostics

Hot reload keeps the logical id stable and bumps the binding version
whenever the manifest definition fingerprint changes.

## Lifecycle

Manifest install performs a reconcile step against the current
thread-local registry:

1. New trigger id: register version `1`, emit `registering`, then
   `active`.
2. Existing trigger id with unchanged definition: keep the current
   active binding.
3. Existing trigger id with changed definition: mark the old binding
   `draining`, register a new active version, and keep both bindings
   visible until the old version reaches `in_flight == 0`.
4. Removed manifest trigger: mark the live binding `draining`. Once
   `in_flight == 0`, it transitions to `terminated`.

Dynamic registrations follow the same state machine, but they are not
reconciled by manifest reload.

## Metrics and draining

- `begin_in_flight(id, version)` increments `received` and `in_flight`
  and updates `last_received_ms`.
- `finish_in_flight(id, version, outcome)` decrements `in_flight` and
  increments one of `dispatched`, `failed`, or `dlq`.
- A draining binding becomes terminated only after the in-flight count
  returns to zero.

This keeps hot reload safe: events that started under version `N`
complete under version `N`, while new events route to version `N+1`.

## Event-log integration

When an active event log is installed for the VM thread, every lifecycle
transition appends a record to the `triggers.lifecycle` topic. The event
payload includes:

- logical trigger id
- `id@vN` binding key
- provider
- trigger kind
- handler kind
- transition `from_state` and `to_state`

`harn doctor` uses the installed registry snapshot to report the live
bindings it sees after manifest load, including state, version, and
zeroed metrics for newly installed triggers.

The trigger stdlib’s manual replay path also depends on the registry:

- `trigger_fire(...)` records the synthetic event on `triggers.events`
- `trigger_replay(...)` looks up that recorded envelope plus any pending
  stdlib DLQ summary entry on `triggers.dlq`
- the wrapper then re-enters the dispatcher against the resolved live binding
  version and threads `replay_of_event_id` through dispatch observability

## Test Harness

`harn_vm::triggers::test_util` now provides the shared trigger-system
test harness used by both Rust unit tests and `.harn` conformance
fixtures. The harness owns:

- a reusable mock clock with wall-clock and monotonic hooks
- a recording connector sink/registry for emitted normalized events
- named fixture runners that cover cron, webhook verification,
  retry/backoff, DLQ/replay, dedupe, rate limiting, cost guards, crash
  recovery, hot reload, and dead-man alerts

The script-facing entrypoint is the `trigger_test_harness(...)` builtin,
which returns a structured report for the selected fixture instead of
requiring each conformance script to rebuild connector state by hand.
