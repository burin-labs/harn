# Trigger Dispatcher

The trigger dispatcher is the runtime path that turns a normalized
`TriggerEvent` plus a live registry binding into actual handler work.

At MVP, the dispatcher fully wires the local-function path plus synchronous
and pending-handle `a2a://...` dispatch. `worker://...` dispatch now enqueues
durable jobs on the shared EventLog so a separate orchestrator or handler-only
consumer can drain them later.

## Dispatch shape

Each dispatch goes through the same sequence:

1. Append the inbound event to `trigger.inbox.envelopes`.
2. Match the event against active registry bindings for the provider +
   event kind.
3. Evaluate the optional `when` predicate in the same VM/runtime surface as
   the handler.
4. Apply any manifest flow-control gates (`batch`, `debounce`, `rate_limit`,
   `throttle`, `singleton`, `concurrency`, `priority`).
5. Invoke the resolved handler target.
6. Record each attempt on `trigger.attempts`.
7. Record successful handler results on `trigger.outbox`.
8. Schedule retries from the manifest retry policy.
9. Move exhausted deliveries into the in-memory DLQ and append a copy to
   `trigger.dlq`.
10. When the dispatch is a replay, emit a `replay_chain` action-graph edge
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

- fetches `/.well-known/agent-card.json` from the target host, with legacy
  discovery fallbacks for older Harn A2A servers
- defaults to HTTPS-only discovery + dispatch; cleartext HTTP is rejected unless
  the trigger binding explicitly sets `allow_cleartext = true`
- selects the first JSON-RPC entry in `supportedInterfaces` before it will
  dispatch
- treats the URI path as the `target_agent` label that propagates into the
  outbound envelope and the action graph
- sends the `TriggerEvent` envelope over `a2a.SendMessage`
- returns either the inline agent result (when the peer completes
  synchronously) or a pending task handle payload for the caller

For local-dev receivers started with `harn serve`, add `allow_cleartext = true`
on the trigger binding. `harn serve` is HTTP-only today, so the dispatcher will
otherwise stop after the HTTPS probe instead of silently downgrading.

For `worker://<queue>` routes, the dispatcher:

- appends a durable job record under `worker.<queue>`
- records the originating trigger id, binding key, binding version, event id,
  and effective priority on the queued job
- returns an enqueue receipt with the queue name, job event id, and
  `worker.<queue>.responses` topic
- leaves execution to a later `harn orchestrator queue drain <queue>` consumer
  running against the same EventLog backend

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
- `worker.queues`
- `worker.<queue>`
- `worker.<queue>.claims`
- `worker.<queue>.responses`
- dynamic flow-control gate topics under
  `trigger.{debounce,rate_limit,throttle,singleton,concurrency,batch}.*`

`trigger.inbox.envelopes` is the dispatcher's durable ingress stream.
`trigger.inbox.claims` stores TTL-bound dedupe claims for `InboxIndex`.
Harn v0.7.23 also soft-reads the legacy mixed `trigger.inbox` topic on
startup so older event logs keep working while new writes go only to the
split topics.

The long-running orchestrator inbox pump admits a bounded number of
outstanding dispatch tasks. A full pump stops reading new inbox envelopes
and leaves their source cursor unacked until capacity is available. Pump
state is visible through `orchestrator.lifecycle` events such as
`pump_received`, `pump_eligible`, `pump_admitted`, `pump_dispatch_started`,
`pump_dispatch_completed`, and `pump_acked`, plus Prometheus metrics for
backlog, outstanding count, and admission delay.

Flow-control topics record per-gate admission decisions and waits. For example,
`concurrency = { key = "event.headers.tenant", max = 2 }` writes records under
`trigger.concurrency.<binding-and-gate>`, while `debounce` and `batch` emit the
selected/merged decision for the keyed gate they evaluated.

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

When `batch = { ... }` coalesces multiple envelopes, the handler still receives
one root `TriggerEvent`, and the remaining serialized members are attached on
`event.batch`.

Replay dispatches add one more edge kind:

- `replay_chain`

The portal renders that edge as the visible link from the replayed trigger
event back to the original event id.

## Current MVP limits

- `a2a://...` currently uses the single-shot `a2a.SendMessage` path only; push
  callbacks, streaming chunk accumulation, and remote cancel/resubscribe stay
  deferred
- worker consumers use polling claim/ack/TTL semantics today through
  `harn orchestrator queue drain`; there is not yet a long-running dedicated
  worker daemon mode
- DLQ storage is in-memory plus event-log append; durable replay remains
  follow-up work
