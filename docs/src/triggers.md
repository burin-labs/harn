# Triggers

Triggers connect external events to typed Harn handlers. A trigger binding
matches inbound deliveries from a provider, optionally gates them through a
typed predicate, and then dispatches the surviving event to a local function,
an A2A target, or a worker queue.

Use the trigger surface that matches how you want to manage the binding:

- `[[triggers]]` in `harn.toml` for declarative, manifest-loaded bindings
- `trigger_register(...)` from `std/triggers` for dynamic runtime bindings
- `trigger_fire(...)` / `trigger_replay(...)` when tests or operators need to
  inject or replay events manually

Manifest triggers support one source per binding or a parent handler with
multiple `[[triggers.sources]]` entries. `kind = "stream"` covers cataloged
continuous sources such as Kafka, NATS JetStream, Pulsar, Postgres CDC, email,
and WebSocket ingest, including tumbling, sliding, and session window metadata.

## LLM predicates

Trigger predicates let a binding decide whether an event should dispatch before
the handler runs:

```toml
[[triggers]]
id = "slack-outage-triage"
kind = "webhook"
provider = "slack"
match = { events = ["slack.message"] }
when = "handlers::about_outages"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::triage_outage"
budget = { daily_cost_usd = 1.00, max_concurrent = 10 }
```

The predicate must resolve to `fn(event: TriggerEvent) -> bool` or
`fn(event: TriggerEvent) -> Result<bool, _>`.

Typical pattern:

```harn
pub fn about_outages(event: TriggerEvent) -> bool {
  let result = llm_call(
    "Is this Slack message about a production outage?",
    nil,
    {provider: "openai", model: "gpt-4o-mini", llm_retries: 0},
  )
  return contains(result.text.lower(), "yes")
}
```

## Cost governance and replay

Predicate evaluation is safety-defaulted:

- `when_budget.max_cost_usd`, `tokens_max`, and `timeout` cap a single
  predicate evaluation
- `budget.daily_cost_usd` applies to aggregate predicate spend for the trigger
  across the current UTC day
- if either budget is exceeded, the predicate short-circuits to `false`
- replay caches predicate `llm_call(...)` responses so `trigger_replay(...)`
  can deterministically re-evaluate the predicate without hitting a live
  provider

Every predicate evaluation emits `predicate.evaluated`, and budget violations
emit `predicate.budget_exceeded` or `predicate.daily_budget_exceeded` on the
trigger lifecycle stream.

## Failure handling

Predicates fail closed:

- manifest parse/type errors in `when` prevent the trigger from loading
- runtime predicate failures short-circuit dispatch to `false`
- three consecutive predicate failures for the same trigger open a five-minute
  circuit breaker
- while the breaker is open, new events skip the predicate and handler and emit
  an operator-visible warning
