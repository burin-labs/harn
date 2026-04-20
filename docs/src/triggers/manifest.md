# Trigger manifests

`[[triggers]]` extends `harn.toml` with declarative trigger registrations in the
same manifest-overlay family as `[exports]`, `[llm]`, and `[[hooks]]`.

Each entry declares:

- a stable trigger `id`
- a trigger `kind` such as `webhook`, `cron`, or `a2a-push`
- a `provider` from the registered trigger provider catalog
- an `autonomy_tier` that defines the default execution mode
- a delivery `handler`
- optional dedupe, retry, budget, secret, and predicate settings

## Shape

```toml
[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
autonomy_tier = "act_with_approval"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.001, tokens_max = 500, timeout = "5s" }
handler = "handlers::on_new_issue"
dedupe_key = "event.dedupe_key"
retry = { max = 7, backoff = "svix", retention_days = 7 }
priority = "normal"
budget = { daily_cost_usd = 5.00, max_concurrent = 10 }
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
- `budget.daily_cost_usd` must be `>= 0`
- cron triggers must declare a parseable `schedule`
- cron `timezone` must be a valid IANA timezone name
- secret references must use `<namespace>/<name>` syntax and the namespace must
  match the trigger provider

Errors include the manifest path plus the `[[triggers]]` table index so the bad
entry is easy to locate.

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
