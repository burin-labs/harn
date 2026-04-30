# LLM and agents

Harn has built-in support for calling language models, streaming responses,
running persistent agent loops, and delegating work to child agents. This page is
the map; the detailed references now live in focused pages.

## Start here

| Topic | Use it for |
|---|---|
| [`llm_call`](./llm/llm_call.md) | Single model requests, structured JSON output, completions, budgets, and mock responses |
| [`agent_loop`](./llm/agent_loop.md) | Persistent agents, profiles, daemon loops, skills, and delegated workers |
| [Tools](./llm/tools.md) | Typed tools, Tool Vault progressive disclosure, and MCP server tools |
| [Streaming](./llm/streaming.md) | `llm_stream`, partial deltas, transcripts, workflow sessions, and token usage summaries |
| [Providers](./llm/providers.md) | Provider setup, API details, local servers, enterprise cloud providers, and capability overrides |

## Providers

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter, Ollama,
HuggingFace, Bedrock, Azure OpenAI, Vertex AI, and local OpenAI-compatible
servers. Most scripts choose a provider with the `provider` option, the
`HARN_LLM_PROVIDER` environment variable, or a model name that Harn can infer.

See [LLM providers](./llm/providers.md) for API keys, local model setup,
enterprise provider notes, Ollama runtime environment variables, and the
capability matrix.

## Capability matrix + `harn.toml` overrides

Provider capability rules, project overrides, and packaged provider adapters now
live in [LLM providers](./llm/providers.md#capability-matrix--harntoml-overrides).

## llm_call

Use `llm_call(prompt, system?, options?)` for a single model turn. It returns a
canonical dict with `text`, `visible_text`, `model`, `provider`, token usage,
structured `data` when JSON mode is enabled, tool calls, thinking blocks, and a
transcript.

```harn
let result = llm_call("Translate to French: Hello, world", nil, {
  provider: "openai",
  model: "gpt-4o",
  max_tokens: 1024,
})
println(result.text)
```

For schema-validated JSON, use `llm_call_structured(...)` or its safe/result
envelope variants. See [LLM calls](./llm/llm_call.md) for the full options and
return-value tables.

## llm_call_structured

Use `llm_call_structured(prompt, schema, options?)` for schema-validated JSON
responses. Safe and diagnostic-envelope variants are documented in
[LLM calls](./llm/llm_call.md#llm_call_structured).

## llm_completion

Use `llm_completion(prefix, suffix?, system?, options?)` for text continuation
and fill-in-the-middle generation. It shares provider, model, budget, and usage
semantics with `llm_call`.

## Tool Vault

Tool Vault is Harn's progressive-tool-disclosure primitive. Tools marked
`defer_loading: true` stay out of the prompt-visible tool surface until native
or client-executed `tool_search` promotes them. Use it for large tool registries
and MCP-heavy agents.

Typed tool patterns, Tool Vault options, provider support, and MCP server tool
prefixing are covered in [LLM tools](./llm/tools.md).

## agent_loop

Use `agent_loop(prompt, system?, options?)` when an agent should keep working
across turns. Persistent loops continue until the model emits the completion
sentinel, a budget or iteration limit is reached, daemon state idles, or a tool
policy fails.

```harn
let result = agent_loop(
  "Write a function that sorts a list, then write tests for it.",
  "You are a senior engineer.",
  {persistent: true, profile: "tool_using"}
)
println(result.status)
println(result.llm.iterations)
```

The result is namespaced as `llm`, `tools`, `trace`, `task_ledger`, and
`transcript`. Profiles preload common loop budgets for tool-using, researcher,
verifier, and completer loops. See [Agent loops](./llm/agent_loop.md).

## Daemon stdlib wrappers

Use `daemon_spawn`, `daemon_trigger`, `daemon_snapshot`, `daemon_stop`, and
`daemon_resume` when you want first-class daemon handles instead of wiring daemon
options on `agent_loop` directly.

## Skills lifecycle

Skills bundle metadata, a system-prompt fragment, scoped tools, and lifecycle
hooks into a typed unit. Pass a skill registry to `agent_loop` with the
`skills:` option to match, activate, scope, and optionally deactivate skills
across turns. See [Agent loops](./llm/agent_loop.md#skills-lifecycle).

## Streaming responses

`llm_stream` returns a channel of response chunks. It accepts the same provider,
model, and generation options as `llm_call`; the channel closes when the response
is complete. See [Streaming and transcripts](./llm/streaming.md).

## Delegated workers

For long-running or parallel orchestration, `spawn_agent`, `sub_agent_run`,
`wait_agent`, `send_input`, `resume_agent`, and `close_agent` expose child-agent
lifecycle directly in the runtime. See
[delegated workers](./llm/agent_loop.md#delegated-workers).

## Transcript management

Transcripts carry context across calls, forks, repairs, resumptions, and workflow
sessions. Use `transcript_render_visible`, `transcript_render_full`,
`transcript_events`, `transcript_summarize`, and `transcript_compact` when host
apps need stable rendering and replay boundaries. See
[Streaming and transcripts](./llm/streaming.md#transcript-management).

## Workflow runtime

For multi-stage orchestration, prefer workflow graphs and `workflow_execute`
over product-side loop wiring. This keeps orchestration structure, transcript
policy, context policy, artifacts, and retries inside Harn. See
[workflow runtime notes](./llm/streaming.md#workflow-runtime).

## Cost tracking

`llm_call`, `agent_loop`, and workflow sessions expose normalized token usage.
Use `llm_cost`, `llm_session_cost`, `llm_budget`, `llm_budget_remaining`, and
per-call `budget` envelopes to estimate and enforce spend before provider
requests leave the process. See [LLM calls](./llm/llm_call.md#cost-tracking).

## Provider API details

Provider-specific endpoint, auth, readiness, and local-server notes are in
[LLM providers](./llm/providers.md#provider-api-details).

## Testing with mock LLM responses

The `mock` provider and `llm_mock(...)` queue deterministic text, tool-call, and
error responses without API keys. See
[mock LLM responses](./llm/llm_call.md#testing-with-mock-llm-responses).
