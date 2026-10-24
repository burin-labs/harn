# Call-cost settlement reference

`std/llm/economics.settle_call_cost` estimates one call from supplied usage.
Rust hosts use `harn_vm::llm::usage::settle_call_cost` with `CallCostInput`.
Both use the runtime's provider-receipt settlement owner. Neither sends a
provider request, records a call, nor spends a budget.

```harn
import { settle_call_cost } from "std/llm/economics"

fn main(harness: Harness) {
  const estimate = settle_call_cost({
    provider: "anthropic",
    model: "claude-sonnet-4-20250514", // harn-doc-model-ref: allow-stale
    started_at_ms: 1792756800000,
    input_tokens: 1000,
    cache_write_tokens: 1000,
    prompt_cache_ttl: "1h",
    billing: {hosted_tool_calls: {web_search: 1}},
  })
  harness.stdio.println(json_stringify(estimate))
}
```

The model and instant above select a historical catalog card; this example
does not call that model. It estimates $0.006 for the one-hour cache write and
$0.01 for the hosted search, for a total of $0.016.

## Input

`provider`, `model`, and `started_at_ms` are required. The instant is Unix
milliseconds and selects the dated promotion and recurring price window.

| Field | Meaning |
| --- | --- |
| `input_tokens` | Full normalized input, including cache reads and writes. Defaults to zero. |
| `output_tokens` | Full output token count. Defaults to zero. |
| `cache_read_tokens` | Cached input already included in `input_tokens`. Defaults to zero. |
| `cache_write_tokens` | Cache writes already included in `input_tokens`. Defaults to zero. |
| `prompt_cache_ttl` | `"5m"` or `"1h"`. Omit for the route's standard cache-write rate. |
| `served_fast` | Select accelerated-serving rates when true. Defaults to false. |
| `billing.hosted_tool_calls` | Map of catalog tool names to call counts. |
| `billing.audio_input_tokens` | Audio tokens already included in full input. |
| `billing.audio_output_tokens` | Audio tokens already included in full output. |
| `billing.cached_audio_input_tokens` | Audio tokens included in both audio input and cached input. |

All counts must be nonnegative. Cache reads plus writes must fit within full
input. Audio counts must fit within their enclosing counts. Invalid counts,
unknown input fields, and out-of-range instants fail before pricing.

The input describes the scenario to price. Omitted billing counters add no
units to that scenario; an estimate does not prove that a real response
reported every billable unit. For observed spend, retain the runtime's usage
ledger instead of replacing it with a new estimate.

## Result

| Field | Meaning |
| --- | --- |
| `cost_usd` | Complete estimate for the supplied units, or nil when a required price is missing. |
| `known_cost_usd` | The runtime's priced subtotal, retained when the complete amount is nil. |
| `pricing` | Selected card, request instant, input band, serving tier, and pricing limitations. Nil for an unknown route or a free route that needs no catalog card. |
| `unpriced` | Named gaps: `pricing_unknown`, `cache_read_rate_unknown`, `cache_ttl_unpriced`, `hosted_tool` with `name`, or `modality` with `name`. |
| `rates` | The selected token rates and their source, including the one-hour cache-write rate. |
| `breakdown` | Input, output, cache-read, cache-write charges, and `adjustments_usd` for hosted-tool and audio charges. |

Promotions and schedules apply at the request instant. Input bands apply to
the whole request. Cache writes use the requested lifetime. Hosted tools add
their per-call fees; audio rates replace those tokens' text charges.
Self-hosted inference stays free. Hosted-tool units still need their own
prices, and a real fee card is retained even when its quoted fee is zero.
Token rates and breakdown charges stay zero for that free inference.

When a one-hour cache rate is absent, the subtotal retains the runtime's
standard-lifetime estimate and `cache_ttl_unpriced` prevents a complete amount.
An absent cached-read rate also prevents a complete estimate and names
`cache_read_rate_unknown`; the subtotal retains the runtime's full-input-rate
fallback. These fallback amounts are estimates, not measured provider charges.
`pricing.monthly_allowance_unapplied` names hosted tools with a monthly free
allowance. These estimates use their listed fees because a single call has no
account-wide allowance census. A catalog platform fee is also an estimate,
named by `pricing.platform_fee_basis`.

`estimate_call_cost` uses this same owner and multiplies each charge by its
`calls` option. It accepts the same billing, lifetime, tier, and request-instant
fields. An omitted instant uses the active pricing clock. Its existing four
breakdown buckets remain; `adjustments_usd` adds hosted-tool and audio charges.
`pricing_facts` retains the selected card and limitations.

`estimate_session_cost` returns nil for a session or model's complete total
when any contributing call lacks a price. `known_cost_usd` retains the priced
subtotal, and `unknown_calls` names how many calls prevented a complete total.
`volume_cost` also retains a known subtotal when its complete total is nil.
