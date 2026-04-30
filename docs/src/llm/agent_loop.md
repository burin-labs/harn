# Agent loops

## agent_loop

Run an agent that keeps working until it's done. The agent maintains
conversation history across turns and loops until it emits the
completion sentinel `##DONE##`. In tagged text-tool stages the runtime
wraps it as `<done>##DONE##</done>`; in no-tool and native-tool stages
the model emits bare `##DONE##`. Returns a dict with canonical visible text,
tool usage, transcript state, and any deferred queued human messages.

```harn
let result = agent_loop(
  "Write a function that sorts a list, then write tests for it.",
  "You are a senior engineer.",
  {persistent: true}
)
println(result.text)           // the accumulated output
println(result.status)         // "done", "stuck", "budget_exhausted", "idle", "watchdog", or "failed"
println(result.llm.iterations) // number of LLM round-trips
```

### How it works

1. Sends the prompt to the model
2. Reads the response
3. If `persistent: true`:
   - Checks if the response contains the completion sentinel
     (`##DONE##`, optionally wrapped as `<done>...</done>`
     in tagged text-tool stages)
   - If yes, stops and returns the accumulated output
   - If no, sends a nudge message asking the agent to continue
   - Repeats until done or limits are hit
4. If `persistent: false` (default): returns after the first response

### agent_loop return value

`agent_loop` returns a namespaced dict. Execution metrics live under
`llm`, tool invocation data under `tools`. This shape replaces the
earlier flat layout (`iterations`, `duration_ms`, `tools_used`,
`successful_tools`, `rejected_tools`, `tool_calling_mode` were all
top-level keys before `v0.8`).

| Field | Type | Description |
|---|---|---|
| `status` | string | Terminal state: `"done"` (natural completion), `"stuck"` (exceeded `max_nudges` consecutive text-only turns), `"budget_exhausted"` (hit `max_iterations` without any explicit break), `"idle"` (daemon yielded with no remaining wake source), `"watchdog"` (daemon idle-wait tripped the `idle_watchdog_attempts` limit), or `"failed"` (`require_successful_tools` not satisfied). |
| `text` | string | Accumulated text output from all iterations |
| `visible_text` | string | Human-visible accumulated output |
| `llm` | dict | LLM execution metrics — see below |
| `tools` | dict | Tool invocation summary — see below |
| `deferred_user_messages` | list | Queued human messages deferred until agent yield/completion |
| `daemon_state` | string | Final daemon lifecycle state; mirrors `status` for daemon loops. |
| `daemon_snapshot_path` | string or nil | Persisted snapshot path when daemon persistence is enabled |
| `task_ledger` | dict | Final task-ledger state (deliverables, nudges, etc.) |
| `trace` | dict | Structured span/event summary for observability |
| `transcript` | dict | Transcript of the full conversation state |

Nested `llm` fields:

| Field | Type | Description |
|---|---|---|
| `iterations` | int | Number of LLM round-trips |
| `duration_ms` | int | Total wall-clock time in milliseconds |
| `input_tokens` | int | Sum of input tokens across LLM calls |
| `output_tokens` | int | Sum of output tokens across LLM calls |

Nested `tools` fields:

| Field | Type | Description |
|---|---|---|
| `calls` | list | Names of tools that were attempted |
| `successful` | list | Tools that returned `status: "ok"` at least once |
| `rejected` | list | Tools rejected by approval policy or capability ceiling |
| `mode` | string | Tool-calling contract used for the loop (`"native"`, `"text"`, …) |

### agent_loop options

Same as `llm_call`, plus additional options:

| Key | Type | Default | Description |
|---|---|---|---|
| `profile` | string | `"tool_using"` | Named preset for common loop shapes. One of `"tool_using"`, `"researcher"`, `"verifier"`, or `"completer"`; explicit option keys override profile defaults |
| `persistent` | bool | `false` | Keep looping until the completion sentinel is emitted (`##DONE##`, or `<done>##DONE##</done>` in tagged text-tool stages) |
| `max_iterations` | int | `50` | Maximum number of LLM round-trips |
| `max_nudges` | int | `8` | Max consecutive text-only responses before stopping |
| `nudge` | string | see below | Custom message to send when nudging the agent |
| `llm_retries` | int | `2` | Retries on transient HTTP / provider errors. Explicit option keys override profile defaults |
| `llm_backoff_ms` | int | `2000` | Base exponential backoff in ms between LLM retries |
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
| `native_tool_fallback` | string | `"allow"` | Native-tool-stage policy when the provider emits text-mode `<tool_call>` content instead of native tool calls. `"allow"` preserves the current recovery path, `"allow_once"` accepts the first fallback turn then rejects later repeats with corrective feedback, and `"reject"` fails closed on the first text fallback |
| `stop_after_successful_tools` | `list<string>` | nil | Stop after a tool-calling turn whose successful results include one of these tool names. Useful for workflow-owned verify loops such as `["edit", "scaffold"]` |
| `require_successful_tools` | `list<string>` | nil | Mark the loop `status = "failed"` unless at least one of these tool names succeeds at some point during the interaction. Keeps action stages honest when every attempted effect was rejected or errored |
| `loop_detect_warn` | int | `2` | Consecutive identical tool calls before appending a redirection hint |
| `loop_detect_block` | int | `3` | Consecutive identical tool calls before replacing the result with a hard redirect |
| `loop_detect_skip` | int | `4` | Consecutive identical tool calls before skipping execution entirely |
| `skills` | skill_registry or list | nil | Skill registry exposed to the match-and-activate lifecycle phase. See [Skills lifecycle](#skills-lifecycle) |
| `skill_match` | dict | `{strategy: "metadata", top_n: 1, sticky: true}` | Match configuration — `strategy` (`"metadata"` \| `"host"` \| `"embedding"`), `top_n`, `sticky` |
| `working_files` | list\|string | `[]` | Paths that feed `paths:` glob auto-trigger in the metadata matcher and ride along as a hint to host-delegated matchers |
| `mcp_servers` | list | nil | MCP servers to connect for this loop. Harn calls `tools/list` once per server, adds discovered tools as `<server>__<tool>`, and dispatches matching tool calls through `tools/call` |

`agent_loop` forwards `thinking`, `interleaved_thinking`, and
`anthropic_beta_features` to every model turn. For Claude Opus 4.6/4.7
agent loops, `thinking: true` is the single switch that enables extended
thinking and the Anthropic interleaved-thinking beta header.

Profiles preload the common loop-budget and retry keys below. Pass any
key explicitly to override the profile's value for that call.

| Profile | `max_iterations` | `max_nudges` | `tool_retries` | `llm_retries` | `schema_retries` |
|---|---:|---:|---:|---:|---:|
| `tool_using` | 50 | 8 | 0 | 2 | 0 |
| `researcher` | 30 | 4 | 0 | 2 | 0 |
| `verifier` | 5 | 0 | 0 | 2 | 3 |
| `completer` | 1 | 0 | 0 | 2 | 0 |

When `daemon: true`, the loop transitions `active -> idle -> active` instead of
terminating on a text-only turn. Idle daemons can be woken by queued human
messages, `agent/resume` bridge notifications, `wake_interval_ms`, or watched
file changes from `watch_paths`.

For MCP server tool catalogs, see [MCP server tools](./tools.md#mcp-server-tools).

Native-tool stages also expose structured fallback / retry metadata in the
result `trace` summary. Look for `native_text_tool_fallbacks`,
`native_text_tool_fallback_rejections`, and `empty_completion_retries` when
debugging provider contract drift or OpenAI-compatible empty completions.

Default nudge message:

> The nudge is mode-aware:
> In tagged text-tool stages it asks for concrete tool progress and reserves `<done>##DONE##</done>` for real completion.
> In no-tool or native-tool stages it asks for concrete progress and reserves bare `##DONE##` for completion.

When `persistent: true`, the system prompt is automatically extended with:

> IMPORTANT: You MUST keep working until the task is complete.
> The completion instruction is mode-aware:
> tagged text-tool stages use `<done>##DONE##</done>`, while no-tool and native-tool stages use bare `##DONE##`.

## Daemon stdlib wrappers

When you want a first-class daemon handle instead of wiring `agent_loop`
options manually, use the daemon builtins:

- `daemon_spawn(config)`
- `daemon_trigger(handle, event)`
- `daemon_snapshot(handle)`
- `daemon_stop(handle)`
- `daemon_resume(path)`

`daemon_spawn` accepts the same daemon-related options that `agent_loop`
understands (`wake_interval_ms`, `watch_paths`, `idle_watchdog_attempts`,
etc.) plus `event_queue_capacity`, which bounds the durable FIFO trigger queue
used by `daemon_trigger`.

```harn
let daemon = daemon_spawn({
  name: "reviewer",
  task: "Watch for trigger events and summarize the latest change.",
  system: "You are a careful reviewer.",
  provider: "mock",
  persist_path: ".harn/daemons/reviewer",
  event_queue_capacity: 256,
})

daemon_trigger(daemon, {kind: "file_changed", path: "src/lib.rs"})
let snap = daemon_snapshot(daemon)
println(snap.pending_event_count)
daemon_stop(daemon)
let resumed = daemon_resume(".harn/daemons/reviewer")
```

These wrappers preserve queued trigger events across stop/resume. If a daemon is
stopped while a trigger is mid-flight, that trigger is re-queued and replayed on
resume instead of being lost.

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

## Skills lifecycle

Skills bundle metadata, a system-prompt fragment, scoped tools, and
lifecycle hooks into a typed unit. Declare them with the top-level
`skill NAME { ... }` language form (see [the Harn spec](../language-spec.md))
or the imperative `skill_define(...)` builtin, then pass the resulting
`skill_registry` to `agent_loop` via the `skills:` option. The agent
loop matches, activates, and (optionally) deactivates skills across
turns automatically.

### Matching strategies

`skill_match: { strategy: ..., top_n: 1, sticky: true }` controls how
the loop picks which skill(s) to activate:

- `"metadata"` (default) — in-VM BM25-ish scoring over
  `description` + `when_to_use` combined with glob matching against
  the `paths:` list. Name-in-prompt mentions count as a strong
  boost. No host round-trip, so matching is fast and deterministic.
- `"host"` — delegates scoring to the host via the `skill/match`
  bridge RPC (see [bridge-protocol.md](../bridge-protocol.md)).
  Useful for embedding-based or LLM-driven matchers. Failing RPC
  falls back to metadata scoring with a warning.
- `"embedding"` — alias for `"host"`; accepted so the language
  matches Anthropic's canonical terminology.

### Activation lifecycle

- **Match** runs at the head of iteration 0 (always) and, when
  `sticky: false`, before every subsequent iteration (reassess).
- **Activate**: the skill's `on_activate` closure (if any) is
  called, its `prompt` body is woven into the effective system
  prompt, and `allowed_tools` narrows the tool surface for the
  next LLM call. Each activation emits
  `AgentEvent::SkillActivated` + a `skill_activated` transcript
  event with the match score and reason.
- **Deactivate** (only in `sticky: false` mode) — when reassess
  picks a different top-N, the previously-active skill's
  `on_deactivate` runs and the scoped tool filter is dropped.
  Emits `AgentEvent::SkillDeactivated` + a `skill_deactivated`
  transcript event.
- **Session resume**: when `session_id:` is set, the set of active
  skills at the end of one run is persisted in the session store.
  The next `agent_loop` call on the same session rehydrates them
  before iteration-0 matching runs, so sticky re-entry stays hot
  without re-matching from a cold prompt.

### Scoped tools

A skill's `allowed_tools` list is the union across all active
skills; any tool outside that union is filtered out of both the
contract prompt and the native tool schemas the provider sees.
Runtime-internal tools like `__harn_tool_search` are never filtered
— scoping gates the user-declared surface, not the runtime's own
scaffolding.

### Frontmatter honoured by the runtime

| Field | Type | Effect |
|---|---|---|
| `description` | string | Primary ranking signal for metadata matching |
| `when_to_use` | string | Secondary ranking signal |
| `paths` | `list<string>` | Glob patterns for `paths:` auto-trigger |
| `allowed_tools` | `list<string>` | Whitelist applied to the tool surface on activation |
| `prompt` | string | Body woven into the active-skill system-prompt block |
| `disable-model-invocation` | bool | When `true`, the matcher skips the skill entirely |
| `user-invocable` | bool | Placeholder for host UI (not consumed by the runtime today) |
| `mcp` | `list<string>` | MCP servers the skill wants booted (consumed by host integrations) |
| `on_activate` / `on_deactivate` | fn | Closures invoked on transition |

### Example

```harn,ignore
skill ship {
  description "Ship a production release"
  when_to_use "User says ship/release/deploy"
  paths ["infra/**", "Dockerfile"]
  allowed_tools ["deploy_service"]
  prompt "Follow the deploy runbook. One command at a time."
}

let result = agent_loop(
  "Ship the new release to production",
  "You are a staff deploy engineer.",
  {
    provider: "anthropic",
    tools: tools(),
    skills: ship,
    working_files: ["infra/terraform/cluster.tf"],
  }
)
```

The loop emits one `skill_matched` event per match pass (including
zero-candidate passes so replayers see the boundary), one
`skill_activated` per activated skill, and one `skill_scope_tools`
event per activation whose `allowed_tools` narrowed the surface.

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

```harn,ignore
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
| `sub_agent_run(task, options?)` | Run an isolated child agent loop and return a single clean result envelope to the parent |
| `send_input(handle, task)` | Re-run a completed worker with a new task, carrying transcript/artifacts forward when applicable |
| `resume_agent(id_or_snapshot_path)` | Restore a persisted worker snapshot and continue it in the current runtime |
| `wait_agent(handle_or_list)` | Wait for one worker or a list of workers to finish |
| `close_agent(handle)` | Cancel a worker and mark it terminal |
| `list_agents()` | Return summaries for all known workers in the current runtime |

### `sub_agent_run`

Use `sub_agent_run(...)` when you want a full child `agent_loop` with its own
session and narrowed capability scope, but you do not want the child transcript
to spill into the parent conversation history.

```harn,ignore
let result = sub_agent_run("Find the config entrypoints.", {
  provider: "mock",
  tools: repo_tools(),
  allowed_tools: ["search", "read"],
  token_budget: 1200,
  returns: {
    schema: {
      type: "object",
      properties: {
        paths: {type: "array", items: {type: "string"}}
      },
      required: ["paths"]
    }
  }
})

if result.ok {
  println(result.data.paths)
} else {
  println(result.error.category)
}
```

The parent transcript only records the outer tool call and tool result. The
child keeps its own session and transcript, linked by `session_id` / parent
lineage metadata.

`sub_agent_run(...)` returns an envelope with:

- `ok`
- `summary`
- `artifacts`
- `evidence_added`
- `tokens_used`
- `budget_exceeded`
- `session_id`
- `transcript`
- `data` when the child requests JSON mode or `returns.schema` succeeds
- `error: {category, message, tool?}` when the child fails or a narrowed tool
  policy rejects a call

`agent_loop(...)`, `sub_agent_run(...)`, and `spawn_agent(...)` also accept a
`permissions` dict for per-agent dynamic policy. `allow` and `deny` entries can
be tool-name glob lists, argument pattern lists, or Harn predicates over the tool
args. `on_escalation` receives a `PermissionRequest` and may return
`{grant: "once"}`, `{grant: "session"}`, `true`, or `false`. Permission
decisions are recorded as `PermissionGrant`, `PermissionDeny`, and
`PermissionEscalation` transcript events, while parent `policy` ceilings still
intersect with child declarations.

Set `background: true` to get a normal worker handle back instead of waiting
inline. The resulting worker uses `mode: "sub_agent"` and can be resumed with
`wait_agent(...)`, `send_input(...)`, and `close_agent(...)`.
Background handles retain the original structured `request` plus a normalized
`provenance` object, so parent pipelines can recover child questions, actions,
workflow stages, and verification steps directly from the handle/result.

Workers can persist state and child run paths between sessions. Use `carry`
inside `spawn_agent(...)` when you want continuation to reset transcript state,
drop carried artifacts, or disable workflow resume against the previous child
run record. Worker configs may also include `execution` to pin delegated work
to an explicit cwd/env overlay or a managed git worktree:

`carry.transcript_mode` is explicit and accepts:

- `inherit` (default): pass the completed worker transcript into the next
  `send_input(...)` / trigger cycle.
- `fork`: start the next cycle from a copy of the completed transcript with a
  fresh transcript id and `metadata.parent_transcript_id` pointing at the
  source transcript.
- `reset`: start the next cycle with no carried transcript.
- `compact`: compact the completed worker transcript before it is persisted and
  inherited by the next cycle.

Worker result artifacts are parent-facing summaries. Their `data.payload`
omits bulky nested `transcript` and `artifacts` fields by default while keeping
the worker request, provenance, execution profile, result text/status, and
produced artifact ids available for routing and audit.

```harn
let worker = spawn_agent({
  task: "Run the repo-local verification pass",
  graph: some_graph,
  carry: {transcript_mode: "compact", artifact_mode: "inherit"},
  execution: {
    worktree: {
      repo: ".",
      branch: "worker/research-pass",
      cleanup: "preserve"
    }
  }
})
```
