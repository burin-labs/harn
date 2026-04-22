# Trigger manifests

`[[triggers]]` extends `harn.toml` with declarative trigger registrations in the
same manifest-overlay family as `[exports]`, `[llm]`, and `[[hooks]]`.

Each entry declares:

- a stable trigger `id`
- a trigger `kind` such as `webhook`, `cron`, or `a2a-push`
- a `provider` from the registered trigger provider catalog
- an `autonomy_tier` (or `tier`) that defines the default execution mode
- a delivery `handler`
- optional dedupe, retry, budget, flow-control, secret, and predicate settings

A single handler can also declare `sources` instead of top-level `kind` /
`provider`. Each source expands into its own concrete trigger binding with an id
of `<trigger-id>.<source-id>`, while sharing the parent handler and predicate.

## Shape

```toml
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
tier = "act_with_approval"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "normal"
budget = { max_cost_usd = 0.001, max_tokens = 500, hourly_cost_usd = 1.00, daily_cost_usd = 5.00, max_autonomous_decisions_per_hour = 25, max_autonomous_decisions_per_day = 100, on_budget_exhausted = "false" }
concurrency = { max = 10 }
secrets = { signing_secret = "github/webhook-secret" }
filter = "event.kind"
```

Supported autonomy tiers:

- `shadow`
- `suggest`
- `act_with_approval`
- `act_auto`

The manifest tier is the default. At dispatch time, Harn resolves the effective
tier from the manifest plus the latest matching trust-graph control record for
that agent.

## Handler URI schemes

Harn currently accepts three handler forms:

- local function:
  `handler = "on_event"` or `handler = "handlers::on_event"`
- A2A dispatch:
  `handler = "a2a://reviewer.prod/triage"`
- worker queue dispatch:
  `handler = "worker://triage-queue"`

Unsupported URI schemes fail fast at load time.

`a2a://...` handlers accept one extra opt-in field:

- `allow_cleartext = true` permits HTTP A2A card discovery / JSON-RPC dispatch
  for that binding

Leave it unset for normal remote targets. It exists for bounded local-dev cases
such as dispatching into `harn serve`, which currently listens on HTTP only.

`worker://...` handlers reuse the top-level scalar dispatch priority:

- `priority = "high"`
- `priority = "normal"`
- `priority = "low"`

That scalar priority becomes the default queue priority when the dispatcher
enqueues the job. An explicit event header `priority` still overrides it at
dispatch time.

Local handlers and predicates resolve through the same module-export plumbing as
the manifest hook loader:

- bare names resolve against `lib.harn` next to the manifest
- `module::function` resolves either through the current manifest's `[exports]`
  table or through package imports under `.harn/packages`

## Validation

The manifest loader rejects invalid trigger declarations before execution:

- trigger ids must be unique across the loaded root manifest plus installed package manifests
- `provider` must exist in the registered trigger provider catalog
- `handler` must be a supported URI, and local handlers must resolve to exported functions
- `allow_cleartext`, when present, must be a boolean and is only valid for
  `a2a://...` handlers
- `when` must resolve to a function with signature `fn(TriggerEvent) -> bool`
  or `fn(TriggerEvent) -> Result<bool, _>`
- `when_budget` requires `when`, and its `max_cost_usd`, `tokens_max`, and
  `timeout` fields must all be valid when present
- `dedupe_key` and `filter` must parse as JMESPath expressions
- `retry.max` must be `<= 100`
- `retry.retention_days` defaults to `7` and must be `>= 1`
- `budget.max_cost_usd`, `budget.hourly_cost_usd`, and
  `budget.daily_cost_usd` must be `>= 0`
- `budget.max_autonomous_decisions_per_hour` and
  `budget.max_autonomous_decisions_per_day` must be `>= 1` when present
- `budget.max_tokens` and `budget.max_concurrent` must be `>= 1` when present
- cron triggers must declare a parseable `schedule`
- cron `timezone` must be a valid IANA timezone name
- secret references must use `<namespace>/<name>` syntax and the namespace must
  match the trigger provider

Errors include the manifest path plus the `[[triggers]]` table index so the bad
entry is easy to locate.

## Multi-source handlers

Use `sources` when one handler should receive events from several trigger
transports:

```toml
[[triggers]]
id = "market-fan-in"
handler = "handlers::on_market_event"
when = "handlers::should_handle"
debounce = { key = "event.provider + \":\" + event.kind", period = "2s" }

[[triggers.sources]]
id = "open"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 14 * * 1-5"
timezone = "America/New_York"

[[triggers.sources]]
id = "quotes"
kind = "stream"
provider = "kafka"
match = { events = ["quote.tick"] }
topic = "quotes"
consumer_group = "harn-market"
window = { mode = "sliding", key = "event.provider_payload.key", size = "5m", every = "1m" }
```

The loader registers `market-fan-in.open` and `market-fan-in.quotes`. Source
tables inherit parent `when`, `when_budget`, flow-control, retry, dedupe,
filter, and secrets unless the source overrides the same field.

For compact manifests, `sources = [{ ... }, { ... }]` inline arrays are accepted
with the same source fields.

## Stream triggers

`kind = "stream"` registers continuous event sources. The built-in provider
catalog currently recognizes these STREAM-01 providers:

- `kafka`
- `nats`
- `pulsar`
- `postgres-cdc`
- `email`
- `websocket`

Stream providers are cataloged with a shared `StreamEventPayload` typed payload.
Concrete broker/email/WebSocket connector loops are represented as placeholder
connectors until a deployment supplies a Harn connector override or a future
native connector lands.

Windowing is declared with `window = { ... }`:

- tumbling: `window = { mode = "tumbling", size = "1m" }`
- sliding: `window = { mode = "sliding", size = "5m", every = "1m" }`
- session: `window = { mode = "session", gap = "30s" }`

All window modes accept optional `key` and `max_items`. Durations use the same
compact suffixes as flow control: `s`, `m`, `h`, `d`, `w`. Stream triggers can
also use regular `debounce`, `concurrency`, `throttle`, `rate_limit`,
`singleton`, and keyed `priority` controls.

## LLM-gated predicates

`when` runs before handler dispatch, so it is the right place to express typed
LLM classification gates such as:

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

Behavior:

- the predicate may call `llm_call(...)`
- per-evaluation overruns emit `predicate.budget_exceeded` and short-circuit to
  `false`
- `budget.daily_cost_usd` applies to aggregate predicate spend for the trigger
  over the current UTC day; once exceeded, the trigger keeps returning `false`
  until the next UTC midnight
- replay reuses cached predicate `llm_call(...)` responses from the provider
  request cache plus the event-scoped `trigger.inbox` record
- three consecutive predicate failures open a five-minute circuit breaker that
  fails closed with operator-visible warnings

## Flow control

Trigger manifests can shape dispatch admission with top-level flow-control
tables:

```toml
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"

concurrency = { key = "event.headers.tenant", max = 10 }
throttle = { key = "event.headers.user", period = "1m", max = 30 }
rate_limit = { period = "1h", max = 1000 }
debounce = { key = "event.headers.pr_id", period = "30s" }
singleton = { key = "event.headers.repo" }
priority = { key = "event.headers.tier", order = ["gold", "silver", "bronze"] }
```

Supported tables:

- `concurrency = { max = N }` or `concurrency = { key = "<expr>", max = N }`
- `throttle = { period = "<duration>", max = N }` or
  `throttle = { key = "<expr>", period = "<duration>", max = N }`
- `rate_limit = { period = "<duration>", max = N }` or
  `rate_limit = { key = "<expr>", period = "<duration>", max = N }`
- `debounce = { key = "<expr>", period = "<duration>" }`
- `singleton = {}` or `singleton = { key = "<expr>" }`
- `batch = { size = N, timeout = "<duration>" }` or
  `batch = { key = "<expr>", size = N, timeout = "<duration>" }`
- `priority = { key = "<expr>", order = ["...", "..."] }`

Durations use compact suffixes: `s`, `m`, `h`, `d`, `w`.

`key` expressions compile into Harn closures over the typed `TriggerEvent`
surface. They use the same event shape as `when` predicates and local handlers,
so expressions like `event.headers.tenant`, `event.kind`, or
`event.provider_payload.raw.repo.full_name` all resolve through the normal
stdlib trigger types.

When a keyed field omits `key`, Harn uses a single global gate for that binding.
For example, `rate_limit = { period = "1h", max = 1000 }` applies one shared
hourly budget across all matching events for that trigger.

`priority` is overloaded:

- `priority = "low" | "normal" | "high"` keeps the existing dispatch-priority
  field
- `priority = { key = "...", order = [...] }` enables concurrency-waiter
  ordering for flow control

`batch` delivers the selected leader event to the handler and attaches the full
coalesced group under `event.batch`.

Legacy `budget.max_concurrent` still loads, but Harn treats it as deprecated and
normalizes it to `concurrency = { max = N }` with a warning.

Current validation rules:

- `concurrency.max`, `throttle.max`, `rate_limit.max`, and `batch.size` must be
  positive
- `priority.order` must be non-empty
- `priority = { ... }` requires `concurrency = { ... }`
- `batch` cannot be combined with `debounce`, `singleton`, `concurrency`,
  keyed priority ordering, `throttle`, `rate_limit`, or legacy
  `budget.max_concurrent`

## Durable dedupe retention

Trigger dedupe now uses a durable inbox index backed by the shared EventLog
topic `trigger.inbox.claims`. Each successful claim stores the binding id plus the
resolved `dedupe_key`, and duplicate deliveries are rejected until the claim's
TTL expires.

- configure the TTL with `retry.retention_days`
- the default is `7` days
- shorter retention trims durable dedupe history sooner, which lowers storage
  cost but increases the chance that a late provider retry will be treated as a
  fresh event

Use a retention window at least as long as the provider's maximum retry window.
If a provider can redeliver for longer than your configured TTL, Harn may
dispatch that late retry again once the durable claim has expired.

Harn v0.7.23 still soft-reads legacy claim records from the old mixed
`trigger.inbox` topic on startup, but all new claim writes land under
`trigger.inbox.claims`.

## Doctor output

`harn doctor` now lists loaded triggers with:

- trigger id
- trigger kind
- provider
- handler kind (`local`, `a2a`, or `worker`)
- budget summary

## Examples

See the example manifests under [`examples/triggers`](../../../examples/triggers):

- [`cron-daily-digest`](../../../examples/triggers/cron-daily-digest/harn.toml)
- [`github-new-issue`](../../../examples/triggers/github-new-issue/harn.toml)
- [`a2a-reviewer-fanout`](../../../examples/triggers/a2a-reviewer-fanout/harn.toml)
- [`stream-fan-in`](../../../examples/triggers/stream-fan-in/harn.toml)
