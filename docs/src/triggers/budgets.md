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
