# LLM calls and agent loops

Harn has built-in support for calling language models and running persistent agent loops. No libraries or SDKs needed.

## Providers

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter, Ollama,
HuggingFace, and a local OpenAI-compatible server. Set the appropriate
environment variable to authenticate or point Harn at a local endpoint:

| Provider | Environment variable | Default model |
|---|---|---|
| Anthropic (default) | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-sonnet-4-20250514` |
| HuggingFace | `HF_TOKEN` or `HUGGINGFACE_API_KEY` | explicit `model` |
| Ollama | `OLLAMA_HOST` (optional) | `llama3.2` |
| Local server | `LOCAL_LLM_BASE_URL` | `LOCAL_LLM_MODEL` or explicit `model` |

Ollama runs locally and doesn't require an API key. The default host is
`http://localhost:11434`.

For a generic OpenAI-compatible local server, set `LOCAL_LLM_BASE_URL` to
something like `http://192.168.86.250:8000` and either pass
`{provider: "local", model: "qwen2.5-coder-32b"}` or set
`LOCAL_LLM_MODEL=qwen2.5-coder-32b`.

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
| `provider` | string | `"anthropic"` | Any configured provider. Built-in names include `"anthropic"`, `"openai"`, `"openrouter"`, `"huggingface"`, `"ollama"`, and `"local"` |
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
| `thinking` | bool/dict | nil | Enable extended reasoning. `true` or `{budget_tokens: N}` |
| `tools` | list | nil | Tool definitions |
| `tool_choice` | string/dict | `"auto"` | `"auto"`, `"none"`, `"required"`, or `{name: "tool"}` |
| `tool_search` | bool/string/dict | nil | Progressive tool disclosure. See [Tool Vault](#tool-vault) |
| `cache` | bool | `false` | Enable prompt caching (Anthropic) |
| `stream` | bool | `true` | Use streaming SSE transport. Set `false` for synchronous request/response. Env: `HARN_LLM_STREAM` |
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

## Tool Vault

Harn's Tool Vault is the progressive-tool-disclosure primitive: tool
definitions that stay out of the model's context until they're
surfaced by a search call. This keeps context cheap for agents with
hundreds of tools (coding agents, MCP-heavy setups) without requiring
the integrator to hand-filter tools per turn.

### Per-tool flag: `defer_loading`

Any tool registered via `tool_define` (or the `tool { … }` language
form) can opt out of eager loading:

```harn
var registry = tool_registry()
registry = tool_define(registry, "deploy", "Deploy to production", {
  parameters: {env: {type: "string"}},
  defer_loading: true,
  handler: { args -> shell("deploy " + args.env) },
})
```

Deferred tools never appear in the model's context unless a
tool-search call surfaces them. They *are* sent to the provider (so
prompt caching stays warm on Anthropic — the schemas live in the
API prefix but not the model's context).

### Call-level option: `tool_search`

Turning progressive disclosure on is one option away:

```harn
let r = llm_call(prompt, sys, {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: registry,
  tool_search: "bm25",
})
```

Accepted shapes:

| Shape | Meaning |
|---|---|
| `tool_search: true` | Default: `bm25` variant, mode `auto`. |
| `tool_search: "bm25"` | Natural-language queries. |
| `tool_search: "regex"` | Python-regex queries. |
| `tool_search: false` | Explicit off (same as omitting). |
| `tool_search: {variant, mode, strategy, always_loaded, budget_tokens, name, include_stub_listing}` | Explicit dict form. |

`mode` options:

- `"auto"` (default) — use native if the provider supports it,
  otherwise fall back to the client-executed path (no error).
- `"native"` — force the provider's native mechanism. Errors if
  unsupported.
- `"client"` — force the client-executed path even on providers with
  native support. Useful for A/B-ing strategies or pinning behavior
  across heterogeneous provider fleets.

### Provider support

| Provider | Native `tool_search` | Variants / modes |
|---|---|---|
| Anthropic Claude Opus/Sonnet 4.0+, Haiku 4.5+ | ✓ | `bm25`, `regex` |
| Anthropic 3.x or earlier 4.x Haiku | ✗ (uses client fallback) | — |
| OpenAI Responses API — GPT 5.4+ | ✓ | `hosted` (default), `client` |
| OpenAI pre-5.4 (`gpt-4o`, `gpt-4.1`, …) | ✗ | client fallback works today |
| OpenRouter / Together / Groq / DeepSeek / Fireworks / HuggingFace / local | ✓ when routed model matches `gpt-5.4+` upstream | hosted forwarded; escape hatch below for proxies |
| Gemini, Ollama, mock (default model) | ✗ | client fallback works today |

The OpenAI native path (harn#71) emits a flat `{"type": "tool_search",
"mode": "hosted"}` meta-tool at the front of the tools array, alongside
`defer_loading: true` on the wrapper of each user tool. The server runs
the search and replies with `tool_search_call` / `tool_search_output`
entries that Harn parses into the same transcript event shape as the
Anthropic path (replays are indistinguishable across providers).

#### Namespace grouping

OpenAI's `tool_search` can group deferred tools into namespaces; pass
`namespace: "<label>"` on `tool_define(...)` to tag a tool. Harn collects
the distinct set into the meta-tool's `namespaces` field. Anthropic
ignores the label — harmless passthrough for replay fidelity.

```harn
tool_define(registry, "deploy_api", "Deploy the API", {
  parameters: {env: {type: "string"}},
  defer_loading: true,
  namespace: "ops",
  handler: { args -> shell("deploy api " + args.env) },
})
```

#### Escape hatch for proxied OpenAI-compat endpoints

Self-hosted routers and enterprise gateways sometimes advertise a model
ID Harn cannot parse (`my-internal-gpt-clone-v2`) yet forward the OpenAI
Responses payload unchanged. Opt into the hosted path with:

```harn
llm_call(prompt, sys, {
  provider: "openrouter",
  model: "my-custom/gpt-forward",
  tools: registry,
  tool_search: {mode: "native"},
  openrouter: {force_native_tool_search: true},
})
```

The override is keyed by the provider name (the same dict you'd use for
any provider-specific knob).

### Client-executed fallback

On providers without native `defer_loading`, Harn falls back to an
in-VM execution path (landed in [harn#70](https://github.com/burin-labs/harn/issues/70)).
The fallback is identical to the native path from a script's point of
view: same option surface, same transcript events, same promotion
behavior across turns. Internally, Harn injects a synthetic tool
called `__harn_tool_search` — when the model calls it, the loop runs
the configured strategy against the deferred-tool index, promotes the
matching tools into the *next* turn's schema list, and emits the
same `tool_search_query` / `tool_search_result` transcript events as
native mode (tagged `mode: "client"` in metadata so replays can
distinguish paths).

Strategies (client mode only):

| `strategy` | Runs in | Notes |
|---|---|---|
| `"bm25"` *(default)* | VM | Tokenized BM25 over `name + description + param text`. Matches `open_file` from query `open file`. |
| `"regex"` | VM | Case-insensitive Rust-regex over the same corpus. No backreferences, no lookaround. |
| `"semantic"` | Host (bridge) | Delegated to the host via `tool_search/query` so integrators can wire embeddings without Harn pulling in ML crates. |
| `"host"` | Host (bridge) | Pure host-side; the VM round-trips the query and promotes whatever the host returns. |

Extra client-mode knobs:

- `budget_tokens: N` — soft cap on the total token footprint of
  promoted tool schemas. Oldest-first eviction when exceeded. Omit to
  keep every promoted schema for the life of the call.
- `name: "find_tool"` — override the synthetic tool's name. Handy
  when a skill's vocabulary suggests a more natural verb (`discover`,
  `lookup`, …).
- `always_loaded: ["read_file", "run"]` — pin tool names to the eager
  set even if `defer_loading: true` is set on their registry entries.
- `include_stub_listing: true` — append a short list of deferred tool
  names + one-line descriptions to the tool-contract prompt so the
  model can eyeball what's available without a search call. Off by
  default to match Anthropic's native ergonomic.

### Pre-flight validation

- At least one user tool must be non-deferred. Harn errors before the
  API call is made, matching Anthropic's documented 400.
- `defer_loading` must be a bool — typos like `defer_loading: "yes"`
  error at `tool_define` time rather than silently falling back to
  the "no defer" default.

### Transcript events

Every native tool-search round-trip emits structured events in the
run record:

- `tool_search_query` — the search tool's invocation (input query,
  search-tool id).
- `tool_search_result` — the references returned by the server (which
  deferred tools got promoted on this turn).

These are stable shapes; replay / eval can reconstruct which tools
were available when without re-running the call.

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
`##DONE##` sentinel. Returns a dict with canonical visible text,
tool usage, transcript state, and any deferred queued human messages.

```harn
let result = agent_loop(
  "Write a function that sorts a list, then write tests for it.",
  "You are a senior engineer.",
  {persistent: true}
)
println(result.text)       // the accumulated output
println(result.status)     // "done", "stuck", "budget_exhausted", "idle", "watchdog", or "failed"
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
| `status` | string | Terminal state: `"done"` (natural completion), `"stuck"` (exceeded `max_nudges` consecutive text-only turns), `"budget_exhausted"` (hit `max_iterations` without any explicit break), `"idle"` (daemon yielded with no remaining wake source), `"watchdog"` (daemon idle-wait tripped the `idle_watchdog_attempts` limit), or `"failed"` (`require_successful_tools` not satisfied). |
| `text` | string | Accumulated text output from all iterations |
| `visible_text` | string | Human-visible accumulated output |
| `iterations` | int | Number of LLM round-trips |
| `duration_ms` | int | Total wall-clock time in milliseconds |
| `tools_used` | list | Names of tools that were called |
| `rejected_tools` | list | Tools rejected by policy/host ceiling |
| `deferred_user_messages` | list | Queued human messages deferred until agent yield/completion |
| `daemon_state` | string | Final daemon lifecycle state; mirrors `status` for daemon loops. |
| `daemon_snapshot_path` | string or nil | Persisted snapshot path when daemon persistence is enabled |
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
| `policy` | dict | nil | Capability ceiling applied to this agent loop |
| `daemon` | bool | `false` | Idle instead of terminating after text-only turns |
| `persist_path` | string | nil | Persist daemon snapshots to this path on idle/finalize |
| `resume_path` | string | nil | Restore daemon state from a previously persisted snapshot |
| `wake_interval_ms` | int | nil | Fixed timer wake interval for daemon loops |
| `watch_paths` | list/string | nil | Files to poll for mtime changes while idle |
| `consolidate_on_idle` | bool | `false` | Run transcript auto-compaction before persisting an idle daemon snapshot |
| `idle_watchdog_attempts` | int | nil (disabled) | Max consecutive idle-wait ticks that may return no wake reason before the daemon terminates with `status = "watchdog"`. Guards against a misconfigured daemon (e.g. bridge never signals, no timer, no watch paths) hanging the session silently |
| `context_callback` | closure | nil | Per-turn hook that can rewrite prompt-visible `messages` and/or the effective `system` prompt before the next LLM call |
| `context_filter` | closure | nil | Alias for `context_callback` |
| `post_turn_callback` | closure | nil | Hook called after each tool turn. Receives turn metadata and may inject a message, request an immediate stage stop, or both |
| `turn_policy` | dict | nil | Turn-shape policy for action stages. Supports `require_action_or_yield: bool`, `allow_done_sentinel: bool` (default `true`; set to `false` in workflow-owned action stages so nudges stop advertising the done sentinel), and `max_prose_chars: int` |
| `stop_after_successful_tools` | `list<string>` | nil | Stop after a tool-calling turn whose successful results include one of these tool names. Useful for workflow-owned verify loops such as `["edit", "scaffold"]` |
| `require_successful_tools` | `list<string>` | nil | Mark the loop `status = "failed"` unless at least one of these tool names succeeds at some point during the interaction. Keeps action stages honest when every attempted effect was rejected or errored |
| `loop_detect_warn` | int | `2` | Consecutive identical tool calls before appending a redirection hint |
| `loop_detect_block` | int | `3` | Consecutive identical tool calls before replacing the result with a hard redirect |
| `loop_detect_skip` | int | `4` | Consecutive identical tool calls before skipping execution entirely |

When `daemon: true`, the loop transitions `active -> idle -> active` instead of
terminating on a text-only turn. Idle daemons can be woken by queued human
messages, `agent/resume` bridge notifications, `wake_interval_ms`, or watched
file changes from `watch_paths`.

Default nudge message:

> You have not output ##DONE## yet — the task is not complete.
> Use your tools to continue working. Only output ##DONE## when
> the task is fully complete and verified.

When `persistent: true`, the system prompt is automatically extended with:

> IMPORTANT: You MUST keep working until the task is complete.
> Do NOT stop to explain or summarize — take action. Output ##DONE##
> only when the task is fully complete and verified.

### Context callback

`context_callback` lets you keep the full recorded transcript for replay and
debugging while showing the model a smaller or rewritten prompt-visible
history on each turn.

The callback receives one argument:

```harn
{
  iteration: int,
  system: string?,
  messages: list,
  visible_messages: list,
  recorded_messages: list,
  recent_visible_messages: list,
  recent_recorded_messages: list,
  latest_visible_user_message: string?,
  latest_visible_assistant_message: string?,
  latest_recorded_user_message: string?,
  latest_recorded_assistant_message: string?,
  latest_tool_result: string?,
  latest_recorded_tool_result: string?
}
```

It may return:

- `nil` to leave the current prompt-visible context unchanged
- a `list` of messages to use as the next prompt-visible message list
- a `dict` with optional `messages` and `system` fields

Example: hide older assistant messages so the model mostly sees user intent,
tool results, and the latest assistant turn.

```harn
fn hide_old_assistant_turns(ctx) {
  var kept = []
  var latest_assistant = nil
  for msg in ctx.visible_messages {
    if msg?.role == "assistant" {
      latest_assistant = msg
    } else {
      kept = kept + [msg]
    }
  }
  if latest_assistant != nil {
    kept = kept + [latest_assistant]
  }
  return {messages: kept}
}

let result = agent_loop(task, "You are a coding assistant.", {
  persistent: true,
  context_callback: hide_old_assistant_turns
})
```

### Post-turn callback

`post_turn_callback` runs after a tool-calling turn completes. Use it when the
workflow should react to the tool outcomes directly instead of waiting for the
model to emit another message.

The callback receives:

```harn
{
  tool_names: list,
  tool_results: list,
  successful_tool_names: list,
  tool_count: int,
  iteration: int,
  consecutive_single_tool_turns: int,
  session_tools_used: list,
  session_successful_tools: list,
}
```

Each `tool_results` entry has:

```harn
{tool_name: string, status: string, rejected: bool}
```

It may return:

- a `string` to inject as the next user-visible message
- a `bool` where `true` stops the current stage immediately after the turn
- a `dict` with optional `message` and `stop` fields

Example: stop after the first successful write turn, but still allow multiple
edits in that same turn.

```harn
fn stop_after_successful_write(turn) {
  if turn?.successful_tool_names?.contains("edit") {
    return {stop: true}
  }
  return ""
}
```

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

## Delegated workers

For long-running or parallel orchestration, Harn exposes a worker/task
lifecycle directly in the runtime.

```harn
let worker = spawn_agent({
  name: "research-pass",
  task: "Draft a summary",
  node: {
    kind: "subagent",
    mode: "llm",
    model_policy: {provider: "mock"},
    output_contract: {output_kinds: ["summary"]}
  }
})

let done = wait_agent(worker)
println(done.status)
```

`spawn_agent(...)` accepts either:

- a `graph` plus optional `artifacts` and `options`, which runs a typed
  workflow in the background, or
- a `node` plus optional `artifacts` and `transcript`, which runs a single
  delegated stage and preserves transcript continuity across `send_input(...)`

Worker configs may also include `policy` to narrow the delegated worker to a
subset of the parent's current execution ceiling, or a top-level
`tools: ["name", ...]` shorthand:

```harn
let worker = spawn_agent({
  task: "Read project files only",
  tools: ["read", "search"],
  node: {
    kind: "subagent",
    mode: "llm",
    model_policy: {provider: "mock"},
    tools: repo_tools()
  }
})
```

If neither is provided, the worker inherits the current execution policy as-is.
If either is provided, Harn intersects the requested worker scope with the
parent ceiling before the worker starts or is resumed. Permission denials are
returned to the agent loop as structured tool results:
`{error: "permission_denied", tool, reason}`.

Worker lifecycle builtins:

| Function | Description |
|---|---|
| `spawn_agent(config)` | Start a worker from a workflow graph or delegated stage |
| `send_input(handle, task)` | Re-run a completed worker with a new task, carrying transcript/artifacts forward when applicable |
| `resume_agent(id_or_snapshot_path)` | Restore a persisted worker snapshot and continue it in the current runtime |
| `wait_agent(handle_or_list)` | Wait for one worker or a list of workers to finish |
| `close_agent(handle)` | Cancel a worker and mark it terminal |
| `list_agents()` | Return summaries for all known workers in the current runtime |

Workers can persist state and child run paths between sessions. Use `carry`
inside `spawn_agent(...)` when you want continuation to reset transcript state,
drop carried artifacts, or disable workflow resume against the previous child
run record. Worker configs may also include `execution` to pin delegated work
to an explicit cwd/env overlay or a managed git worktree:

```harn
let worker = spawn_agent({
  task: "Run the repo-local verification pass",
  graph: some_graph,
  execution: {
    worktree: {
      repo: ".",
      branch: "worker/research-pass",
      cleanup: "preserve"
    }
  }
})
```

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

Transcript helpers also expose the canonical event model:

```harn
let visible = transcript_render_visible(result.transcript)
let full = transcript_render_full(result.transcript)
let events = transcript_events(result.transcript)
```

Use these when a host app needs to render human-visible chat separately from
internal execution history.

For chat/session lifecycle, `std/agents` now exposes a higher-level workflow
session contract on top of raw transcripts and run records:

```harn
import "std/agents"

let result = task_run("Write a note", some_flow, {provider: "mock"})
let session = workflow_session(result)
let forked = workflow_session_fork(session)
let archived = workflow_session_archive(forked)
let resumed = workflow_session_resume(archived)
let persisted = workflow_session_persist(result, ".harn-runs/chat.json")
let restored = workflow_session_restore(persisted.run.persisted_path)
```

Each workflow session also carries a normalized `usage` summary copied from the
underlying run record when available:

```harn
println(session?.usage?.input_tokens)
println(session?.usage?.output_tokens)
println(session?.usage?.total_duration_ms)
println(session?.usage?.call_count)
```

This is the intended host integration boundary:

- hosts persist chat tabs, titles, and durable asset files
- Harn persists transcript/run-record/session semantics
- hosts should prefer restoring a Harn session or transcript over inventing a
  parallel hidden memory format

## Workflow runtime

For multi-stage orchestration, prefer the workflow runtime over product-side
loop wiring. Define a helper that assembles the tools your agents will use:

```harn
fn review_tools() {
  var tools = tool_registry()
  tools = tool_define(tools, "read", "Read a file", {
    parameters: {path: {type: "string"}},
    returns: {type: "string"},
    handler: nil
  })
  tools = tool_define(tools, "edit", "Edit a file", {
    parameters: {path: {type: "string"}},
    returns: {type: "string"},
    handler: nil
  })
  tools = tool_define(tools, "run", "Run a command", {
    parameters: {command: {type: "string"}},
    returns: {type: "string"},
    handler: nil
  })
  return tools
}

let graph = workflow_graph({
  name: "review_and_repair",
  entry: "act",
  nodes: {
    act: {kind: "stage", mode: "agent", tools: review_tools()},
    verify: {kind: "verify", mode: "agent", tools: tool_select(review_tools(), ["run"])}
  },
  edges: [{from: "act", to: "verify"}]
})

let run = workflow_execute(
  "Fix the failing test and verify the change.",
  graph,
  [],
  {max_steps: 6}
)
```

This keeps orchestration structure, transcript policy, context policy,
artifacts, and retries inside Harn instead of product code.

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

### HuggingFace

- Endpoint: `https://router.huggingface.co/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- Use `HF_TOKEN` or `HUGGINGFACE_API_KEY`
- Same message format as OpenAI

### Ollama

- Endpoint: `<OLLAMA_HOST>/v1/chat/completions`
- Default host: `http://localhost:11434`
- No authentication required
- Same message format as OpenAI

### Local OpenAI-compatible server

- Endpoint: `<LOCAL_LLM_BASE_URL>/v1/chat/completions`
- Default host: `http://localhost:8000`
- No authentication required
- Same message format as OpenAI

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

// Pattern-matched mocks (reusable, not consumed)
llm_mock({text: "I don't know.", match: "*unknown*"})

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
