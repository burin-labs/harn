# LLM calls

## llm_call

Make a single LLM request. Harn normalizes provider responses into a
canonical dict so product code does not need to parse provider-native
message shapes.

```harn
let result = llm_call("What is 2 + 2?")
log(result.text)
```

With a system message:

```harn
let result = llm_call(
  "Explain quicksort",
  "You are a computer science teacher. Be concise."
)
log(result.text)
```

With options — build them through the typed `LlmCallOptions` alias from
`std/llm/options` (the documented path; option typos then surface at
`harn check` time instead of being silently ignored):

```harn
import { LlmCallOptions } from "std/llm/options"

let opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-4o",
  max_tokens: 1024,
}
let result = llm_call(
  "Translate to French: Hello, world",
  "You are a translator.",
  opts,
)
log(result.text)
```

With image or video content:

```harn
import { image_content, video_content } from "std/llm/media"
import { LlmCallOptions } from "std/llm/options"

let opts: LlmCallOptions = {
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
let result = llm_call("", nil, opts)
log(result.text)
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

### Return value

`llm_call` always returns a dict:

| Field | Type | Description |
|---|---|---|
| `text` | string | The text content of the response |
| `visible_text` | string | Human-visible assistant output |
| `model` | string | The model used |
| `provider` | string | Canonical provider identifier |
| `input_tokens` | int | Input/prompt token count |
| `output_tokens` | int | Output/completion token count |
| `cache_read_tokens` | int | Prompt tokens served from provider-side cache when supported |
| `cache_write_tokens` | int | Prompt tokens written into provider-side cache when supported |
| `cache_creation_input_tokens` | int | Anthropic-compatible alias for `cache_write_tokens` |
| `cache_hit_ratio` | float | Fraction of prompt tokens served from provider-side cache |
| `cache_savings_usd` | float | Estimated prompt-cache savings versus full input-token price; cache writes can be negative when writes cost more than normal input |
| `served_fast` | bool | `true` when the provider confirmed it served this request at the accelerated ("fast mode") tier; drives premium-tier billing |
| `usage` | dict | Token and prompt-cache accounting fields, including the cache fields above and `served_fast` |
| `data` | any | Parsed JSON (when `response_format: "json"`) |
| `tool_calls` | list | Tool calls (when model uses tools) |
| `thinking` | string | Reasoning trace (when `thinking` is enabled) |
| `private_reasoning` | string | Provider reasoning metadata kept separate from visible text |
| `blocks` | list | Canonical structured content blocks across providers |
| `logprobs` | list | Token log probability records when requested and returned by the provider |
| `stop_reason` | string | `"end_turn"`, `"max_tokens"`, `"tool_use"`, `"stop_sequence"` |
| `provider_response_id` | string | Provider-native response id when available, such as OpenAI Responses `resp_*` |
| `transcript` | dict | Transcript carrying message history, events, summary, metadata, and id |

### Options dict

Build options through the typed structural alias `LlmCallOptions` (from
`std/llm/options`) — either annotate a binding
(`let opts: LlmCallOptions = {...}`) or pass a literal through the
`llm_options({...})` constructor so unknown keys are rejected at check
time. A raw dict still executes unchanged; the typed path is the
documented one, and the `unnormalized-options` lint nudges agent-loop
call sites toward it.

| Key | Type | Default | Description |
|---|---|---|---|
| `provider` | string | `"anthropic"` | Any configured provider. Built-in names include `"anthropic"`, `"openai"`, `"openrouter"`, `"huggingface"`, `"ollama"`, `"gemini"`, and `"local"` |
| `model` | string | varies by provider | Model identifier |
| `model_role` | string | nil | Fill missing call options from `[model_roles.<role>]` before normal provider/model/routing resolution. Explicit call options win. The `merge`/`fast_apply` roles also read `HARN_LLM_MERGE_*` and `HARN_LLM_FAST_APPLY_*` provider/model/route-policy overrides. |
| `max_tokens` | int | `16384` | Maximum tokens in the response |
| `temperature` | float | provider default | Sampling temperature (0.0-2.0) |
| `top_p` | float | nil | Nucleus sampling |
| `top_k` | int | nil | Top-K sampling (Anthropic/Ollama only) |
| `stop` | list | nil | Stop sequences |
| `seed` | int | nil | Reproducibility seed (OpenAI/Ollama) |
| `frequency_penalty` | float | nil | Frequency penalty (OpenAI only) |
| `presence_penalty` | float | nil | Presence penalty (OpenAI only) |
| `logprobs` | bool | `false` | Request token log probabilities when the selected provider route supports them |
| `top_logprobs` | int | nil | Request top alternative token log probabilities where supported |
| `response_format` | string | `"text"` | `"text"` or `"json"`; with `output_schema`/`json_schema`, `"json"` selects schema-validated JSON rather than loose JSON-object mode |
| `output_format` | string/dict | `{kind: "text"}` | Provider-neutral output shape: `"text"`, `"json_object"`, or `{kind: "json_schema", schema, strict?}` |
| `schema` / `json_schema` / `output_schema` | dict | nil | JSON Schema, OpenAPI Schema Object, canonical Harn schema dict, or `Schema<T>` type alias for structured output |
| `output_validation` | string | `"off"` | `"error"` throws after exhausted schema retries; `"warn"` logs and returns the final envelope; `"off"` returns the final envelope without a warning |
| `schema_retries` | int | `1` | Re-prompt on schema validation failure with a corrective user message. Applies to direct and `routing_policy` calls. |
| `schema_stream_abort` | bool | inferred | Defaults to `true` when `output_schema` is set. Aborts impossible streaming JSON early and consumes one `schema_retries` slot. |
| `llm_retries` | int | `0` | (deprecated; prefer `with_retry` from `std/llm/handlers`) Retries on transient HTTP / provider errors. Raw `llm_call` is fail-fast by default; set to N to allow N retries after the first attempt. Off-by-one: `llm_retries: 3` ≈ `with_retry(..., {max_attempts: 4})` |
| `llm_backoff_ms` | int | `250` | (deprecated; prefer `with_retry`) Base exponential backoff in ms between LLM retries |
| `reasoning_policy` / `thinking_policy` | string/bool | nil | Provider-aware reasoning policy. Values: `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`; `none`, `disabled`, and `no_think` alias to `off`. Harn lowers this to the selected route's native thinking shape. Explicit `thinking` or `reasoning_effort` wins. |
| `reasoning_scale` / `problem_scale` | string | `"medium"` | Scale hint for `reasoning_policy: "auto"`: `small`, `medium`, or `large`. |
| `reasoning_task` | string | inferred | Task hint for `reasoning_policy: "auto"`: `chat`, `agent`, `code`, `verify`, or `summarize`. |
| `thinking` | bool/dict | nil | Enable typed provider reasoning. `true` and `{budget_tokens: N}` remain shorthand for `{mode: "enabled"}`; use `{mode: "enabled", budget_tokens: N}`, `{mode: "adaptive"}`, or `{mode: "effort", level: "none" \| "minimal" \| "low" \| "medium" \| "high" \| "xhigh"}`. On Anthropic Opus models that declare interleaved-thinking support, `{mode: "enabled"}` also sends `anthropic-beta: interleaved-thinking-2025-05-14`. When `thinking: false` is set on a model whose chat template uses an in-prompt directive (Qwen3's `/no_think`), Harn auto-prepends the directive to the system message — `thinking: false` works uniformly across providers without scripts needing to know per-template prompt syntax. |
| `interleaved_thinking` | bool | `false` | Add Anthropic's `interleaved-thinking-2025-05-14` beta header for this call. `thinking: true` enables it automatically on supported Anthropic Opus models. |
| `anthropic_beta_features` | string/list | nil | Extra Anthropic beta feature names to pass in the comma-separated `anthropic-beta` header on Anthropic-style routes. |
| `vision` | bool | inferred | Require image-input support. Image content blocks set this implicitly; `vision: true` fails before transport unless the selected provider/model declares `vision_supported`. |
| `tools` | list | nil | Tool definitions |
| `tool_choice` | string/dict | `"auto"` | `"auto"`, `"none"`, `"required"`, or `{name: "tool"}` |
| `tool_search` | bool/string/dict | nil | Progressive tool disclosure. See [Tool Vault](./tools.md#tool-vault) |
| `api_mode` | string | `"chat_completions"` | OpenAI only: set `"responses"` to use Harn's native OpenAI Responses path. Generic OpenAI-compatible providers stay on chat completions. |
| `provider_tools` / `hosted_tools` | list | nil | OpenAI Responses only. Pass provider-hosted tools such as `{type: "web_search"}`, `{type: "file_search", ...}`, or `{type: "mcp", server_label, server_url, require_approval}`. Harn records provider-native IDs and normalized metadata but does not execute these tools locally. |
| `previous_response_id` | string | nil | OpenAI Responses conversation-state link. Use only when provider-side state is desired instead of replaying the full Harn transcript. |
| `response_store` / `responses_store` | bool | provider default | OpenAI Responses persistence flag. A bool `store` is also accepted for direct raw Responses calls, but cache handlers reserve `store: {backend...}` for cache storage configuration. |
| `background` | bool | provider default | OpenAI Responses background-mode flag. |
| `truncation` | string | provider default | OpenAI Responses provider-side truncation/compaction policy such as `"auto"`. |
| `compact` | bool | `false` | OpenAI Responses standalone compaction. When `true`, Harn posts the request to `/responses/compact` and returns provider compaction items in `result.blocks`. |
| `include` | list | nil | OpenAI Responses metadata expansions to request. |
| `max_tool_calls` | int | nil | OpenAI Responses provider-executed tool-call limit. |
| `budget` | dict | nil | Pre-flight LLM budget envelope. Supports `max_cost_usd`, `max_input_tokens`, `max_output_tokens`, and `total_budget_usd` |
| `cache` | bool | `false` | Enable prompt caching (Anthropic) |
| `fast` | bool | `false` | Opt into the model's accelerated-serving ("fast mode") tier. Maps to the per-provider knob declared in the catalog (`speed` for Anthropic, `service_tier` for OpenAI) and injects the Anthropic beta header when required. Rejected for models with no `fast_mode` tier or a deprecated one. Billed at the catalog's premium `fast_mode.pricing` only when the provider confirms it served fast (`result.served_fast`). `speed: "fast"` is accepted as an alias. |
| `stream` | bool | `true` | Use streaming SSE transport. Set `false` for synchronous request/response. Env: `HARN_LLM_STREAM` |
| `timeout` | int | `120` | Request timeout in seconds. `timeout_ms` accepted as an alias and rounded up to whole seconds (HTTP transports take `Duration::from_secs`); sub-second budgets must be enforced at the caller. |
| `messages` | list | nil | Full message list (overrides prompt) |
| `structural_experiment` | string/dict/closure | nil | Prompt-structure transform applied immediately before the provider call. Built-ins: `prompt_order_permutation(seed: N)`, `doubled_prompt`, `chain_of_draft`, `inverted_system`. Env: `HARN_STRUCTURAL_EXPERIMENT` |
| `transcript` | dict | nil | Continue from a previous transcript; prompt is appended as the next user turn |
| `model_tier` | string | nil | Resolve a configured tier alias such as `"small"`, `"mid"`, or `"frontier"` |

The `cache` option above enables provider-side prompt caching when a provider
supports it. It does not memoize full LLM responses. For Harn-owned response
caching, import `with_cache` from `std/llm/handlers`:

Model roles are ordinary option defaults, so they compose with the
existing routing layer instead of bypassing it:

```toml
[model_roles.merge]
provider = "ollama"
model = "devstral-small-2"
temperature = 0.0
route_policy = "manual"
```

```harn
import { LlmCallOptions } from "std/llm/options"

let merge_opts: LlmCallOptions = {model_role: "merge", output_schema: schema}
let merged = llm_call(prompt, sys, merge_opts)
```

```harn
import { with_cache } from "std/llm/handlers"

let result = with_cache("Summarize this file", nil, {
  provider: "anthropic",
  model: "claude-haiku-4-5",
  store: {backend: "sqlite", namespace: "summaries"},
  ttl: "10m",
  max_entries: 256,
})
```

`with_cache` returns the same envelope as `llm_call`. Its key is
content-addressed as `sha256:` over canonical JSON for `{prompt, system,
provider, model, temperature, top_p, max_tokens}` after defaults resolve. The
default store is sqlite under Harn state, namespace `llm.with_cache`, TTL 10
minutes, and LRU size 256. Use `store: {backend: "fs", namespace, path?}` for
one-file-per-entry storage. Calls with `tools` bypass the cache by default;
set `skip_when` to a bool or predicate closure to override that policy.

Provider-specific overrides can be passed as sub-dicts (provider-named
keys ride alongside the typed alias):

```harn
import { LlmCallOptions } from "std/llm/options"

let opts: LlmCallOptions = {
  provider: "ollama",
  ollama: {num_ctx: 32768},
}
let result = llm_call("hello", nil, opts)
```

### OpenAI Responses mode

Set `api_mode: "responses"` with `provider: "openai"` when the call should
use OpenAI's native Responses API instead of the generic
`/chat/completions` adapter:

```harn
import { LlmCallOptions } from "std/llm/options"

let opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-5.4",
  api_mode: "responses",
  output_format: {kind: "json_schema", schema: summary_schema, strict: true},
  provider_tools: [
    {type: "web_search"},
    {type: "mcp", server_label: "docs", server_url: "https://mcp.example.com", require_approval: "always"},
  ],
  truncation: "auto",
  max_tool_calls: 4,
}
let result = llm_call("Search and summarize current docs.", nil, opts)
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

let experiment_opts: LlmCallOptions = {
  provider: "mock",
  structural_experiment: "prompt_order_permutation(seed: 42)",
}
let result = llm_call("Instruction\n\nContext block", nil, experiment_opts)
```

For custom transforms, pass a closure (or a `std/experiments.custom(...)`
spec) that rewrites `{messages, system}` and returns either `nil`, a new
message list, or `{messages?, system?, metadata?}`.

## llm_call_structured

`llm_call_structured(prompt, schema, options?)` is the ergonomic
helper for the "ask for JSON against this schema, retry on
validation failure, return just the parsed data" pattern. It wraps
`llm_call` and pre-applies the schema-validated-JSON defaults so
callsites stop repeating the same four options.

```harn
import { LlmCallOptions } from "std/llm/options"

let schema = {
  type: "object",
  required: ["name", "age"],
  properties: {
    name: {type: "string"},
    age: {type: "integer"},
  },
}
let structured_opts: LlmCallOptions = {provider: "anthropic", system: "You are precise."}
let person = llm_call_structured(
  "Extract the speaker's name and age from the transcript.",
  schema,
  structured_opts,
)
log(person.name)
log(person.age)
```

### Parameters

| Parameter | Type | Required | Description |
|---|---|---|---|
| prompt | string | yes | The user message |
| schema | dict or `Schema<T>` | yes | JSON Schema dict or a type alias in value position. When passed a `Schema<T>` the return narrows to `T`. |
| options | dict | no | Any option `llm_call` accepts, plus `system` (lifted into the system-message slot) and `retries` (alias for `schema_retries`) |

### Return value

The validated `data` payload, typed as `T` when the schema is a
`Schema<T>`. Throws on exhausted schema retries or transport
failure — callers can assume the return matches the schema.

The `{response_format: "json", output_validation: "error",
schema_retries: 3}` defaults are applied unless the caller
overrides them in `options`.

### Non-throwing variant

`llm_call_structured_safe(prompt, schema, options?)` returns the
`{ok, data, error}` envelope (mirroring `llm_call_safe` but with
the validated `.data` pre-unwrapped) instead of throwing:

```harn
let r = llm_call_structured_safe(prompt, schema, {provider: "openai"})
if !r.ok {
  log("structured call failed:", r.error.category, r.error.message)
  return nil
}
let person = r.data
```

`r.error.category` is one of the canonical `ErrorCategory` strings
(`"rate_limit"`, `"timeout"`, `"schema_validation"`, `"auth"`,
`"budget_exceeded"`, `"transient_network"`, `"generic"`, …) — match on the category
instead of string-sniffing the message.

### Diagnostic envelope variant

`llm_call_structured_result(prompt, schema, options?)` returns the
full failure-mode envelope production agent pipelines need, so
callers can keep raw model text, attempt counts, and validation /
repair state without hand-rolling parse / repair chains. It never
throws on transport or schema failures — `ok: false` plus
`error_category` distinguishes the failure mode.

```harn
import { LlmCallOptions } from "std/llm/options"

let result_opts: LlmCallOptions = {
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
let r = llm_call_structured_result(prompt, schema, result_opts)
if r.ok {
  let person = r.data
  // ...
} else {
  log("structured call failed:", r.error_category, "raw:", r.raw_text)
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
| `repaired` | bool | `true` when the repair pass produced valid JSON. |
| `extracted_json` | bool | `true` when JSON had to be lifted from prose / markdown fences. |
| `usage` | `{input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_creation_input_tokens, cache_hit_ratio, cache_savings_usd, served_fast}` | Token and prompt-cache accounting from the final attempt. |
| `model` | string | Model that produced the final attempt. |
| `provider` | string | Provider that produced the final attempt. |

Repair-pass semantics:

- The `repair` block is recognized only by
  `llm_call_structured_result`. Pass `repair: {enabled: true, ...}`
  to enable it; presence of the dict implies opt-in.
- Repair runs at most once, with `schema_retries: 0`, only when the
  main call ended with malformed JSON or schema-invalid output. It
  is skipped on transport failures because there is no raw text to
  salvage.
- Override keys (`model`, `provider`, `max_tokens`, `system`, …) are
  merged onto the main call's options for the repair attempt.

### When to use which helper

- Product code that needs just the parsed payload: prefer
  `llm_call_structured`. It removes the `output_validation`,
  `schema_retries`, `response_format`, and `.data` noise from every
  callsite.
- Code that also needs token counts, transcript, thinking traces, or
  to pass a pre-built transcript: call `llm_call` directly and read
  `.text` / `.data` / `.input_tokens` / etc. off the full result
  dict.
- Call sites that prefer explicit branching over `try` blocks:
  `llm_call_structured_safe` (the non-throwing envelope).
- Production agent pipelines that need raw-text retention, attempt
  counts, and an optional repair pass on malformed JSON:
  `llm_call_structured_result` — replaces the
  `llm_call → response.data → safe_parse → json_extract → repair →
  schema_check` chain that downstream callers would otherwise
  hand-roll.

## Composable callers

`agent_loop` accepts an `llm_caller:` option — a closure that owns
each turn's `llm_call(...)`. Wrap it with middleware from
`std/llm/handlers` (retry / fallback / shadow / logging / budget /
cache / circuit breaker) to compose resilience without forking the
loop:

```harn,ignore
import {default_llm_caller, with_retry} from "std/llm/handlers"

let caller = with_retry(default_llm_caller(), {max_attempts: 4})

let result = agent_loop(task, system, {
  loop_until_done: true,
  llm_caller: caller,
})
```

Caller contract:

```harn,ignore
fn(call) -> {ok: true, value: <llm dict>}
          | {ok: false, status: <reserved>, error?: any, retryable?: bool}
//   call = {prompt, system, opts, turn: {iteration, session_id, attempt}}
```

`with_retry`'s `max_attempts: N` counts total attempts. Migrating
`llm_retries: K` (deprecated): pass `max_attempts: K + 1` — the legacy
option counted retries *after* the first attempt.

See [Composable callers and middleware](../stdlib/llm-handlers.md)
for the full module catalog (`handlers`, `ensemble`, `refine`,
`budget`, `defaults`, `safe`, `prompts`, `catalog`).

## llm_completion

Use `llm_completion` for text continuation and fill-in-the-middle generation.
It lives at the same abstraction level as `llm_call`.

```harn
import { LlmCallOptions } from "std/llm/options"

let completion_opts: LlmCallOptions = {
  provider: "ollama",
  model_tier: "small",
}
let result = llm_completion("let total = ", ";", nil, completion_opts)
log(result.text)
```

## Cost tracking

Harn provides builtins for estimating and controlling LLM costs:

```harn
// Estimate cost for a specific call
let cost = llm_cost("claude-sonnet-5", 1000, 500)
log("Estimated cost: $${cost}")

// Check cumulative session costs
let session = llm_session_cost()
log("Total: $${session.total_cost}")
log("Calls: ${session.call_count}")
log("Input tokens: ${session.input_tokens}")
log("Output tokens: ${session.output_tokens}")

// Set a budget (LLM calls throw if exceeded)
llm_budget(1.00)
log("Remaining: $${llm_budget_remaining()}")
```

For per-call controls, pass a `budget` envelope on `llm_call` (the typed
shape is `LlmBudget` from `std/llm/options`):

```harn
import { LlmBudget, LlmCallOptions } from "std/llm/options"

let budget: LlmBudget = {
  max_cost_usd: 0.001,
  max_input_tokens: 8000,
  max_output_tokens: 1024,
}
let budgeted_opts: LlmCallOptions = {
  provider: "openai",
  model: "gpt-4o",
  max_tokens: 1024,
  budget: budget,
}
let result = try {
  llm_call("Summarize this", nil, budgeted_opts)
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
| `llm_session_cost()` | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `llm_budget(max_cost)` | Set session budget in USD. LLM calls throw if exceeded |
| `llm_budget_remaining()` | Remaining budget (nil if no budget set) |
| `tiktoken_count_tokens(text, model)` | Count text with the selected tiktoken encoder for known OpenAI/Claude/Gemini model families |

Import `std/llm/budget` for reusable helpers such as
`estimate_text_tokens_detail(text, model)`, which includes the encoder label
(`cl100k_base`, `o200k_base`, etc.) and whether the count is exact or an
approximation.

## Testing with mock LLM responses

The `mock` provider returns deterministic responses without API keys.
Use `llm_mock()` to queue specific responses — text, tool calls, or both:

```harn
// Queue a text response (consumed in FIFO order)
llm_mock({text: "The capital of France is Paris."})
let r = llm_call("What is the capital of France?", nil, {provider: "mock"})
assert_eq(r.text, "The capital of France is Paris.")

// Queue a response with tool calls
llm_mock({
  text: "Let me read that file.",
  tool_calls: [{name: "read_file", arguments: {path: "src/main.rs"}}],
})

// Queue token logprobs for confidence/reranking tests
llm_mock({text: "certain", logprobs: [{token: "certain", logprob: 0.0}]})

// Pattern-matched mocks (reusable by default, matched in declaration order)
llm_mock({text: "I don't know.", match: "*unknown*"})
llm_mock({text: "step 1", match: "*planner*", consume_match: true})
llm_mock({text: "step 2", match: "*planner*", consume_match: true})

// Provider-style error envelopes exercise the same catch/safe-call paths
// as live provider failures.
llm_mock({error: {status: 503, kind: "transient", reason: "upstream_unavailable"}})

// Inspect what was sent to the mock provider
let calls = llm_mock_calls()
// Each entry: {messages: [...], system: "..." or nil, tools: [...] or nil}

// Clear all mocks and call log between tests
llm_mock_clear()
```

When no `llm_mock()` responses are queued, the mock provider falls back to
its default deterministic behavior (echoing prompt metadata). This means
existing tests using `provider: "mock"` without `llm_mock()` continue to
work unchanged.
