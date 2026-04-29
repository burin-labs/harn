# LLM calls

## llm_call

Make a single LLM request. Harn normalizes provider responses into a
canonical dict so product code does not need to parse provider-native
message shapes.

```harn
let result = llm_call("What is 2 + 2?")
println(result.text)
```

With a system message:

```harn
let result = llm_call(
  "Explain quicksort",
  "You are a computer science teacher. Be concise."
)
println(result.text)
```

With options:

```harn
let result = llm_call(
  "Translate to French: Hello, world",
  "You are a translator.",
  {
    provider: "openai",
    model: "gpt-4o",
    max_tokens: 1024
  }
)
println(result.text)
```

With image content:

```harn
let image = bytes_to_base64(read_file_bytes("diagram.png"))
let result = llm_call("", nil, {
  provider: "openai",
  model: "gpt-4o",
  messages: [{
    role: "user",
    content: [
      {type: "text", text: "Summarize this diagram."},
      {type: "image", base64: image, media_type: "image/png", detail: "auto"},
    ],
  }],
})
println(result.text)
```

Image blocks use the provider-neutral shape
`{type: "image", url?: string, base64?: string, media_type: string, detail?: "low"|"high"|"auto"}`.
Exactly one of `url` or `base64` is required. Harn translates it to
Anthropic `source`, OpenAI `image_url`, Gemini `inline_data`/`file_data`,
or Ollama `images` fields at the provider boundary. Ollama's REST API
only accepts base64 image data, so `url` image blocks are rejected for
`provider: "ollama"`.

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
| `usage` | dict | Token and prompt-cache accounting fields, including the cache fields above |
| `data` | any | Parsed JSON (when `response_format: "json"`) |
| `tool_calls` | list | Tool calls (when model uses tools) |
| `thinking` | string | Reasoning trace (when `thinking` is enabled) |
| `private_reasoning` | string | Provider reasoning metadata kept separate from visible text |
| `blocks` | list | Canonical structured content blocks across providers |
| `stop_reason` | string | `"end_turn"`, `"max_tokens"`, `"tool_use"`, `"stop_sequence"` |
| `transcript` | dict | Transcript carrying message history, events, summary, metadata, and id |

### Options dict

| Key | Type | Default | Description |
|---|---|---|---|
| `provider` | string | `"anthropic"` | Any configured provider. Built-in names include `"anthropic"`, `"openai"`, `"openrouter"`, `"huggingface"`, `"ollama"`, `"gemini"`, and `"local"` |
| `model` | string | varies by provider | Model identifier |
| `max_tokens` | int | `16384` | Maximum tokens in the response |
| `temperature` | float | provider default | Sampling temperature (0.0-2.0) |
| `top_p` | float | nil | Nucleus sampling |
| `top_k` | int | nil | Top-K sampling (Anthropic/Ollama only) |
| `stop` | list | nil | Stop sequences |
| `seed` | int | nil | Reproducibility seed (OpenAI/Ollama) |
| `frequency_penalty` | float | nil | Frequency penalty (OpenAI only) |
| `presence_penalty` | float | nil | Presence penalty (OpenAI only) |
| `response_format` | string | `"text"` | `"text"` or `"json"` |
| `schema` | dict | nil | JSON Schema, OpenAPI Schema Object, or canonical Harn schema dict for structured output |
| `llm_retries` | int | `0` | Retries on transient HTTP / provider errors. Raw `llm_call` is fail-fast by default; set to N to allow N retries after the first attempt |
| `llm_backoff_ms` | int | `250` | Base exponential backoff in ms between LLM retries |
| `thinking` | bool/dict | nil | Enable typed provider reasoning. `true` and `{budget_tokens: N}` remain shorthand for `{mode: "enabled"}`; use `{mode: "enabled", budget_tokens: N}`, `{mode: "adaptive"}`, or `{mode: "effort", level: "low" \| "medium" \| "high"}`. On Anthropic Opus models that declare interleaved-thinking support, `{mode: "enabled"}` also sends `anthropic-beta: interleaved-thinking-2025-05-14`. |
| `interleaved_thinking` | bool | `false` | Add Anthropic's `interleaved-thinking-2025-05-14` beta header for this call. `thinking: true` enables it automatically on supported Anthropic Opus models. |
| `anthropic_beta_features` | string/list | nil | Extra Anthropic beta feature names to pass in the comma-separated `anthropic-beta` header on Anthropic-style routes. |
| `vision` | bool | inferred | Require image-input support. Image content blocks set this implicitly; `vision: true` fails before transport unless the selected provider/model declares `vision_supported`. |
| `tools` | list | nil | Tool definitions |
| `tool_choice` | string/dict | `"auto"` | `"auto"`, `"none"`, `"required"`, or `{name: "tool"}` |
| `tool_search` | bool/string/dict | nil | Progressive tool disclosure. See [Tool Vault](./tools.md#tool-vault) |
| `budget` | dict | nil | Pre-flight LLM budget envelope. Supports `max_cost_usd`, `max_input_tokens`, `max_output_tokens`, and `total_budget_usd` |
| `cache` | bool | `false` | Enable prompt caching (Anthropic) |
| `stream` | bool | `true` | Use streaming SSE transport. Set `false` for synchronous request/response. Env: `HARN_LLM_STREAM` |
| `timeout` | int | `120` | Request timeout in seconds |
| `messages` | list | nil | Full message list (overrides prompt) |
| `structural_experiment` | string/dict/closure | nil | Prompt-structure transform applied immediately before the provider call. Built-ins: `prompt_order_permutation(seed: N)`, `doubled_prompt`, `chain_of_draft`, `inverted_system`. Env: `HARN_STRUCTURAL_EXPERIMENT` |
| `transcript` | dict | nil | Continue from a previous transcript; prompt is appended as the next user turn |
| `model_tier` | string | nil | Resolve a configured tier alias such as `"small"`, `"mid"`, or `"frontier"` |

Provider-specific overrides can be passed as sub-dicts:

```harn
let result = llm_call("hello", nil, {
  provider: "ollama",
  ollama: {num_ctx: 32768}
})
```

Structural experiments can be enabled directly on a call:

```harn
let result = llm_call("Instruction\n\nContext block", nil, {
  provider: "mock",
  structural_experiment: "prompt_order_permutation(seed: 42)",
})
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
let schema = {
  type: "object",
  required: ["name", "age"],
  properties: {
    name: {type: "string"},
    age: {type: "integer"},
  },
}
let person = llm_call_structured(
  "Extract the speaker's name and age from the transcript.",
  schema,
  {provider: "anthropic", system: "You are precise."},
)
println(person.name)
println(person.age)
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
let r = llm_call_structured_result(prompt, schema, {
  provider: "auto",
  schema_retries: 2,
  // Optional repair pass — runs only on malformed JSON or
  // schema-invalid output. Skipped on transport failures.
  repair: {
    enabled: true,
    model: "cheapest_over_quality(low)",
    max_tokens: 600,
  },
})
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
| `usage` | `{input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cache_creation_input_tokens, cache_hit_ratio, cache_savings_usd}` | Token and prompt-cache accounting from the final attempt. |
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

## llm_completion

Use `llm_completion` for text continuation and fill-in-the-middle generation.
It lives at the same abstraction level as `llm_call`.

```harn
let result = llm_completion("let total = ", ";", nil, {
  provider: "ollama",
  model_tier: "small"
})
println(result.text)
```

## Cost tracking

Harn provides builtins for estimating and controlling LLM costs:

```harn
// Estimate cost for a specific call
let cost = llm_cost("claude-sonnet-4-20250514", 1000, 500)
println("Estimated cost: $${cost}")

// Check cumulative session costs
let session = llm_session_cost()
println("Total: $${session.total_cost}")
println("Calls: ${session.call_count}")
println("Input tokens: ${session.input_tokens}")
println("Output tokens: ${session.output_tokens}")

// Set a budget (LLM calls throw if exceeded)
llm_budget(1.00)
println("Remaining: $${llm_budget_remaining()}")
```

For per-call controls, pass a `budget` envelope on `llm_call`:

```harn
let result = try {
  llm_call("Summarize this", nil, {
    provider: "openai",
    model: "gpt-4o",
    max_tokens: 1024,
    budget: {
      max_cost_usd: 0.001,
      max_input_tokens: 8000,
      max_output_tokens: 1024,
    },
  })
}
```

Harn estimates prompt tokens before the provider request leaves the process,
projects cost with the provider/model pricing table, and throws a terminal
`budget_exceeded` dict when a limit would be exceeded. In a `try { ... }`
expression that surfaces as `Result.Err({kind: "terminal", reason:
"budget_exceeded", projected_cost_usd: ...})`.

`agent_loop` accepts the same envelope. `max_*` limits apply to each model turn;
`total_budget_usd` is an aggregate loop budget and exits gracefully with
`status: "budget_exhausted"` before starting a turn that would exceed it.

| Function | Description |
|---|---|
| `llm_cost(model, input_tokens, output_tokens)` | Estimate USD cost from embedded pricing table |
| `llm_session_cost()` | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `llm_budget(max_cost)` | Set session budget in USD. LLM calls throw if exceeded |
| `llm_budget_remaining()` | Remaining budget (nil if no budget set) |

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

// Pattern-matched mocks (reusable by default, matched in declaration order)
llm_mock({text: "I don't know.", match: "*unknown*"})
llm_mock({text: "step 1", match: "*planner*", consume_match: true})
llm_mock({text: "step 2", match: "*planner*", consume_match: true})

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
