# Trigger Observability In The Action Graph

Harn now projects dispatcher-independent trigger activity into persisted run
observability. This lands the first half of issue #163: `trigger` and
`predicate` nodes, plus the matching `trigger_dispatch` and `predicate_gate`
edges.

## What lands in this change

- A synthetic `trigger` node is added when a run carries a `trigger_event`
  envelope in `run.metadata`.
- Workflow `condition` stages render as `predicate` nodes in
  `observability.action_graph_nodes`.
- Entry edges from the trigger node into the workflow render as
  `trigger_dispatch`.
- Transitions leaving a predicate render as `predicate_gate`.
- `trace_id` propagates from the `TriggerEvent` onto the synthetic trigger
  node and every downstream action-graph node derived from that run.

The runtime also streams the derived graph onto the shared event-log topic
`observability.action_graph` whenever a run record is persisted. This reuses
the generalized `EventLog` infrastructure instead of a parallel observability
bus.

## Current shape

This scoped change is intentionally limited to the dispatcher-independent
surface:

- Landed here: `trigger` and `predicate` node kinds.
- Deferred to T-06: `dispatch`, `a2a_hop`, `worker_enqueue`, and `dlq`.
- Deferred to T-06: portal replay controls and dispatcher-coupled UI work.
- Deferred to T-06: A2A `trace_id` header propagation.

## Example

When a workflow is started with a `trigger_event` option, the persisted run
record will include observability nodes like:

```json
{
  "kind": "trigger",
  "label": "cron:tick",
  "trace_id": "trace_123"
}
```

and:

```json
{
  "kind": "predicate",
  "label": "gate",
  "trace_id": "trace_123"
}
```

with edges such as:

```json
{"kind": "trigger_dispatch", "from_id": "trigger:...", "to_id": "stage:..."}
{"kind": "predicate_gate", "label": "true"}
```

The portal does not yet render specialized UI for these nodes in this PR; it
will consume the shared event-log topic in the dispatcher follow-up.
