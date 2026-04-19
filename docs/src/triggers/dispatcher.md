# Trigger Dispatcher

The trigger dispatcher is the runtime path that turns a normalized
`TriggerEvent` plus a live registry binding into actual handler work.

At MVP, the dispatcher fully wires the local-function path plus synchronous
and pending-handle `a2a://...` dispatch. The `worker://...` scheme remains an
explicit stub with a clear follow-up ticket.

## Dispatch shape

Each dispatch goes through the same sequence:

1. Append the inbound event to `trigger.inbox.envelopes`.
2. Match the event against active registry bindings for the provider +
   event kind.
3. Evaluate the optional `when` predicate in the same VM/runtime surface as
   the handler.
4. Invoke the resolved handler target.
5. Record each attempt on `trigger.attempts`.
6. Record successful handler results on `trigger.outbox`.
7. Schedule retries from the manifest retry policy.
8. Move exhausted deliveries into the in-memory DLQ and append a copy to
   `trigger.dlq`.
9. When the dispatch is a replay, emit a `replay_chain` action-graph edge
   linking the new trigger node back to the original event id.

The dispatcher keeps per-thread stats for:

- in-flight dispatch count
- retry queue depth
- DLQ depth

`harn doctor` surfaces that snapshot next to the trigger registry view.

## Handler URI resolution

Manifest handler URIs support three forms:

- bare/local function name: `handler = "on_issue"` or `handler = "handlers::on_issue"`
- remote A2A target: `handler = "a2a://reviewer.prod/triage"`
- worker queue target: `handler = "worker://triage-queue"`

By the time the dispatcher sees a manifest-installed binding, local function
handlers have already been resolved to concrete `VmClosure` values through the
same export-loading path used by manifest hooks and trigger predicates.

The dispatcher still re-normalizes those shapes internally so it can emit a
stable handler kind and target URI in lifecycle logs and action-graph nodes.

For `a2a://host[:port]/path` routes, the dispatcher:

- fetches `/.well-known/a2a-agent` from the target host
- requires exactly one JSON-RPC interface in the agent card before it will
  dispatch
- treats the URI path as the `target_agent` label that propagates into the
  outbound envelope and the action graph
- sends the `TriggerEvent` envelope over `a2a.SendMessage`
- returns either the inline agent result (when the peer completes
  synchronously) or a pending task handle payload for the caller

## Retry policy

Bindings carry a normalized `TriggerRetryConfig`:

- `Svix`
- `Linear { delay_ms }`
- `Exponential { base_ms, cap_ms }`

The default retry budget is 7 total attempts.

The Svix schedule is:

`immediate -> 5s -> 5m -> 30m -> 2h -> 5h -> 10h -> 10h`

The last slot saturates, so attempts beyond the published vector continue to
wait 10 hours unless a future manifest surface narrows that policy.

## Cancellation

Dispatcher shutdown is cooperative:

- a shutdown signal flips the active per-dispatch VM cancel tokens immediately
- sleeping retry waits listen for the shared shutdown broadcast and abort early
- local handlers observe cancellation through the existing VM
  `install_cancel_token(...)` path and exit on the next instruction boundary

This keeps the trigger runtime aligned with the orchestrator shutdown model
without inventing a second cancellation mechanism.

## Event-log topics

The dispatcher uses the shared `EventLog` instead of a parallel queue layer:

- `trigger.inbox.envelopes`
- `trigger.inbox.claims`
- `trigger.outbox`
- `trigger.attempts`
- `trigger.dlq`
- `triggers.lifecycle`
- `observability.action_graph`

`trigger.inbox.envelopes` is the dispatcher's durable ingress stream.
`trigger.inbox.claims` stores TTL-bound dedupe claims for `InboxIndex`.
Harn v0.7.23 also soft-reads the legacy mixed `trigger.inbox` topic on
startup so older event logs keep working while new writes go only to the
split topics.

`triggers.lifecycle` now includes dispatcher-specific lifecycle records:

- `DispatchStarted`
- `DispatchSucceeded`
- `DispatchFailed`
- `RetryScheduled`
- `DlqMoved`

## Action-graph updates

Dispatcher streaming now covers the local-handler path plus the first A2A hop:

- node kinds: `trigger`, `predicate`, `dispatch`, `a2a_hop`, `retry`, `dlq`
- edge kinds: `trigger_dispatch`, `a2a_dispatch`, `predicate_gate`, `retry`,
  `dlq_move`

Each update is appended to `observability.action_graph` using the shared
`RunActionGraphNodeRecord` / `RunActionGraphEdgeRecord` schema so the portal
and any other subscriber can consume dispatcher traces without special-casing a
separate payload format.

Replay dispatches add one more edge kind:

- `replay_chain`

The portal renders that edge as the visible link from the replayed trigger
event back to the original event id.

## Current MVP limits

- `a2a://...` currently uses the single-shot `a2a.SendMessage` path only; push
  callbacks, streaming chunk accumulation, and remote cancel/resubscribe stay
  deferred
- `worker://...` still returns `DispatchError::NotImplemented` and points at
  `O-05 #182`
- DLQ storage is in-memory plus event-log append; durable replay remains
  follow-up work
