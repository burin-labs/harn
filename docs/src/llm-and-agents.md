# LLM calls and agent loops

Harn has built-in support for calling language models and running persistent agent loops. No libraries or SDKs needed.

## Providers

Harn supports four LLM providers. Set the appropriate environment variable to authenticate:

| Provider | Environment variable | Default model |
|---|---|---|
| Anthropic (default) | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-sonnet-4-20250514` |
| Ollama | `OLLAMA_HOST` (optional) | `llama3.2` |

Ollama runs locally and doesn't require an API key. The default host is `http://localhost:11434`.

## llm_call

Make a single LLM request. Always returns a dict with `text`, `model`,
`input_tokens`, and `output_tokens` fields.

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
| `model` | string | The model used |
| `input_tokens` | int | Input/prompt token count |
| `output_tokens` | int | Output/completion token count |
| `data` | any | Parsed JSON (when `response_format: "json"`) |
| `tool_calls` | list | Tool calls (when model uses tools) |
| `thinking` | string | Reasoning trace (when `thinking` is enabled) |
| `stop_reason` | string | `"end_turn"`, `"max_tokens"`, `"tool_use"`, `"stop_sequence"` |
| `transcript` | dict | Transcript carrying message history, summary, metadata, and id |

### Options dict

| Key | Type | Default | Description |
|---|---|---|---|
| `provider` | string | `"anthropic"` | `"anthropic"`, `"openai"`, `"ollama"`, or `"openrouter"` |
| `model` | string | varies by provider | Model identifier |
| `max_tokens` | int | `4096` | Maximum tokens in the response |
| `temperature` | float | provider default | Sampling temperature (0.0-2.0) |
| `top_p` | float | nil | Nucleus sampling |
| `top_k` | int | nil | Top-K sampling (Anthropic/Ollama only) |
| `stop` | list | nil | Stop sequences |
| `seed` | int | nil | Reproducibility seed (OpenAI/Ollama) |
| `frequency_penalty` | float | nil | Frequency penalty (OpenAI only) |
| `presence_penalty` | float | nil | Presence penalty (OpenAI only) |
| `response_format` | string | `"text"` | `"text"` or `"json"` |
| `schema` | dict | nil | JSON Schema for structured output |
| `thinking` | bool/dict | nil | Enable extended reasoning. `true` or `{budget_tokens: N}` |
| `tools` | list | nil | Tool definitions |
| `tool_choice` | string/dict | `"auto"` | `"auto"`, `"none"`, `"required"`, or `{name: "tool"}` |
| `cache` | bool | `false` | Enable prompt caching (Anthropic) |
| `timeout` | int | `120` | Request timeout in seconds |
| `messages` | list | nil | Full message list (overrides prompt) |
| `transcript` | dict | nil | Continue from a previous transcript; prompt is appended as the next user turn |
| `model_tier` | string | nil | Resolve a configured tier alias such as `"small"`, `"mid"`, or `"frontier"` |

Provider-specific overrides can be passed as sub-dicts:

```harn
let result = llm_call("hello", nil, {
  provider: "ollama",
  ollama: {num_ctx: 32768}
})
```

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

## agent_loop

Run an agent that keeps working until it's done. The agent maintains
conversation history across turns and loops until it outputs the
`##DONE##` sentinel. Returns a dict with `{status, text, iterations,
duration_ms, tools_used, transcript}`.

```harn
let result = agent_loop(
  "Write a function that sorts a list, then write tests for it.",
  "You are a senior engineer.",
  {persistent: true}
)
println(result.text)       // the accumulated output
println(result.status)     // "done" or "stuck"
println(result.iterations) // number of LLM round-trips
```

### How it works

1. Sends the prompt to the model
2. Reads the response
3. If `persistent: true`:
   - Checks if the response contains `##DONE##`
   - If yes, stops and returns the accumulated output
   - If no, sends a nudge message asking the agent to continue
   - Repeats until done or limits are hit
4. If `persistent: false` (default): returns after the first response

### agent_loop return value

`agent_loop` returns a dict with the following fields:

| Field | Type | Description |
|---|---|---|
| `status` | string | `"done"` or `"stuck"` |
| `text` | string | Accumulated text output from all iterations |
| `iterations` | int | Number of LLM round-trips |
| `duration_ms` | int | Total wall-clock time in milliseconds |
| `tools_used` | list | Names of tools that were called |
| `transcript` | dict | Transcript of the full conversation state |

### agent_loop options

Same as `llm_call`, plus additional options:

| Key | Type | Default | Description |
|---|---|---|---|
| `persistent` | bool | `false` | Keep looping until `##DONE##` |
| `max_iterations` | int | `50` | Maximum number of LLM round-trips |
| `max_nudges` | int | `3` | Max consecutive text-only responses before stopping |
| `nudge` | string | see below | Custom message to send when nudging the agent |
| `tool_retries` | int | `0` | Number of retry attempts for failed tool calls |
| `tool_backoff_ms` | int | `1000` | Base backoff delay in ms for tool retries (doubles each attempt) |

Default nudge message:

> You have not output ##DONE## yet — the task is not complete.
> Use your tools to continue working. Only output ##DONE## when
> the task is fully complete and verified.

When `persistent: true`, the system prompt is automatically extended with:

> IMPORTANT: You MUST keep working until the task is complete.
> Do NOT stop to explain or summarize — take action. Output ##DONE##
> only when the task is fully complete and verified.

### Example with retry

```harn
retry 3 {
  let result = agent_loop(
    task,
    "You are a coding assistant.",
    {
      persistent: true,
      max_iterations: 30,
      max_nudges: 5,
      provider: "anthropic",
      model: "claude-sonnet-4-20250514"
    }
  )
  println(result.text)
}
```

## Streaming responses

`llm_stream` returns a channel that yields response chunks as they
arrive. Iterate over it with a `for` loop:

```harn
let stream = llm_stream("Tell me a story", "You are a storyteller")
for chunk in stream {
  print(chunk)
}
```

`llm_stream` accepts the same options as `llm_call` (provider, model,
max_tokens). The channel closes automatically when the response is
complete.

## Transcript management

Harn includes transcript primitives for carrying context across calls,
forks, repairs, and resumptions:

```harn
let first = llm_call("Plan the work", nil, {provider: "mock"})
let second = llm_call("Continue", nil, {
  provider: "mock",
  transcript: first.transcript
})

let compacted = transcript_compact(second.transcript, {
  keep_last: 4,
  summary: "Planning complete."
})
```

Use `transcript_summarize()` when you want Harn to create a fresh summary with
an LLM, or `transcript_compact()` when you want a local compaction step.

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

| Function | Description |
|---|---|
| `llm_cost(model, input_tokens, output_tokens)` | Estimate USD cost from embedded pricing table |
| `llm_session_cost()` | Session totals: `{total_cost, input_tokens, output_tokens, call_count}` |
| `llm_budget(max_cost)` | Set session budget in USD. LLM calls throw if exceeded |
| `llm_budget_remaining()` | Remaining budget (nil if no budget set) |

## Provider API details

### Anthropic

- Endpoint: `https://api.anthropic.com/v1/messages`
- Auth: `x-api-key` header
- API version: `2023-06-01`
- System message sent as a top-level `system` field

### OpenAI

- Endpoint: `https://api.openai.com/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- System message sent as a message with `role: "system"`

### OpenRouter

- Endpoint: `https://openrouter.ai/api/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- Same message format as OpenAI

### Ollama

- Endpoint: `<OLLAMA_HOST>/v1/chat/completions`
- Default host: `http://localhost:11434`
- No authentication required
- Same message format as OpenAI
