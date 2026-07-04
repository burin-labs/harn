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

Use `harn routes <root> --json` before deploying a trigger project to audit the
static ingress surface. The command reports each declarative trigger's route
path, handler module, declared budgets, statically required capabilities,
vendor-lock disclosure, and prompt/template overhead without executing handler
code.

## Streaming trigger admission

Continuous sources should enter Harn through the connector-facing stream
runtime rather than calling handlers directly. The runtime accepts normalized
`TriggerEvent` values, applies the configured admission policy, emits
deterministic window records, then hands the resulting window event to the
regular dispatcher. That keeps policy, replay metadata, predicate gates,
retries, action-graph updates, and DLQ moves on the same path as webhook and
cron triggers.

The initial runtime primitive supports:

- fixed, tumbling, and sliding count windows
- queue limits with explicit `drop_newest`, `drop_oldest`, or
  `dead_letter_newest` overflow behavior
- optional dedupe-key debounce and period-based throttle admission
- deterministic gate-decision records keyed by a stable cache key and window
  dedupe keys, so replay can reuse the prior LLM decision without a fresh model
  call
- window status records for pending count, lag/drop/dead-letter counters, and
  gate pass/block counters

Window events keep the first source event's provider and tenant context, use a
`.window` event kind suffix, and attach serialized member events to
`event.batch`. The runtime also adds `harn_stream_id`,
`harn_stream_window_id`, and `harn_stream_source_event_ids` headers.

Stream observability is written to these EventLog topics:

- `trigger.stream.status` for admission, debounce, throttle, overflow, and gate
  status records
- `trigger.stream.windows` for emitted window envelopes
- `trigger.stream.gates` for cached and newly evaluated gate decisions
- `trigger.dlq` with `stream_dead_lettered` records when backpressure moves an
  event to the dead-letter path

Connector implementations should call `StreamConnector::push_inbound(...)` or
`push_inbound_with_gate(...)` after activation. Those helpers normalize the
provider-native `RawInbound` through the connector schema first, then admit the
event through the stream runtime; they do not bypass policy or replay logging.

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
    {provider: "openai", model: "gpt-4o-mini"},
  )
  return contains(result.text.lower(), "yes")
}
```

## Cost governance and replay

Predicate evaluation is safety-defaulted:

- `when_budget.max_cost_usd`, `tokens_max`, and `timeout` cap a single
  predicate evaluation
- `budget.daily_cost_usd` applies to aggregate predicate spend for the trigger
  and to handler results that report `cost_usd`, `total_cost_usd`, or eval-pack
  `stats_rows[*].total_cost_usd`, across the current UTC day
- if a predicate budget is exceeded, the predicate short-circuits to `false`;
  if aggregate trigger spend is already exhausted before a handler starts, the
  dispatcher applies `budget.on_budget_exhausted`
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
