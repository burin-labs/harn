# Trigger Observability In The Action Graph

Harn now projects trigger activity into persisted run observability across both
workflow-derived nodes and dispatcher hops. The current surface includes
`trigger`, `predicate`, `dispatch`, and `a2a_hop` nodes, plus the matching
`trigger_dispatch`, `predicate_gate`, and `a2a_dispatch` edges.

## What lands in this change

- A synthetic `trigger` node is added when a run carries a `trigger_event`
  envelope in `run.metadata`.
- Workflow `condition` stages render as `predicate` nodes in
  `observability.action_graph_nodes`.
- Local dispatch attempts render as `dispatch` nodes.
- Remote A2A dispatch attempts render as `a2a_hop` nodes labelled with the
  resolved `target_agent`.
- Entry edges from the trigger node into the workflow render as
  `trigger_dispatch`.
- Trigger or predicate edges into a remote A2A hop render as `a2a_dispatch`.
- Transitions leaving a predicate on the workflow path render as
  `predicate_gate`.
- `trace_id` propagates from the `TriggerEvent` onto the synthetic trigger
  node and every downstream action-graph node derived from that run,
  including A2A hops.

The runtime also streams the derived graph onto the shared event-log topic
`observability.action_graph` whenever a run record is persisted. This reuses
the generalized `EventLog` infrastructure instead of a parallel observability
bus.

## Current shape

This scoped change still leaves some portal/runtime work deferred:

- Landed here: `trigger`, `predicate`, `dispatch`, and `a2a_hop` node kinds.
- Deferred: `worker_enqueue` specialized nodes and richer DLQ/A2A UI.
- Deferred: portal replay controls and dispatcher-coupled UI work.

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
{"kind": "a2a_dispatch", "from_id": "predicate:...", "to_id": "a2a:..."}
```

The portal does not yet render specialized UI for these nodes in this PR; it
will consume the shared event-log topic in the dispatcher follow-up.
