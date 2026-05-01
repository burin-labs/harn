# Trigger Budgets

LLM-backed trigger predicates can run on every inbound event. A broad Slack
classifier that asks "does this mention cake?" in a busy channel can become a
runaway cost source, so Harn treats cost controls as trigger configuration
rather than an operator afterthought.

Budgets apply only to predicate LLM evaluation. Cheap manifest filters, dedupe,
flow control, and non-LLM handlers still run unless the configured exhaustion
strategy says otherwise.

## Trigger Budget

```toml
[[triggers]]
id = "slack-cake-classifier"
kind = "webhook"
provider = "slack"
match = { events = ["message"] }
when = "handlers::is_cake"
handler = "handlers::on_cake"

budget = {
  max_cost_usd = 0.001,
  max_tokens = 500,
  hourly_cost_usd = 1.00,
  daily_cost_usd = 5.00,
  max_autonomous_decisions_per_hour = 25,
  max_autonomous_decisions_per_day = 100,
  max_concurrent = 10,
  on_budget_exhausted = "false",
}
```

Supported fields:

- `max_cost_usd`: per-predicate LLM spend ceiling. This is also the initial
  expected cost used for preflight budget checks.
- `max_tokens`: per-predicate token ceiling.
- `hourly_cost_usd`: trigger-level UTC-hour spend ceiling.
- `daily_cost_usd`: trigger-level UTC-day spend ceiling.
- `max_autonomous_decisions_per_hour`: maximum `act_auto` handler dispatches
  in a UTC hour before the next matching event is routed to approval.
- `max_autonomous_decisions_per_day`: maximum `act_auto` handler dispatches in
  a UTC day before the next matching event is routed to approval.
- `max_concurrent`: deprecated alias for `concurrency = { max = ... }`.
- `on_budget_exhausted`: one of `false`, `retry_later`, `fail`, or `warn`.

`when_budget = { max_cost_usd = ..., tokens_max = ..., timeout = ... }` remains
supported for older manifests. If both `when_budget` and `budget` specify a
per-predicate ceiling, `when_budget` wins.

## Exhaustion Strategies

- `false`: default. The predicate evaluates to `false`, the event is skipped,
  and lifecycle/Prometheus budget metrics are emitted.
- `retry_later`: the event is recorded as budget-deferred with the next UTC
  reset boundary so an operator can recover it without spending more now.
- `fail`: the event moves directly to the trigger DLQ with a budget-exhausted
  error.
- `warn`: predicate budget exhaustion is logged, but dispatch proceeds. Use this
  only for advisory predicates where cost governance should not block work.

## Global Budget

Use `[orchestrator.budget]` to cap aggregate predicate spend across all triggers
in one orchestrator process.

```toml
[orchestrator.budget]
hourly_cost_usd = 10.00
daily_cost_usd = 25.00
```

When the global budget would be exceeded, Harn disables new LLM predicate
evaluations. Pure filters and other cheap trigger hygiene still run.

## Observability

`harn orchestrator inspect` includes global budget usage and each trigger's
configured budget usage. Prometheus output includes:

- `harn_trigger_budget_cost_today_usd{trigger_id}`
- `harn_trigger_budget_exhausted_total{trigger_id,strategy}`
- `harn_trigger_predicate_cost_usd{trigger_id}` histogram

Lifecycle records use `predicate.budget_exceeded` and include the trigger id,
event id, current spend, configured strategy, and whether the exhausted budget
was trigger-local or global.

Autonomy budget trips use `autonomy.budget_exceeded`. Instead of silently
skipping the handler, Harn appends a `request_approval` HITL record with the
default `operator` reviewer, adds an approval gate node to the action graph, and
records an `autonomy.tier_transition` trust-graph audit entry from `act_auto` to
`act_with_approval`.

## Per-agent Autonomy Budget

`agent_loop` accepts an `autonomy_budget` option that gates an autonomous
loop the same way the trigger-level cap gates a webhook handler. The check
runs at loop entry, before any LLM or MCP work fires — scripts can't bypass
it by skipping the option in a nested call.

```harn
let result = agent_loop(
  prompt,
  system,
  {
    provider: "anthropic",
    autonomy_budget: {per_hour: 10, per_day: 100, key: "captain.persona", reviewer: "oncall"},
  },
)
```

Supported fields:

- `per_hour`: maximum approved agent_loop dispatches in a UTC hour. `nil`
  disables the hourly cap. Must be `>= 1` if set.
- `per_day`: maximum approved agent_loop dispatches in a UTC day. `nil`
  disables the daily cap. Must be `>= 1` if set.
- `key`: stable identifier used to group decisions across agent_loop
  invocations. Defaults to the loop's `session_id`. Choose a stable key
  (typically the persona name or agent identity) when each call mints a
  fresh session — otherwise the budget effectively never accumulates.
- `reviewer`: reviewer name attached to the HITL approval request when
  the budget is exhausted. Defaults to `"operator"`.

When the budget is exhausted, `agent_loop` returns immediately with a
result of this shape (`reason` is `"hourly_autonomy_budget_exceeded"` or
`"daily_autonomy_budget_exceeded"`):

```text
{
  status: "approval_required",
  approval_required: true,
  reason: <one of the two reason strings>,
  request_id: "hitl_approval_...",
  reviewers: ["oncall"],
  from_tier: "act_auto",
  requested_tier: "act_with_approval",
  per_hour: ..., per_day: ...,
  autonomous_decisions_hour: ..., autonomous_decisions_today: ...,
  ...
}
```

The same side effects as the trigger-side path land:

- a HITL approval request on `hitl.approvals` (queryable via `hitl_pending`)
- a `triggers.lifecycle` event named `autonomy.budget_exceeded` carrying
  the agent key, session id, trace id, and reason
- a `trust_graph.records` entry tagged `autonomy.tier_transition` from
  `act_auto` to `act_with_approval`

Counters reset at each UTC hour and UTC day boundary. They live in
process-thread-local memory and are reset by `harn_vm::reset_thread_local_state`
between test runs.
