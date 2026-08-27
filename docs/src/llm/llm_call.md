# LLM calls

## llm_call

Make a single LLM request. Harn normalizes provider responses into a
canonical dict so product code does not need to parse provider-native
message shapes.

```harn
const result = harness.llm.call("What is 2 + 2?")
harness.stdio.log(result.text)
```

With a system message:

```harn
const result = harness.llm.call(
  "Explain quicksort",
  "You are a computer science teacher. Be concise."
)
harness.stdio.log(result.text)
```

With options — build them through the typed `LlmCallOptions` alias from
`std/llm/options` (the documented path; option typos then surface at
`harn check` time instead of being silently ignored):

```harn
import { LlmCallOptions } from "std/llm/options"

const opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-5.4-mini",
  max_tokens: 1024,
}
const result = harness.llm.call(
  "Translate to French: Hello, world",
  "You are a translator.",
  opts,
)
harness.stdio.log(result.text)
```

With image or video content:

```harn
import { image_content, video_content } from "std/llm/media"
import { LlmCallOptions } from "std/llm/options"

const opts: LlmCallOptions = {
  provider: "minimax",
  model: "MiniMax-M3",
  messages: [{
    role: "user",
    content: [
      {type: "text", text: "Summarize these inputs."},
      image_content("diagram.png", {detail: "auto"}),
      video_content("demo.mp4"),
    ],
  }],
}
const result = harness.llm.call("", nil, opts)
harness.stdio.log(result.text)
```

Image blocks use the provider-neutral shape
`{type: "image", url?: string, base64?: string, media_type: string, detail?: "low"|"high"|"auto"}`.
Exactly one of `url` or `base64` is required. Harn translates it to
Anthropic `source`, OpenAI `image_url`, Gemini `inline_data`/`file_data`,
or Ollama `images` fields at the provider boundary. Ollama's REST API
only accepts base64 image data, so `url` image blocks are rejected for
`provider: "ollama"`. `std/llm/media` also provides `image_message(...)`
and `image_vision_context(...)` helpers when a harness wants the same image
as both LLM content and deterministic `vision_ocr(...)` context.

Video blocks use the provider-neutral shape
`{type: "video", url?: string, base64?: string, media_type: string}`. Exactly
one of `url` or `base64` is required. Harn translates video blocks to OpenAI
compatible `video_url` content, and to Gemini `inline_data`/`file_data` parts
for routes that declare video support. `std/llm/media` also provides
`video_message(...)`.

### Parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| prompt | string | yes | The user message |
| system | string | no | System message for the model |
| options | dict | no | Provider, model, and generation settings |

#### When `system` is given twice

The system message can arrive positionally (argument 2) or as the
`system` option. Both are accepted, and the outcome depends on which
form the option uses:

- **Option is a string.** The positional argument wins. The option is
  consulted only when the positional argument is absent, `nil`, or
  blank (empty or whitespace only) — then it becomes the system
  message.
- **Option is a fragment list.** Nothing competes. The list contributes
  `before` / `after` fragments that surround the primary block, and the
  positional argument supplies that primary block. Both appear.
- **Option is `{mode: "replace", content}`.** The replacement wins over
  everything, including a positional argument, and suppresses every
  other prompt contributor.

Passing `system` both ways is legal but easy to misread. Prefer one
form per call site.

### Return value

`harness.llm.call` returns one rigid, canonical envelope. Every field uses a
single snake_case spelling — there are no top-level aliases, and all
accounting lives under `usage`. The typed contract is
`LlmResponse` from `std/llm/envelope`; the same module exports
`LlmUsage`, `LlmOutcome`, `LlmOutcomeKind`, `LlmToolCall`, and
`LlmStreamChunk`.

| Field | Type | Description |
|---|---|---|
| `model` | string | The model that produced the response |
| `provider` | string | Canonical provider identifier |
| `usage` | `LlmUsage` | Single owner of all call accounting — tokens, cost, prompt-cache, and serving tier. See [Usage](#usage) below. |
| `outcome` | `LlmOutcome` | Typed classification of what the call produced. Branch on this, never on raw `stop_reason`. See [Outcome](#outcome) below. |
| `text` | string | The public answer, after tool/protocol projection |
| `raw_text` | string | Pre-projection parser source, with protocol tags intact |
| `visible_text` | string | Sanitized human-visible assistant output |
| `canonical_text` | string | Canonical replay form of a tagged-protocol response (present only for tagged responses) |
| `data` | any | Parsed and optionally schema-validated value when `output` requests JSON |
| `thinking` | string | Reasoning trace (when `thinking` is enabled) |
| `thinking_summary` | string | Provider-supplied summary of the reasoning trace, when available |
| `stop_reason` | string | Provider-native stop vocabulary (`"end_turn"`, `"max_tokens"`, `"tool_use"`, `"stop_sequence"`), kept for forensics — prefer `outcome` |
| `tool_calls` | `list<LlmToolCall>` | Dispatchable tool calls, merged from the provider-native and text-protocol channels. Always present, possibly empty. |
| `native_tool_calls` | `list<LlmToolCall>` | Provider-native tool calls only. Always present, possibly empty. |
| `protocol_violations` | `list<ProtocolViolation>` | Typed text-protocol violations: `{kind, message, excerpt?, dropped_reason?}`. Branch on `kind`; `message` is corrective display text. |
| `tool_parse_errors` | list | Errors from parsing malformed tool-call payloads |
| `done_marker` | string | The completion sentinel the model emitted, when one was parsed |
| `provider_response_id` | string | Provider-native response id when available, such as OpenAI Responses `resp_*` |
| `transcript` | dict | Transcript carrying message history, events, summary, metadata, and id |
| `blocks` | list | Canonical structured content blocks across providers. Always present, possibly empty. |
| `logprobs` | list | Token log probability records when requested and returned by the provider |
| `routing` | dict | Route-resolution metadata when the call went through the routing layer |

The four text channels each have a distinct job — none are aliases.
`text` is the public answer after projection, `raw_text` is the
pre-projection source with protocol tags intact, `visible_text` is the
sanitized human-visible output, and `canonical_text` is the canonical
replay form of a tagged-protocol response.

#### Usage

`usage` is the single owner of all call accounting; no accounting field
is duplicated at the envelope's top level. The VM response, provider-response
event, and LLM trace are mechanical projections of the same normalized Rust
ledger. Consumers should read the recorded fields rather than re-price tokens
or derive cache behavior independently.

| Field | Type | Description |
|---|---|---|
| `input_tokens` | int | Input/prompt token count |
| `output_tokens` | int | Output/completion token count |
| `reported_total_tokens` | int \| nil | Whole-call token count reported directly by the provider. This preserves total-only receipts without assigning tokens to an unknown input/output component. |
| `cost_usd` | float \| nil | Cache- and serving-tier-adjusted catalog price for this response; `nil` (not `0`) when pricing is unknown |
| `known_cost_usd` | float | Priced lower bound across all physical provider calls, retained when `cost_usd` is unknown because one call was unpriced |
| `provider_call_count` | int | Physical provider calls represented by this logical call, including retries |
| `unpriced_calls` | int | Physical calls without known catalog pricing |
| `usage_unknown_calls` | int | Physical calls that returned no authoritative token or cost accounting |
| `cache_read_tokens` | int | Prompt tokens served from provider-side cache |
| `cache_write_tokens` | int | Prompt tokens written into provider-side cache |
| `cache_supported` | bool | Compatibility flag that is `false` for explicitly unsupported routes; use `cache_visibility` for declaration certainty |
| `cache_hit_ratio` | float \| nil | Fraction of prompt tokens served from cache; `nil` when cache accounting is unsupported or undeclared |
| `cache_visibility` | string \| nil | `nil` when cache accounting is declared supported, `"unsupported"` when it is declared unavailable (e.g. native Ollama), and `"undeclared"` when the provider catalog makes no claim. Undeclared routes preserve parsed cache tokens, but a zero is not treated as a measured miss. |
| `accounting_status` | string | `"reported"` when token or authoritative cost telemetry was observed; `"unknown"` when a completed response omitted accounting telemetry. Unknown usage keeps `cost_usd` nil rather than treating the call as free. |
| `cache_savings_usd` | float | Estimated prompt-cache savings versus full input-token price; negative when cache writes cost more than normal input |
| `served_fast` | bool | `true` when the provider confirmed it served this request at the accelerated ("fast mode") tier; drives premium-tier billing |
| `provider_telemetry` | dict | Raw provider-reported usage/telemetry, passed through when present |
| `provider_attempts` | dict | How many provider requests this one logical call took, and why the extra ones happened (see below) |

For llama.cpp calls, `usage.provider_telemetry` also separates prompt size from
prompt work:

| Field | Meaning |
|---|---|
| `server_prompt_tokens` | Full prompt size from `usage.prompt_tokens` |
| `server_total_tokens` | Whole-call count from `usage.total_tokens` when the provider reports no component breakdown |
| `server_uncached_prompt_tokens` | Prompt tokens llama.cpp evaluated instead of reading from cache |
| `server_cached_prompt_tokens` | Prompt tokens llama.cpp read from cache |
| `server_prompt_eval_ms` | Time llama.cpp spent evaluating the uncached prompt |
| `server_generation_ms` | Time llama.cpp spent generating the answer |
| `server_total_ms` | Prompt evaluation time plus generation time |

Flat `provider_call_response` and LLM-trace records use the same canonical
names (`input_tokens`, `output_tokens`, `cache_read_tokens`, and
`cache_write_tokens`). The former trace-only `cache_tokens` mirror is not a
separate accounting field.

#### Provider attempts

`harness.llm.call` is one logical call, but the runtime may issue several provider
requests to complete it: a rate-limited request is retried, so is one that
returns nothing the loop can act on, so is a transport failure.
`usage.provider_attempts` reports that:

| Field | Meaning |
|---|---|
| `total` | Provider requests issued, including the one that succeeded |
| `retries` | `total - 1` |
| `rate_limited` | Requests rejected with a retryable rate-limit error |
| `empty_completion` | Requests that returned nothing the loop could act on |
| `other` | Retryable failures that were neither — transport errors, server faults, tool-format degrades |

Retries are broken out by reason because they mean different things: rate
limiting says the provider is saturated, an empty completion says the model
produced nothing usable, and a transport retry says the link is flaky.

This is distinct from any counter named for "calls". A run whose provider
rejected 47 of 146 requests with retryable 429s still made 96 logical calls,
and reporting only the latter hides the contention that explains why the run
was slow and stopped early.

#### Outcome

Every response carries `outcome: {kind, billed}`. `billed` is `true`
when the provider charged tokens for the call. Consumers should branch on
`outcome.kind` rather than re-deriving intent from the provider-native
`stop_reason`:

| `kind` | Meaning |
|---|---|
| `"complete"` | The model committed a normal answer and stopped cleanly. |
| `"tool_use"` | The actionable content is one or more tool calls. |
| `"truncated"` | Generation was cut on an output-token limit; text and especially tool-call arguments are suspect. |
| `"refused"` | The provider refused or filtered the completion. |
| `"paused"` | The provider paused the turn (e.g. Anthropic `pause_turn`); resume it rather than judging it. |
| `"empty"` | Nothing usable was committed: no visible text, no tool calls, no thinking. |

`kind == "empty"` together with `billed == true` is the
billed-noncommittal signal — the condition default retry policy
re-dispatches on. `std/llm/envelope` ships predicates for these branches
so callers do not re-implement them: `llm_response_is_empty`,
`llm_response_is_billed_empty`, and `llm_response_is_truncated`.

### Options dict

This section is reference material. For a step-by-step upgrade from older
spellings, see [Migrating to 0.10](../migrations/v0.10.md#llm-call-options).

`LlmCallOptions` in `std/llm/options` is the checked authoring surface. Annotate
a binding or use `llm_options({...})`:

```harn
import { LlmCallOptions, llm_options } from "std/llm/options"
import { system_before } from "std/llm/prompts"

const opts: LlmCallOptions = llm_options({
  provider: "openai",
  model: "gpt-5.4",
  system: [system_before("Return a compact result.")],
  output: {
    schema: {
      type: "object",
      properties: {answer: {type: "string"}},
      required: ["answer"],
    },
    strict: true,
    validation: "error",
  },
  effort: "high",
  timeout_ms: 120000,
})
```

The runtime applies the same registry to typed values, literals, computed
dicts, direct calls, streams, and agent-loop dispatch. An unknown key is an
error with a nearest-name suggestion. A removed spelling is an error with its
replacement. Keys beginning with `_` are reserved for internal host plumbing.

#### Routing

| Key | Type | Meaning |
|---|---|---|
| `model` | string | Model selector. |
| `model_role` | string | Fill missing route fields from `[model_roles.<name>]`. Explicit options win. |
| `model_tier` | string | Resolve a configured tier such as `small`, `mid`, or `frontier`. |
| `provider` | string | Provider id, or `auto` for model-based resolution. |
| `api_mode` | `chat_completions` \| `responses` | OpenAI API family. |
| `route_policy` | string \| dict | Catalog-backed route policy. |
| `fallback_chain` | string \| list | Ordered provider fallbacks. |
| `routing` | dict | Explicit routing policy object. |
| `equivalent_failover` | bool \| dict | Build a capability-equivalent failover chain. |
| `models` | list | Inline cheap-first `ModelLadder`; advances only on route failures. |
| `ladder` | string | Named `[model_ladders.<name>]` catalog ladder. |

`models`, `ladder`, explicit `model`/`provider`, and `routing` are competing
route owners. Do not combine them.

`equivalent_failover` does not add automatic alternatives for local providers
such as `ollama`, `mlx`, `vllm`, `llamacpp`, or `tgi` because the catalog does
not prove which models are installed. Use an explicit `routing` policy when
those local routes are known to be available.

Use `harness.llm.model_ladder(name)` to inspect the label and ordered steps of
a named ladder without starting a call. Keep reusable provider/model lists in
catalog source data and pass only the stable ladder name from policy code.

#### Conversation

| Key | Type | Meaning |
|---|---|---|
| `system` | string \| list \| dict | System text, ordered `SystemFragment` values, or an exclusive `{mode: "replace", content}` root. See [When `system` is given twice](#when-system-is-given-twice) for how this interacts with the positional `system` argument. |
| `messages` | list | Full canonical message history; supersedes the positional prompt. |
| `session_id` | string | Continue a session opened with `agent_session_open`. |
| `call_role` | string | Semantic call purpose retained in manifests and telemetry; aliases `mock_scope`, must agree with it, and is `unattributed` when undeclared. |
| `rate_limit_consumer_id` | string | Stable fairness identity for shared provider quotas; defaults to `session_id`. |
| `mock_scope` | string | Deterministic mock-fixture scope; real providers ignore it. |
| `context_profile` | dict | Context-selection profile. |
| `capabilities` | any | Explicit required capabilities. |
| `prefill` | string | Assistant prefill where the route supports it. |
| `previous_response_id` | string | OpenAI Responses conversation-state link. |

Each system fragment has `{content, title?, position?: "before"|"after",
enabled?}`. Use `system_before`, `system_after`, and `with_system_fragments`
from `std/llm/prompts` when composing fragments. Use
`{mode: "replace", content: string}` when `content` must be the entire system
channel; replacement suppresses every additive prompt contributor while
preserving tools and ordinary conversation history.

#### Generation

| Key | Type | Meaning |
|---|---|---|
| `max_tokens` | int | Maximum generated tokens. |
| `temperature` | float | Sampling temperature. |
| `top_p` | float | Nucleus-sampling cutoff. |
| `top_k` | int | Top-k sampling cutoff where supported. |
| `logprobs` | bool \| `{top?: int}` | Request token log probabilities and optionally `0..=20` alternatives per token. |
| `logit_bias` | `list<TokenBias>` | Bias exact, tokenizer-scoped token references by `-100..=100`. See [Exact token references](./tokenizer.md). |
| `min_p` | float | Minimum probability cutoff in `0..=1` where supported. |
| `repetition_penalty` | float | Positive repetition multiplier where supported. |
| `prediction` | `{content: string}` | Non-empty predicted output for providers that can accelerate a mostly-known response. |
| `verbosity` | `"low" \| "medium" \| "high"` | Provider-native response detail level. |
| `mirostat` | `{version: 1 \| 2, target_entropy?: float, learning_rate?: float}` | Ollama Mirostat sampling. Defaults are `5.0` and `0.1`; learning rate is within `(0, 1]`. |
| `stop` | string \| list | Stop sequence or sequences. |
| `stop_at_tool_call` | bool | End the call after the first tool call. |
| `seed` | int | Reproducibility seed where supported. |
| `frequency_penalty` | float | Frequency penalty where supported. |
| `presence_penalty` | float | Presence penalty where supported. |
| `parallel_tool_calls` | bool | Permit or forbid multiple native tool calls in one model turn. Requires at least one native tool. |

Harn admits caller-selected `temperature`, `top_p`, `top_k`, `seed`,
`frequency_penalty`, `presence_penalty`, `stop`, and the advanced controls
above against the resolved
provider/model route before transport. An explicit catalog denial throws a
terminal `invalid_request` error; the option is never silently removed from the
wire request. New advanced controls require an authored lowering, including on
custom routes. Catalog or provider defaults are not caller intent and do not
trigger admission.

Routing applies the same check to every attempted link after that link's model
and option overrides are resolved. Run `harn provider catalog matrix` to inspect
the declared route surface before choosing portable options.

The registry records model facts, not merely fields present in a provider's
schema. Current Gemini 3.5 and 3.6 routes reject `logprobs`, and Groq documents
its `logprobs` and `logit_bias` fields as unsupported by every deployed model.
Harn rejects those options before credential lookup. Cerebras accepts
`logprobs` and `prediction` individually but not together. That combination is
also rejected locally.

#### Output contract and recovery

| Key | Type | Meaning |
|---|---|---|
| `output` | `OutputSpec` | `"text"`, `"json"`, a schema value/type, or `{schema, strict?, validation?, stream_abort?}`. |
| `schema_retries` | int | Bounded corrective retries after schema failure. |
| `schema_retry_nudge` | bool \| string | Automatic, disabled, or caller-supplied corrective prompt. |
| `retries` | int | Wrapper-level bounded call retries. |
| `schema_recover` | bool | Attempt deterministic extraction before repair. |
| `repair` | bool \| dict | Enable or configure LLM-assisted schema repair. |

`output: "json"` parses JSON without a schema. A schema value validates and
narrows `result.data`; the config form controls strict provider transport,
post-parse validation, and early stream abort in one place:

```harn
type Verdict = {pass: bool, reason: string}

const result = harness.llm.call(prompt, nil, {
  output: {
    schema: Verdict,
    strict: true,
    validation: "error",
    stream_abort: true,
  },
  schema_retries: 1,
})
```

When schema validation remains invalid, the thrown error and
`harness.llm.call_safe()` error include a `schema_retry` record:

| Field | Type | Meaning |
|---|---|---|
| `attempts` | int | Model calls made, including the initial call. |
| `budget` | int | Corrective retries allowed after the initial call. |
| `status` | string | `disabled` when `budget` is zero; `exhausted` when every allowed retry ran. |

`harness.llm.call_structured_result()` includes the same record on schema
failure. These fields describe corrective schema retries. Provider and network
retry policy remains separate.

#### Reasoning and modalities

| Key | Type | Meaning |
|---|---|---|
| `thinking` | bool \| `adaptive` \| dict | Explicit provider reasoning mechanism. |
| `effort` | string | Provider-neutral reasoning intent. |
| `reasoning_policy` | bool \| string | Policy that resolves an effort from task and scale. |
| `reasoning_scale` | string | `small`, `medium`, or `large` policy hint. |
| `reasoning_task` | string | `chat`, `agent`, `code`, `verify`, or `summarize` policy hint. |
| `interleaved_thinking` | bool | Request Anthropic interleaved thinking where supported. |
| `anthropic_beta_features` | string \| list | Additional Anthropic beta feature names. |
| `vision` | bool | Require image-input support. |
| `audio` | bool | Require audio-input support. |
| `pdf` | bool | Require PDF-input support. |
| `video` | bool | Require video-input support. |

Use `effort` for intent. Use `thinking` only when a caller needs to choose the
provider mechanism or budget explicitly. Provider capabilities decide how
intent is lowered.

`thinking` is the readable reasoning projection. `blocks` retains the exact
provider continuation material when the provider requires it: Anthropic
`thinking` blocks include their `signature`, and `redacted_thinking` blocks
retain their opaque `data`. Treat those fields as private transcript data and
do not render or edit them.

Replay is capability-owned rather than a caller option. The resolved route's
`reasoning_round_trip` policy is `strip` by default, `echo_signed` for native
Anthropic routes, and `echo_same_key` for routes such as Moonshot Kimi K3 that
require reasoning under the same response field. This keeps private reasoning
off unrelated providers while preserving valid continuation on routes that
require it.

#### Tools

| Key | Type | Meaning |
|---|---|---|
| `tools` | list \| dict | Harn-executed tool definitions. |
| `provider_tools` | list \| dict | Provider-executed tools or remote MCP connectors. |
| `tool_choice` | string \| dict | Automatic, disabled, required, or named tool selection. |
| `tool_search` | bool \| string \| dict | Progressive tool disclosure. |
| `tool_format` | string | Tool-call wire format override. |
| `tool_format_override_reason` | string | Non-empty acknowledgment that deliberately forces `tool_format` past catalog channel gates. |

Harn executes, approves, and audits `tools`. The provider executes
`provider_tools`; Harn records their blocks but never dispatches them locally.
Normally Harn steers a tool-bearing call away from a channel the capability
catalog marks unsafe. A probe or matrix can force the requested channel with a
non-empty `tool_format_override_reason`; Harn then preserves the requested
format and, for `native`, sends the tool schemas even when the route does not
advertise native tools. Provider-call transcript records expose the effective
`tool_format` and `native_tool_count`, so the measured arm is visible at the
wire boundary. Blank reasons do not bypass the catalog gates.

#### Cache, budget, and transport

| Key | Type | Meaning |
|---|---|---|
| `cache` | bool \| dict | Provider prompt caching or wrapper cache policy. |
| `prompt_cache_ttl` | `5m` \| `1h` | Requested provider prompt-cache TTL. |
| `budget` | number \| `LlmBudget` | Maximum cost or token envelope. |
| `timeout_ms` | int | Whole-call timeout in milliseconds. |
| `idle_timeout_ms` | int | Streaming idle timeout in milliseconds. |
| `stream` | bool | Enable streaming transport. |
| `speed` | string | Serving intent: `standard` or `fast`. |

`speed: "fast"` is admitted only when the selected route advertises a usable
fast tier. Premium pricing applies only when provider telemetry confirms that
tier. Explicit `cache: true` and `prompt_cache_ttl` require authored capability
support because Harn must lower them to a provider-specific wire shape. A TTL
must also appear in the route's `prompt_cache_ttls` list. `cache: false` is an
opt-out, not capability intent. `cache` does not memoize full responses; use
`with_cache` from `std/llm/handlers` for Harn-owned response caching.

Streaming applies three independent limits on every SSE and NDJSON provider:
`timeout_ms` bounds the whole request, `HARN_LLM_FIRST_TOKEN_TIMEOUT` bounds
the wait for the first chunk, and `idle_timeout_ms` (or
`HARN_LLM_IDLE_TIMEOUT`) bounds later gaps. A provider must send its protocol's
terminal event (`[DONE]`, an OpenAI `finish_reason`, Anthropic `message_stop`,
or Ollama `done: true`); EOF without one is a transient failure.

Caught stream failures carry `source: "provider_stream"`, `phase`
(`awaiting_first_chunk` or `streaming`), `deadline` (`total`, `first_chunk`,
`idle`, or `nil`), and `partial`. The same fields appear on
`provider_call_error` transcript receipts.

Schema-stream abort receipts carry a `schema_failure` record with `kind`,
`detail`, `path`, and `chunks_consumed`. `kind` is one of `invalid_json`,
`invalid_schema`, `wrong_type`, `missing_required`, `unexpected_property`,
`max_length`, `min_length`, or `constraint_violation`. `detail` keeps the exact
validator message for diagnosis. The error category remains
`schema_stream_aborted`.

#### OpenAI Responses

These keys require `provider: "openai"` and `api_mode: "responses"`.

| Key | Type | Meaning |
|---|---|---|
| `store` | bool \| dict | Provider persistence, or a wrapper cache store config at the wrapper seam. |
| `background` | bool | Run the response in provider background mode. |
| `truncation` | string | Provider-side truncation or compaction policy. |
| `compact` | bool | Use the standalone `/responses/compact` endpoint. |
| `include` | list | Provider metadata expansions. |
| `max_tool_calls` | int | Provider-executed tool-call limit. |

#### Provider options and observability

| Key | Type | Meaning |
|---|---|---|
| `provider_options` | dict | Namespaced provider escape hatch: `{openai: {...}, ollama: {...}}`. |
| `metadata` | dict | Caller metadata for wrappers and telemetry. |
| `reminders` | any | Reminder injection config. |
| `structural_experiment` | any | Final prompt-structure transform. |

Provider-specific fields are legal only below `provider_options`. They are
never accepted as top-level provider names. Use this escape hatch for a native
control that is not part of Harn's portable option contract; Harn does not
reinterpret such fields as portable generation intent:

```harn
const result = harness.llm.call("hello", nil, {
  provider: "ollama",
  provider_options: {ollama: {num_ctx: 32768}},
})
```

Provider-native spellings of first-class generation controls are rejected in
this escape hatch. For example, `provider_options.ollama.repeat_penalty` must
become top-level `repetition_penalty`, and OpenAI `top_logprobs` must become
`logprobs: {top: ...}`. This keeps capability admission, cache identity,
routing, and replay on one typed path.

Model roles are ordinary defaults and compose with the same routing path:

```harn
const merged = harness.llm.call(prompt, nil, {
  model_role: "merge",
  output: schema,
})
```

Removed option names are not compatibility aliases. `harn check` and the
runtime report the canonical replacement, so stale computed dicts cannot be
silently projected away.

### OpenAI Responses mode

Set `api_mode: "responses"` with `provider: "openai"` when the call should
use OpenAI's native Responses API instead of the generic
`/chat/completions` adapter:

```harn
import { LlmCallOptions } from "std/llm/options"

const opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-5.4",
  api_mode: "responses",
  output: {schema: summary_schema, strict: true, validation: "error"},
  provider_tools: [
    {type: "web_search"},
    {
      type: "mcp",
      server_label: "docs",
      server_url: "https://mcp.example.com",
      require_approval: "always",
    },
  ],
  truncation: "auto",
  max_tool_calls: 4,
}
const result = harness.llm.call(
  "Search and summarize current docs.", nil, opts,
)
```

Use normal Harn `tools` when Harn should execute, approve, and audit a tool or
MCP server locally. Use `provider_tools` only when the provider should execute a
hosted tool or remote MCP connector. Provider-executed calls appear in
`result.blocks`, transcript `provider_payload.blocks`, and `provider_response_id`
metadata with `executor: "provider_native"` and the provider-native IDs.
Set `compact: true` for a standalone Responses compaction pass; Harn records the
opaque `compaction` items as private blocks so later turns can explicitly choose
whether to feed the compacted provider window back as input.

Structural experiments can be enabled directly on a call:

```harn
import { LlmCallOptions } from "std/llm/options"

const experiment_opts: LlmCallOptions = {
  provider: "mock",
  structural_experiment: "prompt_order_permutation(seed: 42)",
}
const result = harness.llm.call(
  "Instruction\n\nContext block", nil, experiment_opts,
)
```

For custom transforms, pass a closure (or a `std/experiments.custom(...)`
spec) that rewrites `{messages, system}` and returns either `nil`, a new
message list, or `{messages?, system?, metadata?}`.

## llm_call_structured

`harness.llm.call_structured(prompt, schema, options?)` is the ergonomic
helper for the "ask for JSON against this schema, retry on
validation failure, return just the parsed data" pattern. It wraps
`harness.llm.call` and pre-applies the schema-validated-JSON defaults so
callsites stop repeating the same four options.

```harn
import { LlmCallOptions } from "std/llm/options"

const schema = {
  type: "object",
  required: ["name", "age"],
  properties: {
    name: {type: "string"},
    age: {type: "integer"},
  },
}
const structured_opts: LlmCallOptions = {
  provider: "anthropic", system: "You are precise.",
}
const person = harness.llm.call_structured(
  "Extract the speaker's name and age from the transcript.",
  schema,
  structured_opts,
)
harness.stdio.log(person.name)
harness.stdio.log(person.age)
```

### Parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| prompt | string | yes | The user message |
| schema | dict or `Schema<T>` | yes | JSON Schema dict or a type alias in value position. When passed a `Schema<T>` the return narrows to `T`. |
| options | dict | no | Any option `harness.llm.call` accepts, plus `system` (lifted into the system-message slot) and `retries` (alias for `schema_retries`) |

### Return value

The validated `data` payload, typed as `T` when the schema is a
`Schema<T>`. Throws on exhausted schema retries or transport
failure — callers can assume the return matches the schema.

The `{output: {schema, strict: true, validation: "error"},
schema_retries: 3}` defaults are applied unless the caller
overrides them in `options`.

### Non-throwing variant

`harness.llm.call_structured_safe(prompt, schema, options?)` returns the
`{ok, data, error}` envelope (mirroring `harness.llm.call_safe` but with
the validated `.data` pre-unwrapped) instead of throwing:

```harn
const r = harness.llm.call_structured_safe(
  prompt, schema, {provider: "openai"},
)
if !r.ok {
  harness.stdio.log(
    "structured call failed:", r.error.category, r.error.message,
  )
  return nil
}
const person = r.data
```

`r.error.category` is one of the canonical `ErrorCategory` strings
(`"rate_limit"`, `"timeout"`, `"schema_validation"`, `"auth"`,
`"budget_exceeded"`, `"transient_network"`, `"generic"`, …) — match on the category
instead of string-sniffing the message.

### Diagnostic envelope variant

`harness.llm.call_structured_result(prompt, schema, options?)` returns the
full failure-mode envelope production agent pipelines need, so
callers can keep raw model text, attempt counts, and validation /
repair state without hand-rolling parse / repair chains. It never
throws on transport or schema failures — `ok: false` plus
`error_category` distinguishes the failure mode.

Set `operation_timeout_ms` to cap the entire structured operation, including
schema retries, native-to-prompt fallback, and repair. This is distinct from
`timeout_ms`, which bounds each provider request. On expiry the in-flight scope
is cancelled and the envelope returns `error_category: "timeout"` plus the
configured `operation_timeout_ms`.

```harn
import { LlmCallOptions } from "std/llm/options"

const result_opts: LlmCallOptions = {
  provider: "auto",
  schema_retries: 2,
  // Optional repair pass — runs only on malformed JSON or
  // schema-invalid output. Skipped on transport failures.
  repair: {
    enabled: true,
    model: "cheapest_over_quality(low)",
    max_tokens: 600,
  },
}
const r = harness.llm.call_structured_result(prompt, schema, result_opts)
if r.ok {
  const person = r.data
  // ...
} else {
  harness.stdio.log(
    "structured call failed:", r.error_category, "raw:", r.raw_text,
  )
}
```

Envelope fields:

| Field | Type | Description |
|---|---|---|
| `ok` | bool | `true` when the parsed payload validated against the schema. |
| `data` | `T \| nil` | Validated payload, or `nil` on failure. Narrows to `T` when `schema: Schema<T>`. |
| `raw_text` | string | Final attempt's raw model text. Preserved on failure for offline diagnostics or manual repair. |
| `error` | string | Human-readable error message (empty on success). |
| `error_category` | `string \| nil` | `nil` on success. On failure, one of `transport`-class categories (`rate_limit`, `timeout`, `auth`, `transient_network`, …) or `missing_json` / `schema_validation` / `repair_failed`. |
| `attempts` | int | Number of model calls made. `1` = no retries; `2+` = schema retries kicked in. `0` only when arg parsing failed before any call. |
| `repaired` | bool | `true` when a repair tier produced valid JSON. |
| `repair_tier` | `string \| nil` | `nil` when no repair ran. `"local"` for a mechanical fix with no extra provider call. `"llm"` for a successful reissue. |
| `extracted_json` | bool | `true` when JSON had to be lifted from prose / markdown fences. |
| `usage` | dict | Final-attempt token, cache-adjusted priced-cost, and prompt-cache accounting. Each structured retry is charged as its own provider call; this envelope is not an aggregate. Unknown `cost_usd` stays `nil`. |
| `model` | string | Model that produced the final attempt. |
| `provider` | string | Provider that produced the final attempt. |

Repair-pass semantics:

- After a schema or JSON miss, Harn first tries a local mechanical
  salvage: trailing commas, unquoted identifier keys, prose/fence
  extraction, and truncated closing braces. That tier spends no extra
  tokens. If the repaired text still fails the schema, it is rejected
  rather than coerced into a plausible shape.
- The `repair` block is recognized only by
  `harness.llm.call_structured_result`. Pass `repair: {enabled: true, ...}`
  to enable the LLM reissue; presence of the dict implies opt-in.
- The LLM tier runs at most once, with `schema_retries: 0`, only when
  local salvage did not produce valid JSON. It is skipped on transport
  failures because there is no raw text to salvage.
- Override keys (`model`, `provider`, `max_tokens`, `system`, …) are
  merged onto the main call's options for the LLM repair attempt.

### Recover already-produced text

`harness.llm.recover_schema(text, schema, opts?)` is the after-the-fact
owner for already-produced text. Use it when you already have model
text from `harness.llm.call` (or another producer) and want parse →
extract → regex → optional LLM repair without issuing a new structured
call first.

```harn,ignore
const raw = harness.llm.call("Summarize the ticket").text
const r = harness.llm.recover_schema(raw, schema)
if r.ok {
  harness.stdio.log(r.data)
} else {
  harness.stdio.log("recovery failed:", r.stage, r.error_category)
}
```

Set `{repair: false}` for a fully deterministic pass. The optional LLM
repair block accepts the same overrides as
`harness.llm.call_structured_result`. See the language spec for the
`{ok, data, raw_text, error, error_category, attempts, stage, repaired}`
envelope and the `parsed` / `extracted` / `regex` / `llm_repair`
stages.

### When to use which helper

- Product code that needs just the parsed payload: prefer
  `harness.llm.call_structured`. It removes the `output`, `schema_retries`,
  and `.data` boilerplate from every
  callsite.
- Code that also needs token counts, transcript, thinking traces, or
  to pass a pre-built transcript: call `harness.llm.call` directly and read
  `.text` / `.data` / `.usage.input_tokens` / etc. off the full result
  dict.
- Call sites that prefer explicit branching over `try` blocks:
  `harness.llm.call_structured_safe` (the non-throwing envelope).
- Production agent pipelines that need raw-text retention, attempt
  counts, and an optional repair pass on malformed JSON:
  `harness.llm.call_structured_result` — replaces the
  `harness.llm.call → response.data → try { json_parse(...) } → json_extract → repair →
  schema_check` chain that downstream callers would otherwise
  hand-roll.

## Composable callers

`agent_loop` accepts an `llm_caller:` option — a closure that owns
each turn's `harness.llm.call(...)`. Wrap it with middleware from
`std/llm/handlers` (retry / fallback / shadow / logging / budget /
cache / circuit breaker) to compose resilience without forking the
loop:

```harn,ignore
import {default_llm_caller} from "std/llm/caller"
import {with_retry} from "std/llm/handlers"

const caller = with_retry(default_llm_caller(), {max_attempts: 4})

const result = agent_loop(harness, task, system, {
  loop_until_done: true,
  llm_caller: caller,
})
```

`llm_caller(opts = nil)` is the blessed default stack — it is exactly
`with_retry(default_llm_caller(), opts?.retry ?? {})` with typed
reserved-status classification and billed-empty re-dispatch on by
default. Reach for it instead of re-composing retry by hand, and compose
`with_cache` / `with_budget` / `with_logging` around it when you need
more:

```harn,ignore
import {llm_caller} from "std/llm/caller"

const result = agent_loop(harness, task, system, {
  loop_until_done: true,
  llm_caller: llm_caller({retry: {max_attempts: 4}}),
})
```

Caller contract:

```harn,ignore
fn(call) -> {ok: true, value: <llm dict>}
          | {ok: false, status: <reserved>, error?: any, retryable?: bool}
//   call = {prompt, system, opts, turn: {iteration, session_id, attempt}}
```

`with_retry`'s `max_attempts: N` counts total attempts. Migrating
`llm_retries: K` (removed in 0.10): pass `max_attempts: K + 1` — the
removed option counted retries *after* the first attempt. See
[Migrating to 0.10](../migrations/v0.10.md).

See [Composable callers and middleware](../stdlib/llm-handlers.md)
for the full module catalog (`handlers`, `ensemble`, `refine`,
`budget`, `defaults`, `safe`, `prompts`, `catalog`).

## llm_completion

Use `harness.llm.completion` for text continuation and fill-in-the-middle generation.
It lives at the same abstraction level as `harness.llm.call`.

```harn
import { LlmCallOptions } from "std/llm/options"

const completion_opts: LlmCallOptions = {
  provider: "ollama",
  model_tier: "small",
}
const result = harness.llm.completion(
  "const total = ", ";", nil, completion_opts,
)
harness.stdio.log(result.text)
```

## Cost tracking

Harn provides builtins for estimating and controlling LLM costs:

```harn
// Estimate cost for a specific call
const cost = llm_cost("claude-sonnet-5", 1000, 500)
harness.stdio.log("Estimated cost: $${cost}")

// Check cumulative session costs
const session = harness.llm.session_cost()
harness.stdio.log("Total: $${session.total_cost}")
harness.stdio.log("Calls: ${session.call_count}")
harness.stdio.log("Input tokens: ${session.input_tokens}")
harness.stdio.log("Output tokens: ${session.output_tokens}")

// Set a budget (LLM calls throw if exceeded)
harness.llm.budget(1.00)
harness.stdio.log("Remaining: $${harness.llm.budget_remaining()}")
```

For per-call controls, pass a `budget` envelope on `harness.llm.call` (the typed
shape is `LlmBudget` from `std/llm/options`):

```harn
import { LlmBudget, LlmCallOptions } from "std/llm/options"

const budget: LlmBudget = {
  max_cost_usd: 0.001,
  max_input_tokens: 8000,
  max_output_tokens: 1024,
}
const budgeted_opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-5.4-mini",
  max_tokens: 1024,
  budget: budget,
}
const result = try {
  harness.llm.call("Summarize this", nil, budgeted_opts)
}
```

Harn estimates prompt tokens before the provider request leaves the process,
using `tiktoken-rs` for known OpenAI models, labeled tiktoken approximations
for Claude/Gemini families, and a heuristic fallback for unknown model IDs.
It then projects cost with the provider/model pricing table and throws a
terminal `budget_exceeded` dict when a limit would be exceeded. In a
`try { ... }` expression that surfaces as `Result.Err({kind: "terminal",
reason: "budget_exceeded", projected_cost_usd: ...})`.

`agent_loop` accepts the same envelope. `max_*` limits apply to each model turn;
`total_budget_usd` is an aggregate loop budget and exits gracefully with
`status: "budget_exhausted"` before starting a turn that would exceed it.

| Function | Description |
|---|---|
| `llm_cost(model, input_tokens, output_tokens)` | Estimate USD cost from embedded pricing table |
| `harness.llm.session_cost()` | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `harness.llm.budget(max_cost)` | Set session budget in USD. LLM calls throw if exceeded |
| `harness.llm.budget_remaining()` | Remaining budget (nil if no budget set) |
| `tiktoken_count_tokens(text, model)` | Count text with the selected tiktoken encoder for known OpenAI/Claude/Gemini model families |

Import `std/llm/budget` for reusable helpers such as
`estimate_text_tokens_detail(text, model)`, which includes the encoder label
(`cl100k_base`, `o200k_base`, etc.) and whether the count is exact or an
approximation.

## Testing with mock LLM responses

The `mock` provider returns deterministic responses without API keys.
Use `harness.llm.mock_enqueue()` to queue specific responses — text, tool calls, or both:

```harn
// Queue a text response (consumed in FIFO order)
harness.llm.mock_enqueue({text: "The capital of France is Paris."})
const r = harness.llm.call(
  "What is the capital of France?", nil, {provider: "mock"},
)
assert_eq(r.text, "The capital of France is Paris.")

// Queue a response with tool calls
harness.llm.mock_enqueue({
  text: "Let me read that file.",
  tool_calls: [{name: "read_file", arguments: {path: "src/main.rs"}}],
})

// Queue token logprobs for confidence/reranking tests
harness.llm.mock_enqueue({
  text: "certain", logprobs: [{token: "certain", logprob: 0.0}],
})

// Pattern-matched mocks (reusable by
// default, matched in declaration order)
harness.llm.mock_enqueue({text: "I don't know.", match: "*unknown*"})
harness.llm.mock_enqueue({
  text: "step 1", match: "*planner*", consume_match: true,
})
harness.llm.mock_enqueue({
  text: "step 2", match: "*planner*", consume_match: true,
})

// Provider-style error envelopes exercise the same catch/safe-call paths
// as live provider failures.
harness.llm.mock_enqueue({
  error: {status: 503, kind: "transient", reason: "upstream_unavailable"},
})

// Inspect what was sent to the mock provider
const calls = harness.llm.mock_calls()
// Each entry includes mock_scope,
// messages, system, tools, output/thinking
// controls, and portable generation options
// such as temperature and max_tokens.

// Clear all mocks and call log between tests
harness.llm.mock_clear()
```

For concurrent agent work, prefer `with_llm_script(harness.llm, responses, fn)`
from `std/testing`. It scopes fixture installation and cleanup to the supplied
`HarnessLlm`; helpers that only receive another capability cannot inspect or
mutate the queue. Use `harness.llm.mock_snapshot()` and
`harness.llm.mock_calls()` for queue and call evidence. The JSONL fixture
parser is a runtime-internal implementation seam, not a script interface.

When no `harness.llm.mock_enqueue()` responses are queued, the mock provider falls back to
its default deterministic behavior (echoing prompt metadata). This means
existing tests using `provider: "mock"` without `harness.llm.mock_enqueue()` continue to
work unchanged.
