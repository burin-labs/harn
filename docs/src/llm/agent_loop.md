# Agent loops

## agent_turn

Use `agent_turn(prompt, opts?)` for the common "make one agent complete this
request" case. It wraps `agent_loop`, puts `opts.system` into the system prompt
alongside generic progress guidance, defaults to loop-until-done completion, and
requires a completion judge. Native-tool turns complete naturally when the model
returns final text with no tool calls; text/no-tool turns use the normal
sentinel path. Pass `judge: {...}` or `done_judge: {...}` to customize that
judge; omit it to use the default judge.

The return value is the normal `agent_loop` result with two extra summaries:
`iterations` (`[{iteration, started, ended?, tool_count?, prose_chars?}]`) and `judge_decisions`
(`[{iteration, verdict, reasoning, next_step, judge_duration_ms, trigger?, reason?, confirm?,
converted_from?}]`).

```harn
import { AgentLoopOptions } from "std/agent/options"

const turn_opts: AgentLoopOptions = {
  system: "Be concise and cite concrete evidence.",
  provider: "openai",
  model: "gpt-5-mini",
}
const result = agent_turn("Summarize the current project risks.", turn_opts)
log(result.visible_text)
log(result.judge_decisions[0].verdict)
```

## agent_loop

Run an agent that keeps working until it's done. The agent maintains
conversation history across turns. Native-tool loops stop naturally when the
model returns final assistant text with no tool calls; tagged text-tool loops
use the completion sentinel `<done>##DONE##</done>`, and no-tool sentinel loops
use bare `##DONE##`. Returns a dict with canonical visible text, tool usage,
transcript state, and any deferred queued human messages.

Build the options through the typed `AgentLoopOptions` alias from
`std/agent/options` (or an `agent_preset(...)` constructor). This is the
documented path: option typos surface at `harn check` time, and the
`unnormalized-options` lint flags inline dict literals that bypass it.

```harn
import { AgentLoopOptions } from "std/agent/options"

const opts: AgentLoopOptions = {loop_until_done: true}
const result = agent_loop(
  "Write a function that sorts a list, then write tests for it.",
  "You are a senior engineer.",
  opts,
)
log(result.text)           // the accumulated output
log(result.status)         // "done", "stuck", "budget_exhausted", "idle", "watchdog", or "failed"
log(result.llm.iterations) // number of LLM round-trips
```

### Choosing tool mode

Most agent loops should not set `tool_format`. Pick the provider and model you
want, pass a tool registry, and let Harn use the catalog default for that route.
The default is chosen from real agent-loop runs: native tools when they complete
cleanly, and Harn's text or JSON tool format when that provider/model is more
reliable without native tool calls.

For action stages where a tool must run, add `require_successful_tools`. That
keeps the loop honest even when a model narrates an action or gives a final
answer without actually calling the tool. When debugging a provider route, set
`llm_transcript_dir` and inspect the JSONL transcript before overriding
`tool_format`; forced overrides are best kept to probes and eval harnesses.

### How it works

1. Sends the prompt to the model
2. Reads the response
3. If `loop_until_done: true`:
   - In native-tool mode, treats final text with no tool calls as completion
   - In text-tool or no-tool sentinel mode, checks for the completion sentinel
     (`<done>##DONE##</done>` or bare `##DONE##`)
   - If completion is detected, stops and returns the accumulated output
   - If no completion is detected, sends a nudge message asking the agent to continue
   - Repeats until done or limits are hit
4. If `loop_until_done: false` (default): returns after the first response

### agent_loop return value

`agent_loop` returns a namespaced dict. Execution metrics live under
`llm`, tool invocation data under `tools`. This shape replaces the
earlier flat layout (`iterations`, `duration_ms`, `tools_used`,
`successful_tools`, `rejected_tools`, `tool_calling_mode` were all
top-level keys before `v0.8`).

| Field | Type | Description |
|---|---|---|
| `status` | string | Terminal state: `"done"` (natural completion), `"input_guardrail"` (a configured input guardrail tripped before the first main model turn), `"suspended"` (worker yielded at a cooperative suspend checkpoint), `"stuck"` (exceeded `max_nudges` consecutive text-only turns), `"budget_exhausted"` (hit `max_iterations` without any explicit break), `"verify_capped"` (a structured completion-judge veto cap was reached; see `stop_reason: "completion_judge_cap_reached"` or `"done_judge_cap_reached"`), `"provider_error"` (provider/tool-protocol request failed and was captured in `error`), `"idle"` (daemon yielded with no remaining wake source), `"watchdog"` (daemon idle-wait tripped the `idle_watchdog_attempts` limit), or `"failed"` (`require_successful_tools` not satisfied). |
| `error` | dict or nil | Structured terminal failure for provider/tool-protocol failures: `{category, reason, kind, provider, model, message, phase, tool_format, after_tool_result}`. `after_tool_result` is true when the rejected model request included prior tool observations. |
| `terminal` | dict | Producer-owned terminal classification: `{kind, reason, owner}`. `kind` distinguishes `natural`, user cancellation, policy budget/no-progress/guardrail/custom stops, provider/runtime errors, suspension, and unknown future states. ACP hosts receive the same value before `session/prompt` completes as a `typed_checkpoint` with `schema: "harn.agent_terminal.v1"`; use it instead of inferring success from ACP's coarse `stopReason`. |
| `text` | string | Accumulated text output from all iterations |
| `visible_text` | string | Human-visible accumulated output |
| `output` | any | Present only when `output_schema` is set and the loop completed (`status` `"done"`). The final answer parsed into a value that validates against `output_schema` (or, on a persistent mismatch, the best-effort parse). |
| `output_valid` | bool | Present only when `output_schema` is set and the loop completed. `true` when the terminal answer validated against `output_schema` (directly, or after the one repair re-ask); `false` when it could not be made to parse. |
| `llm` | dict | LLM execution metrics — see below |
| `tools` | dict | Tool invocation summary — see below |
| `deferred_user_messages` | list | Queued human messages deferred until agent yield/completion |
| `daemon_state` | string | Final daemon lifecycle state; mirrors `status` for daemon loops. |
| `daemon_snapshot_path` | string or nil | Persisted snapshot path when daemon persistence is enabled |
| `task_ledger` | dict | Final task-ledger state (deliverables, nudges, etc.) |
| `trace` | dict | Structured span/event summary for observability |
| `transcript` | dict | Transcript of the full conversation state |
| `handle` | dict | Present for `status: "suspended"`; resumable worker handle returned by `resume_agent(...)` |
| `reason` | string | Present for `status: "suspended"`; suspend reason visible to the resumed turn as a `system_reminder` |
| `initiator` | string | Present for `status: "suspended"`; one of `"self"`, `"parent"`, `"operator"`, or `"triggered"` |
| `conditions` | dict or nil | Present for `status: "suspended"`; optional resume trigger conditions |
| `iterations_completed` | int | Present for `status: "suspended"`; completed LLM turns before the checkpoint yielded |
| `repeated_tool_calls` | int | Present when `stall_diagnostics` is enabled. Counts adjacent repeated tool calls with identical name and arguments after the first call in each streak |
| `stall_warnings` | list | Present when `stall_diagnostics` is enabled. Diagnostic warning records emitted when a repeat streak reaches the configured threshold |
| `suspected_loop` | bool | Present when `stall_diagnostics` is enabled. `true` when at least one stall warning fired |
| `completion_judge` | dict | Present when `verify_completion_judge` is configured. `{invocations, vetoes, max_invocations, cap_reached}` — the per-session judge call/veto counts, the resolved cap (`nil` when disabled), and whether the cap was hit. Lets a harness report judge churn without transcript mining |
| `done_judge` | dict | Present when `done_judge` is configured. `{invocations, vetoes, max_invocations, cap_reached}` — the per-session done-judge call/veto counts, the resolved top-level cap (`nil` when disabled or not configured), and whether that cap was hit. This is separate from `done_judge.cadence.max_invocations`, which only gates when the judge is due |

`judge_decision` agent events carry `{verdict, confirm, reason, reasoning,
next_step, trigger}`. `verify_completion` closures, `verify_completion_judge`,
and `done_judge` all use this event, so harnesses can measure completion-gate
class fire rates from structured fields instead of parsing feedback prose.

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
| `rejected` | list | Tools rejected by approval policy, capability ceiling, handler error, or failed dispatch |
| `mode` | string | Tool-calling contract used for the loop (`"native"`, `"text"`, …) |

Every dispatched tool attempt is injected into the next model turn as a tool
result observation. Failed Harn-side handlers and blocked host-tool calls carry
their error text in that observation, so the model can recover from prior failed
attempts instead of inferring from an empty result.

### Simulated users for eval harnesses

Use `std/agent/user` when a harness needs another model, or a deterministic
fixture, to stand in for the human user. The module returns an `answerer` object
that can be wired into an agent as an `ask_user` tool, or as a post-turn callback
for agents that ask clarification questions in plain text.

```harn,ignore
import { AgentLoopOptions } from "std/agent/options"
import {
  agentic_user,
  simulated_user_read_tools,
  user_tools,
} from "std/agent/user"

const answerer = agentic_user(
  "Provide a simple prompt to create ./index.test.ts with full edge coverage.",
  "Research the codebase only if needed. Answer clarification questions with plausible user preferences. If the agent is done, stop.",
  simulated_user_read_tools(),
  "ollama:devstral-small-2",
  {max_replies: 4, max_llm_calls: 8, max_iterations: 4},
)

const opts: AgentLoopOptions = {
  provider: "openai",
  model: "gpt-5-mini",
  tools: user_tools(answerer, coding_tools),
  tool_format: "native",
  loop_until_done: true,
  max_iterations: 20,
}
const result = agent_loop(task, system, opts)
```

For deterministic eval fixtures, use `scripted_user(...)` or its alias
`fixture_user(...)`. Script entries can be strings or dicts with `match`,
`reply`, `action: "stop"`, or `action: "fail"`.

```harn,ignore
import { AgentLoopOptions } from "std/agent/options"
import { scripted_user, user_tools } from "std/agent/user"

const answerer = scripted_user([
  {match: "*test runner*", reply: "Use Vitest and cover empty, invalid, and boundary inputs."},
  {match: "*done*", action: "stop", reason: "complete"},
], {max_replies: 2})

const opts: AgentLoopOptions = {
  tools: user_tools(answerer),
  tool_format: "native",
  loop_until_done: true,
}
agent_loop(task, system, opts)
```

When the target agent does not have an explicit user-question tool, use
`simulated_user_post_turn(answerer)` as `post_turn_callback`. It watches for
plain-text clarification questions, injects a simulated reply, and stops the
loop when the answerer chooses silence.

Both `agentic_user` and `scripted_user` enforce local guardrails. `max_replies`
limits how many user messages can be produced, `max_llm_calls` caps the nested
model calls used by an agentic user, and inner `max_iterations` / `max_nudges`
bound any codebase-research loop. Simulated-user decisions also emit
`tool_call_audit` events with `audit.event_type` set to
`simulated_user_reply`, `simulated_user_stop`, `simulated_user_failed`, or
`simulated_user_budget_exhausted` so evals can audit when the harness user
intervened.

### Interactive chat loops

Use `std/agent/chat` when a harness wants an operator-typed chat loop instead
of hand-driving `agent_loop` one turn at a time. `agent_chat_loop(...)` owns the
session, calls `on_user_input(state)` before each model turn, preserves the
same `session_id` across turns, and closes the session with a typed reason
unless `close_session: false` is set.

```harn,ignore
import { agent_chat_loop, agent_chat_route_input } from "std/agent/chat"
import { read_line } from "std/io"

const result = agent_chat_loop({
  session_id: "review-chat",
  provider: "ollama",
  model: "devstral-small-2",
  tools: coding_tools,
  tool_format: "native",
  on_user_input: { state ->
    const line = read_line()
    if !line.ok {
      return {kind: "exit", reason: line.status ?? "closed"}
    }
    return agent_chat_route_input(line.value, state, {
      "/runs": { req -> {kind: "handled", message: render_recent_runs(req.state)} },
    })
  },
  on_model_turn: { turn, state -> {state: state + {last_text: turn.visible_text}} },
})
```

The chat loop adds a Harn-handled `wait_for_user` tool by default. When the
model calls it, the current `agent_loop` turn stops with
`stop_reason: "wait_for_user"` and the wrapper returns to `on_user_input`.
Pass `wait_for_user_tool: false` to keep the tool registry unchanged.

### Seeding caller-managed history

A harness that owns its own conversation history — a chat app, a replayed
transcript, a router that reconstructs prior turns from its own store — can
seed those turns into an agent loop with the `history` option. It is a list of
messages in the same canonical shape `llm_call` accepts (`{role, content, ...}`,
roles `user` / `assistant` / `tool_result` / `system`). The turns are prepended
to the transcript as real conversational turns, ahead of the fresh task message,
so the loop's first (and every subsequent) provider request presents them
exactly as `llm_call`'s `messages` array would.

```harn,ignore
const result = agent_loop(
  "What is the codeword?",
  nil,
  {
    provider: "ollama",
    model: "devstral-small-2",
    history: [
      {role: "user", content: "Remember this: the codeword is pixel."},
      {role: "assistant", content: "Understood — the codeword is pixel."},
    ],
  },
)
```

`history` is **transient seeding, not session persistence** — the caller owns
the history and passes it in on every call. Seeding is additive to the target
session's transcript, so give each call a fresh session (the default when you
omit `session_id`): reusing a `session_id` keeps the prior turns *and* seeds the
history again, double-counting the conversation. Persistence and seeding are
alternatives, not layers — pick one.
When `history` is non-empty and the task `message` is blank, the loop treats the
last history turn as the current turn and does not append an empty user message.
The seeded turns are ordinary transcript turns thereafter: `done_judge`,
compaction, and per-turn projection all treat them like any turn the loop
produced itself (compaction may summarize them once the transcript grows).

This is the middle rung of the orchestration ladder: use `llm_call` for a single
stateless request; use `agent_loop` with `history` for a tool-using chat turn
that must see prior context; reach for a workflow only when one interaction spans
more than one goal, attempt, or model. Chat-shaped harnesses that previously had
to stay on raw `llm_call`/`llm_stream_call` to keep their history can now use the
full agent loop (tools, judges, compaction) without losing the conversation.

### Streaming visible-text deltas

A chat-shaped harness that wants to render — or transform — tokens as they
arrive no longer has to abandon `agent_loop` for a raw `llm_stream_call`. Pass an
`on_delta` closure and each per-turn model call is issued through the streaming
transport; the callback fires once per streamed chunk of the assistant's
**visible text**:

```harn,ignore
agent_loop("summarize the diff", nil, {
  provider: "anthropic",
  model: "claude-sonnet-5",
  on_delta: { delta -> render_token(delta) },
})
```

Semantics:

- **Observational.** `on_delta` is a pure side-effect seam — its return value is
  ignored, and the loop's transcript is always the true concatenation of the raw
  deltas. To *mask* a stream (e.g. hide a `<secret>…</secret>` span mid-render,
  even when the tag is split across chunks), fold each delta through
  `agent_private_stream_delta` from `std/agent/stream` inside your callback; that
  transforms what you display without altering the transcript the model sees.
- **A complete turn is preserved.** The streaming call returns the same
  normalized result as `llm_call`, so native tool calls and usage survive intact
  and tool dispatch is unaffected. `on_delta` fires only for visible text — it
  never streams tool-call fragments (a deliberate v1 limitation, aligned with the
  [tool-calling north-star](../../rfcs/tool-calling-north-star.md) dialect
  phases).
- **Graceful non-streaming fallback.** When a provider returns a complete
  response without incremental deltas (the mock provider, cached results, or a
  transport that does not stream), `on_delta` still fires exactly once with the
  full visible text, so harness code sees a uniform "at least one delta, and the
  concatenation equals the visible text" contract. `provider_capabilities(...)`
  reports `requires_streaming` for models that must stream.
- **Attempts are observable.** Schema retries, routing failover, and
  context-overflow reissues can each start a fresh provider call. `on_delta`
  reports visible text from every attempted call in order; callers that render a
  single final transcript should treat the callback as live progress, not as the
  authoritative persisted assistant message.
- **Composes with `llm_caller`.** `on_delta` only affects the *default* per-turn
  caller. A custom `llm_caller` short-circuits before the streaming path, so a
  caller that does not itself stream simply never fires `on_delta`.

### agent_loop options

The typed shape of this surface is `AgentLoopOptions` from
`std/agent/options` — every `llm_call` option plus the loop-control
keys below. Annotate a binding
(`let opts: AgentLoopOptions = {...}`) or build the dict via
`agent_preset(...)` / `agent_options(...)`; inline dict literals still
execute but are flagged by the `unnormalized-options` lint.

Same as `llm_call`, plus additional options:

| Key | Type | Default | Description |
|---|---|---|---|
| `profile` | string | `"tool_using"` | Named preset for common loop shapes. One of `"tool_using"`, `"researcher"`, `"verifier"`, or `"completer"`; explicit option keys override profile defaults |
| `history` | list | nil | Caller-managed conversation history to seed. A list of messages in the canonical `llm_call` shape (`{role, content, ...}`, roles `user`/`assistant`/`tool_result`/`system`) prepended to the transcript as real turns ahead of the task message, so the first LLM call sees them exactly as `llm_call`'s `messages` array would. Transient seeding, not session persistence — the caller owns the history. When `history` is non-empty and the task `message` is blank, no empty user turn is appended. See [Seeding caller-managed history](#seeding-caller-managed-history) |
| `loop_until_done` | bool | `false` | Keep looping until completion. Native-tool loops complete on final text with no tool calls; text-tool/no-tool sentinel loops complete on `##DONE##` or `<done>##DONE##</done>` |
| `done_sentinel` | string\|nil | mode-aware | Completion sentinel for sentinel-based loops. Use a non-empty string such as `"##DONE##"` to require sentinel completion, or `nil` for no sentinel. Native-tool loop-until-done loops default to `nil`; text/no-tool loop-until-done loops default to `"##DONE##"` |
| `output_schema` | dict | nil | JSON Schema the loop's **final answer** must satisfy. This is a terminal-answer contract, not a per-turn one: the loop does not force every mid-loop turn into structured mode (which would fight tool-calling). At finalize (only on a `"done"` completion) it validates the terminal answer; if off-shape it re-asks **once** through the `llm_caller` seam for a schema-shaped value. The parsed value is surfaced on `run.output`, and `run.output_valid` reports whether validation passed. For strict one-shot structured extraction with no agent loop, use `llm_call_structured` instead. |
| `max_iterations` | int | `50` | Maximum number of LLM round-trips. Equivalent to `iteration_budget: {mode: "fixed", initial: N, max: N}` |
| `iteration_budget` | string\|dict | nil | Adaptive or fixed iteration cap. Pass a dict `{mode, initial, max, extend_by}` or the string `"adaptive"` / `"fixed"`. See [Adaptive iteration budget](#adaptive-iteration-budget) |
| `loop_control` | closure | nil | Per-iteration policy callback `state -> command`. Receives a normalized loop-state snapshot and returns a command (`extend`/`stop`/`none`). See [Adaptive iteration budget](#adaptive-iteration-budget) |
| `max_nudges` | int | `8` | Max consecutive text-only responses before stopping |
| `nudge` | string | see below | Custom message to send when nudging the agent |
| `llm_caller` | closure | nil | Custom caller wrapping the per-turn `llm_call`. The resilience surface: compose `with_retry` / `with_fallback` from `std/llm/handlers` here. See [Composable callers and middleware](../stdlib/llm-handlers.md). |
| `on_delta` | closure | nil | Observational streaming callback `delta -> nil`, invoked once per streamed chunk of the assistant's visible text during each turn. Lets chat-shaped harnesses render or transform the token stream without leaving `agent_loop`. See [Streaming visible-text deltas](#streaming-visible-text-deltas). |
| `reasoning_policy` / `thinking_policy` | string/bool | `"auto"` | Provider-aware reasoning policy. `auto` chooses a task/scale-appropriate setting; `off` disables thinking where possible and otherwise uses the provider's lowest reasoning floor; `minimal`, `low`, `medium`, `high`, `xhigh`, and `max` request explicit levels. Caller-supplied `thinking` or `reasoning_effort` always wins |
| `reasoning_scale` / `problem_scale` | string | `"medium"` | Scale hint for `reasoning_policy: "auto"`: `small`, `medium`, or `large` |
| `reasoning_task` | string | inferred | Task hint for `reasoning_policy: "auto"`: `chat`, `agent`, `code`, `verify`, or `summarize` |
| `tool_retries` | int | `0` | Number of retry attempts for failed tool calls |
| `tool_backoff_ms` | int | `1000` | Base backoff delay in ms for tool retries (doubles each attempt) |
| `max_concurrent_tools` | int | `1` | Maximum in-flight tool calls from one planner turn. Results are recorded in emitted order even when calls complete out of order |
| `intra_turn_resource_fail_fast` | bool | `true` | When an annotated mutating tool call fails, skip later sibling calls in the same assistant response that target the same declared path resource. Set `false` only for legacy dispatch-all behavior |
| `prefetch_next_turn` | bool | `false` | Start the next planner turn after tool results are recorded while local/custom audit receipt sinks flush in the background. The loop drains those flushes before returning |
| `tool_surface_narrowing` | bool/dict | `{enabled: true, window_turns: 5, mode: "safe"}` | Between turns, remove model-visible tools that were unused across the rolling window. Safe mode narrows unused `read_only` tools while keeping mutating/control/unknown tools by class; dict configs may also set `mode: "aggressive"`, `hard_keep`, `prune_classes`, `keep_classes`, and `unknown_tool_policy` |
| `progress_tool` | bool/dict | `false` | Opt in to a model-facing progress tool that emits `progress_reported` agent events. `true` exposes `agent_progress`; a dict may set `name`, `description`, and `system_prompt_nudge`. ACP clients receive task-list entries as canonical `plan` updates and message-only reports as Harn `progress` narration |
| `policy` | dict | nil | Capability ceiling applied to this agent loop |
| `daemon` | bool | `false` | Idle instead of terminating after text-only turns |
| `persist_path` | string | nil | Persist daemon snapshots to this path on idle/finalize |
| `resume_path` | string | nil | Restore daemon state from a previously persisted snapshot |
| `wake_interval_ms` | int | nil | Fixed timer wake interval for daemon loops |
| `watch_paths` | list/string | nil | Files to poll for mtime changes while idle |
| `consolidate_on_idle` | bool | `false` | Run transcript auto-compaction before persisting an idle daemon snapshot |
| `compaction` | string/dict/bool | `{strategy: "hybrid", keep_last_n: 10}` | Agent-loop context-window policy. Use `"none"` or `false` to disable; `"truncate"`, `"summarize_middle"`, `"summarize_all"`, or `"hybrid"` to choose policy. Dict policies may include `policy` / `compaction_policy` with compaction instructions |
| `compact_threshold` | int | model-aware | Estimated input-token threshold for compaction. Harn lowers this from the provider/model context window when known |
| `compact_keep_first` | int | `0` | Prompt-visible messages to keep verbatim before the compaction summary. The system prompt is always kept separately |
| `compact_keep_last` | int | strategy default | Prompt-visible messages to keep verbatim after the compaction summary |
| `auto_compact` | bool/dict | nil | Auto-compaction options. Dict values may include the same compaction policy fields as `compaction` |
| `transcript_projection` | string/dict | nil | Per-turn model-visible projection over the immutable raw transcript. Policies include `clean_tool_repair`, `squash_failed_calls`, `summary_prefix`, `reachability_gc`, and `custom` |
| `scratchpad` | bool/dict | `false` | Session-local working memory. `true` initializes a compact `{goals, open_items, facts, refs}` scratchpad, recites it at the prompt tail each turn, and periodically reorganizes it. Dict configs may set `enabled`, `recite`, `reorganize_every`, `max_recent_messages`, `schema_retries`, `initial`, and `reorganizer` |
| `idle_watchdog_attempts` | int | nil (disabled) | Max consecutive idle-wait ticks that may return no wake reason before the daemon terminates with `status = "watchdog"`. Guards against a misconfigured daemon (e.g. bridge never signals, no timer, no watch paths) hanging the session silently |
| `context_callback` | closure | nil | Per-turn hook that can rewrite prompt-visible `messages` and/or the effective `system` prompt before the next LLM call |
| `context_filter` | closure | nil | Alias for `context_callback` |
| `timestamp_messages` | bool | `false` | Decorate prompt-visible transcript messages with the current harness timestamp before each LLM call without mutating the stored transcript |
| `message_decorator` | closure | nil | Per-message hook called as `message_decorator(message, context)` before each LLM call. The context includes `session_id`, `iteration`, `index`, and `timestamp` |
| `prompts` / `prompt_overrides` | dict | nil | Override validated logical agent prompt ids such as `agent.loop_contract`, `agent.tool_contract_text`, and `agent.completion_judge_system` with a prompt asset path, `{text}`, `{path}`, or render closure. Unknown ids are rejected. For typo-resistant authoring, pass the typed override shape through `agent_prompt_overrides(...)` from `std/agent/prompts` |
| `post_turn_callback` | closure | nil | Hook called after each turn. Receives turn metadata and may inject a message, request an immediate stage stop, or merge next-turn options such as `llm_options: {tool_choice: "none"}`. Rich verdicts may include `feedback_kind: string` to give injected feedback a stable semantic identity |
| `verify_completion` | closure | nil | Hook called when the loop is about to stop naturally. Return `nil`/`true` to accept the stop or feedback text to veto and continue |
| `verify_completion_judge` | bool/dict | nil | Built-in structured judge for any natural stop. `true` uses defaults; a dict may set `provider`, `model`, `system`, `feedback_fallback`, and `max_invocations` (alias `max_feedback`, default `5`) to cap repeated vetoes. Once the cap is hit the judge stops firing and the loop ends with status `verify_capped` and stop_reason `completion_judge_cap_reached`; set `max_invocations: 0` to disable the cap |
| `done_judge` | bool/dict | nil | Completion structured judge. It runs when the model naturally completes a native-tool loop or emits the done sentinel, and may veto by returning `verdict: "continue"` plus `next_step` or `reasoning`. Dict configs may include `max_invocations` (alias `max_feedback`) to terminally cap repeated vetoes, plus `cadence: {every?, when?, max_invocations?, min_iterations_before_first?}` to control when the judge is due |
| `step_judge` | dict | nil | Per-turn structured judge that runs after an assistant turn and before tool dispatch. Dict configs may include `provider`, `model`, `on_veto` (`"replace"` or `"retain"`), `max_attempts`, `skip_when_empty`, `skip_when_stalled`, and `skip_when_iterations_remaining` (default `1`, skips when no regeneration turn remains). Skips emit `step_judge_decision` with `skipped: true` |
| `input_guardrail` | closure | nil | Pre-loop guardrail closure. It runs before the first main model turn with `{session_id, task, user_message, messages, recent_context, provider, model}` and returns `{tripwire, reason, label?, confidence?}`. A tripwire emits `input_guardrail_verdict` and stops as `status: "input_guardrail"` / `stop_reason: "input_guardrail_tripwire"` without spending the main loop turn. Build the closure with `agent_input_guardrail(...)` from `std/agent/guardrails` |
| `llm_caller_transport` | dict | nil | Explicit guarantees for a custom `llm_caller`. `{forwards_assistant_prefill: true}` permits one-shot assistant-prefill recoveries only when the caller forwards that request option unchanged; absence stays fail-closed. Provider capability and multi-route safety gates still apply. |
| `llm_transcript_dir` | string | nil | Per-loop directory for Harn's existing `llm_transcript.jsonl` sidecar. This is equivalent to scoping `HARN_LLM_TRANSCRIPT_DIR` to one agent loop and is preferred when a script needs run-specific auditable model-turn JSONL |
| `turn_policy` | dict | nil | Turn-shape policy for action stages. Supports `require_action_or_yield: bool`, `allow_done_sentinel: bool` (default `true`; set to `false` in workflow-owned action stages so nudges stop advertising the done sentinel), and `max_prose_chars: int` |
| `native_tool_fallback` | string | `"allow"` | Native-tool-stage policy when the provider emits text-mode `<tool_call>` content instead of native tool calls. `"allow"` preserves the current recovery path, `"allow_once"` accepts the first fallback turn then rejects later repeats with corrective feedback, and `"reject"` fails closed on the first text fallback |
| `stop_after_successful_tools` | `list<string>` | nil | Stop after a tool-calling turn whose successful results include one of these tool names. Useful for workflow-owned verify loops such as `["edit", "scaffold"]` |
| `require_successful_tools` | `list<string\|list<string>>` | nil | Mark a cleanly completed loop `status = "failed"` unless every required tool succeeds at least once. A nested list is an OR group, e.g. `["run_command", ["read_command_output", "read_command_output_tail"]]`. Keeps action stages honest when attempted effects were rejected, errored, or skipped |
| `stall_diagnostics` | bool/dict | nil | Detect adjacent repeated tool calls with the same name and identical arguments. `true` enables conservative defaults (`threshold: 3`, one feedback nudge, argument digests only). Dict options include `enabled`, `threshold`, `inject_feedback`, `max_feedback`, `exempt_tools`/`allow_repeated_tools`, `include_arguments`, and repair knobs either flat or under `repair_diagnostics` |
| `skills` | skill_registry or list | nil | Skill registry exposed to the match-and-activate lifecycle phase. See [Skills lifecycle](#skills-lifecycle) |
| `skill_match` | dict | `{strategy: "metadata", top_n: 1, sticky: true}` | Match configuration — `strategy` (`"metadata"` \| `"host"` \| `"embedding"`), `top_n`, `sticky` |
| `working_files` | list\|string | `[]` | Paths that feed `paths:` glob auto-trigger in the metadata matcher and ride along as a hint to host-delegated matchers |
| `mcp_servers` | list | nil | MCP servers to connect for this loop. Harn calls `tools/list` once per server, adds discovered tools as `<server>__<tool>`, and dispatches matching tool calls through `tools/call` |

`agent_loop` forwards `thinking`, `reasoning_effort`, `interleaved_thinking`,
and `anthropic_beta_features` to every model turn. When neither `thinking` nor
`reasoning_effort` is set, `reasoning_policy: "auto"` lowers provider quirks
into explicit typed thinking options before the call reaches the provider. For
example, OpenAI reasoning models get `thinking: {mode: "effort"}` (`off`
becomes `none` on newer GPT-5 routes that advertise it, otherwise `minimal`),
Gemini 2.5 gets native `generationConfig.thinkingConfig`, Together hybrid
models get `reasoning.enabled`, and Qwen-style local providers can use
`thinking: {mode: "disabled"}` to trigger Harn's `/no_think` injection.
For local Qwen routes (`ollama`, `llamacpp`, `local`, and `mlx`), `auto`
keeps small/medium tasks at this disabled floor because those chat templates
are more reliable on compact edit loops without forced thinking.
For Claude Opus 4.6/4.7 agent loops, `thinking: true` remains the
single explicit switch that enables extended thinking and the Anthropic
interleaved-thinking beta header.

ACP clients can pin the same abstraction for a session with
`session/set_config_option(configId="thought_level")`. Agent loops running in
that session inherit the pin unless their options explicitly set
`reasoning_policy`, `thinking_policy`, `thinking`, or `reasoning_effort`.

Profiles preload the common loop-budget and retry keys below. Pass any
key explicitly to override the profile's value for that call.

### Agent scratchpad

`agent_loop(..., {scratchpad: true})` creates a small structured scratchpad for
the session, renders it as a tail system fragment on every turn, and runs a
structured reorganization pass every three continuing turns. The reorganizer may
use a different provider or model:

```harn
import { AgentLoopOptions } from "std/agent/options"

const scratchpad_opts: AgentLoopOptions = {
  loop_until_done: true,
  scratchpad: {
    reorganize_every: 2,
    reorganizer: {provider: "ollama", model: "devstral-small-2"},
  },
}
agent_loop(task, system, scratchpad_opts)
```

The scratchpad is capped at 16 KiB and is stored as live session state, not as a
synthetic replay message. Updates append compact `agent_scratchpad` transcript
events with action/version/count metadata; session snapshots and final
transcripts expose `scratchpad`, `scratchpad_version`, and
`metadata.agent_scratchpad`.
When paired with `transcript_projection: {policy: "reachability_gc"}`, the loop
automatically supplies the current scratchpad as a GC root and
scratchpad-version write barrier for each provider turn. Referenced tool output
stays visible; stale, unreferenced tool-result bodies can be reclaimed from the
model-visible prefix while the raw transcript remains intact.

Scripts can read and write the state directly with
`agent_session_scratchpad(id)`, `agent_session_set_scratchpad(id, pad, opts?)`,
and `agent_session_clear_scratchpad(id, opts?)`. Reorganization validates that
returned facts cite source refs already present in the scratchpad or recent
turns, so heavy tool output remains referenced rather than copied.

The deterministic regression/eval harness for the none vs append-only vs
periodic-reorg comparison is:

```sh
cargo run --quiet --bin harn -- run examples/evals/agent_scratchpad_retention.harn
```

### Resilience knobs

The preferred surface for retry / fallback / shadow / budget / cache /
circuit-breaker behavior on `agent_loop` is `llm_caller:`. Pass a
closure that wraps the per-turn `llm_call(...)` and the loop will
route every turn through it:

```harn,ignore
import { AgentLoopOptions } from "std/agent/options"
import {default_llm_caller, with_retry, with_fallback, compose} from "std/llm/handlers"

const caller = compose([
  with_retry({max_attempts: 4, backoff: "exponential"}),
])(default_llm_caller())

const opts: AgentLoopOptions = {
  loop_until_done: true,
  llm_caller: caller,
}
const result = agent_loop(task, system, opts)
```

Caller contract: `fn(call) -> {ok, value | status, error?}` where
`call = {prompt, system, opts, turn: {iteration, session_id, attempt}}`.
The pre-0.10 `llm_retries` / `llm_backoff_ms` options were removed —
the loop is fail-fast on transient provider errors unless a composed
`llm_caller` retries them; the `deprecated_llm_options` lint hard-errors
on usage (see [Migrating to 0.10](../migrations/v0.10.md)). See
[Composable callers and middleware](../stdlib/llm-handlers.md) for the
full middleware catalog.

### Agent-loop compaction

`agent_loop` compacts prompt-visible transcript messages before an LLM call
would exceed the configured or discovered context budget. The default policy is
`hybrid`: keep the system prompt and the last 10 prompt-visible messages
verbatim, summarize older messages, and fall back to truncation if the summary
still exceeds the hard limit.

```harn
import { AgentLoopOptions } from "std/agent/options"

const compaction_opts: AgentLoopOptions = {
  provider: "openai",
  model: "gpt-4o",
  compaction: {strategy: "hybrid", keep_last_n: 10},
}
const result = agent_loop(task, system, compaction_opts)
```

Available strategies:

| Strategy | Behavior |
|---|---|
| `"none"` / `false` | Disable automatic agent-loop transcript compaction |
| `"truncate"` | Replace older messages with a deterministic abbreviated summary |
| `"summarize_middle"` | Summarize older messages and keep the latest suffix verbatim |
| `"summarize_all"` | Summarize all compactable prompt-visible messages |
| `"hybrid"` | Summarize older messages, keep the latest suffix, and use truncate as the hard-limit fallback |

Compaction emits `TranscriptCompacted` live events and transcript
`compaction` events with `reason`, `strategy`, `engine_strategy`,
`estimated_tokens_before`, `estimated_tokens_after`, `instruction_mode`,
`instruction_source`, and `compaction_policy`, so replay tools can verify which
trigger, policy, and host/user instruction lane ran.

Hosts can attach first-class compaction instructions without building custom
prompt concatenation. The typed `CompactionPolicy` shape accepts
`instructions`, `mode`, `scope`, `preserve`, `drop`,
`extend_default_instructions`, and `author`. Omitting
`extend_default_instructions` or setting it to `true` appends host/user guidance
after Harn's default compaction rules; `false` replaces the default guidance.
Host-only instructions stay in event and audit metadata and are not copied into
the next model-visible summary unless `scope` is `"model_visible"`,
`"summary"`, or `"transcript"`.

```harn
import {compact_for_bug_fix_resumption} from "std/agent/autocompact"
import { AgentLoopOptions } from "std/agent/options"

const auto_compact_opts: AgentLoopOptions = {
  provider: "mock",
  compact_threshold: 1,
  compact_strategy: "custom",
  auto_compact: {
    policy: compact_for_bug_fix_resumption({author: "host"})
  },
  compact_callback: { archived, _reminders, policy ->
    {summary: "resume with " + policy.mode + " over " + to_string(len(archived)) + " messages"}
  },
}
const result = agent_loop(task, system, auto_compact_opts)
```

Stdlib helpers cover common host commands:
`compaction_policy(...)`, `compact_for_bug_fix_resumption(...)`,
`compact_preserving_test_failures(...)`, and
`compact_retaining_current_plan(...)`.

| Profile | `max_iterations` | `max_nudges` | `tool_retries` | `schema_retries` |
|---|---:|---:|---:|---:|
| `tool_using` | 50 | 8 | 0 | 0 |
| `researcher` | 30 | 4 | 0 | 0 |
| `verifier` | 5 | 0 | 0 | 3 |
| `completer` | 1 | 0 | 0 | 0 |

### Adaptive iteration budget

Plain integer `max_iterations` is a hard cap. `iteration_budget` lets the loop
start with a small initial limit and extend it transparently when there is
evidence of forward progress, instead of forcing harness authors to guess a
single number.

```harn
import { AgentLoopOptions, IterationBudget } from "std/agent/options"

const budget: IterationBudget = {mode: "adaptive", initial: 4, max: 16, extend_by: 2}
const budget_opts: AgentLoopOptions = {iteration_budget: budget}
const result = agent_loop(prompt, system, budget_opts)
```

Fields:

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | string | `"fixed"` | `"fixed"` (no extension) or `"adaptive"` |
| `initial` | int | `max / 4` (adaptive), `max` (fixed) | Iteration cap to start with |
| `max` | int | `16` (adaptive), `50` (fixed) | Hard upper bound; extensions never raise the cap above this |
| `extend_by` | int | `2` (adaptive), `0` (fixed) | Default extension delta when policy returns `{action: "extend"}` without `by` / `until` |
| `expose_decisions` | bool | `mode == "adaptive"` | When true, the result includes an `adaptive_budget` summary with the decision log |

`max_iterations: N` and `iteration_budget: {mode: "fixed", initial: N, max: N}`
are equivalent. Passing both `iteration_budget` and `max_iterations` is allowed;
the budget's `max` wins for the host's autonomy/ACP tracking.
Explicit `max_iterations`, `initial`, `max`, and adaptive `extend_by` values
must be positive integers, and `initial` must be less than or equal to `max`;
invalid fields raise an `agent_loop` error before the first provider call.

### loop_control policy

When the budget is `"adaptive"` (or any time you set `loop_control`), the loop
calls a policy closure once per iteration with a normalized state snapshot and
applies the returned command:

```harn,ignore
loop_control: { state ->
  if state.budget.remaining > 1 {
    return nil
  }
  if state.completion.vetoed {
    return {action: "extend", by: 2, reason: "completion gate vetoed"}
  }
  if state.turn.tool_call_count > 0 && state.progress.changed {
    return {action: "extend", by: 2, reason: "recent turn made progress"}
  }
  return nil
}
```

State snapshot fields:

| Field | Description |
|---|---|
| `iteration` | 1-based iteration just completed |
| `budget.current_limit` | Active iteration cap before this decision |
| `budget.max` | Configured upper bound |
| `budget.remaining` | `current_limit - iteration`; `0` means the next iteration would exceed the cap |
| `budget.extension_count` | Number of prior extensions applied |
| `turn.tool_call_count` | Tool calls executed this turn |
| `turn.tool_names` | Attempted tool names from this turn's dispatch |
| `turn.successful_tool_names` / `turn.rejected_tool_names` | Names from this turn's dispatch |
| `turn.text_chars` | Visible-text length this turn |
| `turn.native_fallback_used` | True when `native_tool_fallback` accepted text-mode tool calls this turn |
| `session.successful_tool_names` / `session.rejected_tool_names` | Cumulative deduplicated tool name sets |
| `session.required_tools_satisfied` / `session.required_tools_missing` | `require_successful_tools` postcondition status |
| `completion.proposed` | True when post-turn logic proposed a natural / sentinel break this turn |
| `completion.vetoed` | True when `verify_completion` / `verify_completion_judge` / `done_judge` vetoed |
| `completion.verdict` / `completion.feedback` | Judge verdict and feedback string when present |
| `progress.changed` | True if this turn made tool calls, produced new successful tool names, or wrote visible text |
| `progress.summary` | Human-readable progress hint (`"executed N tool call(s)"`, `"completion gate vetoed"`, etc.) |

Return value is one of:

| Command | Meaning |
|---|---|
| `nil` / `{action: "none"}` | No-op; loop continues until the next decision |
| `{action: "extend", by: N, reason}` | Raise `current_limit` by `N` (capped by `budget.max`) |
| `{action: "extend", until: M, reason}` | Raise `current_limit` to `M` (capped by `budget.max`) |
| `{action: "stop", status: "incomplete", reason}` | Break the loop with the given final status |

When no `loop_control` is provided and the budget is adaptive, the stdlib
installs a small default policy that extends only when one of these is true at
the cap edge:

- the latest verify/done judge vetoed completion,
- `require_successful_tools` is unsatisfied, or
- the most recent turn executed at least one tool call (i.e. real progress).

All decisions are recorded:

- in the result under `adaptive_budget.decisions` (when `expose_decisions` is
  true, which is the default for adaptive budgets), and
- as `LoopControlDecision` events on the live event stream
  (`{type: "loop_control_decision", action, oldLimit, newLimit, reason, ...}`),
  also surfaced to ACP/A2A bridges.

```harn
import { AgentLoopOptions } from "std/agent/options"

const adaptive_opts: AgentLoopOptions = {iteration_budget: {mode: "adaptive", initial: 4, max: 12}}
const result = agent_loop(prompt, system, adaptive_opts)
log(result.adaptive_budget.extensions_used)
log(result.adaptive_budget.final_limit)
for decision in result.adaptive_budget.decisions {
  log(decision.action + ": " + decision.reason)
}
```

### Presets: how you build agent_loop options

`agent_preset(kind, options?)` from `std/agent/presets` is how you build
`agent_loop` options — not a separate tier, just the constructor for the
agent-cell option dict. It packages the common harness shapes — audit,
repair, summary, verify, and the four captains — so script authors don't
hand-tune `max_iterations`, `max_nudges`, `done_sentinel`, `done_judge`,
`turn_policy`, provider/timeout/budget defaults, and transport retry on every
call. The returned value is an ordinary options dict (caller overrides always
win) that you pass to `agent_loop` directly.

> **llm-tier doctrine**: there is deliberately no preset machinery at the
> `llm_call` tier. An "llm preset" is just a plain typed `LlmCallOptions`
> value (`llm_options({...})`) you spread per call. Budgets, completion
> policy, middleware stacks, and transport retry belong to the agent cell —
> `agent_preset` — only.

Every preset kind layers three things under your explicit input:

1. **Behavior template** — profile, iteration budget, turn policy, reasoning
   defaults (and, for captains, the opt-in middleware layers below).
2. **Fill-nil pack rows** — per-kind defaults for `provider`, `timeout_ms`,
   `budget` (session-cumulative `total_budget_usd`), and `model_ladder`.
   Pack rows fill **only** nil/absent keys at one lower-priority seam; they
   never override explicit caller input. Defaults are data rows, not code
   branches. The model route rows (`provider`, `model`, `models`, `ladder`,
   `model_ladder`, `routing`, and related routing-policy keys) are treated as
   one ownership group: if you provide any route at top level or under
   `llm_options`, the preset does not add a different built-in provider or
   ladder. Supplying both top-level `provider` and `model` without a ladder
   produces a single-step `model_ladder` in the returned options for downstream
   policy code to inspect.
3. **Default transport retry** — v0.10 removed the per-call `llm_retries`
   budget, making a bare `agent_loop` fail-fast on transient transport
   errors. Presets bake bounded resilience back in by wrapping the effective
   `llm_caller:` (yours, the captain-composed router, or the stdlib default)
   with `with_retry` from `std/llm/handlers` (default `max_attempts: 3`,
   exponential backoff). The default predicate retries transport-class
   failures only (transient / rate-limited / timeout / network / 5xx /
   stream interrupt) and never schema-validation, auth, budget,
   context-window, or policy failures. Opt out with `retry: false`; tune
   with `retry: {max_attempts, base_ms, ...}` (any `with_retry` config).

Kinds live in a registry: `agent_preset_kinds()` lists them, and
`agent_preset_register(kind, {family?, pack?})` adds your own — user-defined
kinds are first-class and go through the same spec validation the built-ins
are registered with. `family` is `"generic"` or `"captain"` (captains compose
the middleware layers below); `pack` carries the fill-nil rows.

```harn
import {agent_preset, agent_preset_register} from "std/agent/presets"

agent_preset_register("triage", {
  family: "captain",
  pack: {provider: "openai", timeout_ms: 90000, budget: {total_budget_usd: 5.0}},
})
const opts = agent_preset("triage", {tools: triage_tools})
const run = agent_loop("Triage the queue.", opts?.system, opts)
```

```harn
import {agent_preset} from "std/agent/presets"

// Inspect / read-only audit. Native completion, no done sentinel,
// adaptive budget {initial: 4, max: 12}, max_nudges: 1.
const audit_opts = agent_preset("audit", {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: release_tools,
  require_successful_tools: ["release_run"],
})
const audit = agent_loop("Audit the release", audit_opts?.system, audit_opts)

// Tool-using repair. Wider budget {initial: 4, max: 16}, max_nudges: 2.
// Customize before passing to agent_loop:
const opts = agent_preset("repair", {
  tools: repair_tools,
  iteration_budget: {mode: "adaptive", initial: 6, max: 20},
})
const result = agent_loop(prompt, system, opts)

// Cheap one-shot summary. tool_choice="none", iteration_budget fixed at 1.
const summary_opts = agent_preset("summary", {provider: "openai", model: "gpt-4o-mini"})
const summary = agent_loop("Summarize the audit findings.", nil, summary_opts)

// Local/configured route. The audit preset keeps its audit behavior,
// budget, timeout, and retry defaults, but it does not mix in the built-in
// Anthropic provider or frontier ladder once the route is supplied.
const local_opts = agent_preset("audit", {
  llm_options: {provider: "llamacpp", model: "gemma4-local"},
  tools: release_tools,
})
const local_audit = agent_loop("Audit with the configured local model.", local_opts?.system, local_opts)
```

Preset roles, defaults summarized:

| Preset | `profile` | `tool_format` | `loop_until_done` | `max_nudges` | Default `iteration_budget` | `stall_diagnostics` | `done_sentinel` / `done_judge` |
|---|---|---|---|---:|---|---|---|
| `audit` | `verifier` | `native` | true | 1 | adaptive `{initial: 4, max: 12, extend_by: 2}` | enabled, threshold 3 | both `nil` (natural completion) |
| `repair` | `tool_using` | `native` | true | 2 | adaptive `{initial: 4, max: 16, extend_by: 2}` | enabled, threshold 3 | both `nil` |
| `summary` | `completer` | unset | false | 0 | fixed `{initial: 1, max: 1}` | unset | both `nil`, `tool_choice: "none"` |
| `verify` | `verifier` | unset | false | 0 | adaptive `{initial: 1, max: 5, extend_by: 1}` | unset | `done_judge: true` |
| `merge_captain` | `tool_using` | `native` | true | 3 | adaptive `{initial: 8, max: 60, extend_by: 4}` | enabled, threshold 3 | both `nil`; default consent denies writes |
| `review_captain` | `tool_using` | `native` | true | 3 | adaptive `{initial: 6, max: 30, extend_by: 3}` | enabled, threshold 3 | both `nil` |
| `oncall_captain` | `tool_using` | `native` | true | 3 | adaptive `{initial: 6, max: 24, extend_by: 3}` | enabled, threshold 3 | both `nil`; default `with_rate_limit({max_calls: 50})` |
| `release_captain` | `tool_using` | `native` | true | 3 | adaptive `{initial: 8, max: 40, extend_by: 4}` | enabled, threshold 3 | both `nil`; opt-in `with_dry_run` shadow runs |

### Captain presets

The four captain presets package the persona-shaped service contracts
adopters were re-deriving by hand: a long-enough adaptive budget, a
HITL-friendly consent gate where it matters, a default rate-limit
where unbounded fan-out would hurt, and the canonical cheap-default /
frontier-escalation routing scaffolding from `std/llm/handlers`. They
are the substrate the persona template pack (harn#463) ships entries
on top of.

```harn
import {agent_preset} from "std/agent/presets"

// Merge Captain: long adaptive budget; default consent layer auto-
// approves tools annotated `read`/`search`/`fetch`/`think` and denies
// everything else unless the caller passes a `consent` callable.
const sweep_opts = agent_preset("merge_captain", {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: github_tools,
  consent: { call -> approval_bridge.prompt(call) },     // HITL bridge
  audit_sink: { record -> receipts.append(record) },     // captain ledger
})
const sweep = agent_loop("Sweep open PRs.", sweep_opts?.system, sweep_opts)

// Oncall Captain: defaults `with_rate_limit({max_calls: 50})` so an
// alert-storm loop can't fan out unbounded. Override via `rate_limit`.
const triage_opts = agent_preset("oncall_captain", {
  provider: "openai",
  model: "gpt-5.4",
  tools: oncall_tools,
  rate_limit: {max_calls: 100, message: "alert-loop cap"},
})
const triaged = agent_loop("Triage paging alerts.", triage_opts?.system, triage_opts)

// Release Captain: long checkpointed budget; pass `dry_run: true` (or
// a `with_dry_run` opts dict) to layer a shadow-run gate.
const ship_opts = agent_preset("release_captain", {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: release_tools,
  dry_run: true,
  cheap_caller: cheap_default_caller,
  frontier_caller: frontier_caller,
  escalate_predicate: { call -> call?.opts?.task_kind == "judge" },
  logging_sink: { record -> receipts.llm_call(record) },
})
const shipping = agent_loop("Cut v0.9.0.", ship_opts?.system, ship_opts)
```

Captain layers are opt-in: the preset only adds an `audit_sink` /
`telemetry` / `consent` / `rate_limit` / `dry_run` /
`handoff_sink` layer when the caller supplies the matching dependency
(plus the per-captain defaults in the table above). Pass an explicit
`tool_caller` to opt out completely. Captain presets also build an
`llm_caller` from `cheap_caller` + `frontier_caller` +
`escalate_predicate` (cheap-by-default with frontier escalation, per
the cost-moat substrate) and an optional `logging_sink` for receipts.

Each preset installs `reasoning_policy: "auto"`, `reasoning_scale: "small"`,
and a role-appropriate `reasoning_task` when the caller has not already set a
low-level `thinking` / `reasoning_effort` option or a reasoning policy hint.
`agent_loop_options` then applies the same provider-aware defaults used by
`llm_call`, so known model quirks are handled consistently instead of being
duplicated in each preset.

Caller-supplied `thinking`, `reasoning_effort`, `reasoning_policy`,
`reasoning_scale`, `reasoning_task`, and `iteration_budget` always win. Sugar:
`iteration_budget: "adaptive"` keeps the preset's numeric defaults and
explicitly switches the mode to adaptive.

When `daemon: true`, the loop transitions `active -> idle -> active` instead of
terminating on a text-only turn. Idle daemons can be woken by queued human
messages, `agent/resume` bridge notifications, `wake_interval_ms`, or watched
file changes from `watch_paths`.

Daemon idle is a special case of `agent_await_resumption`: the stdlib records
the same lifecycle audit and normalized `ResumeConditions` metadata, with
`timeout` / `on_event` preconfigured from daemon options, while keeping the
existing in-process idle wait so daemon-mode return behavior is unchanged.

For MCP server tool catalogs, see [MCP server tools](./tools.md#mcp-server-tools).

Native-tool stages also expose structured fallback / retry metadata in the
result `trace` summary. Look for `native_text_tool_fallbacks`,
`native_text_tool_fallback_rejections`, and `empty_completion_retries` when
debugging provider contract drift or OpenAI-compatible empty completions.

Default nudge message:

> The nudge is mode-aware:
> In tagged text-tool stages it asks for concrete tool progress and reserves `<done>##DONE##</done>` for real completion.
> In native-tool stages it asks for concrete tool progress and treats final text with no tool calls as completion.
> In no-tool sentinel stages it asks for concrete progress and reserves bare `##DONE##` for completion.

When `loop_until_done: true`, the system prompt is automatically extended with:

> IMPORTANT: You MUST keep working until the task is complete.
> The completion instruction is mode-aware:
> native-tool stages complete by returning final text with no tool calls,
> tagged text-tool stages use `<done>##DONE##</done>`, and no-tool sentinel
> stages use bare `##DONE##`.

`done_judge` adds a second gate after completion is detected. The loop renders
the transcript for a structured judge call and expects
`verdict: "done" | "continue"` plus optional `reasoning` and `next_step`.
A veto injects runtime feedback and the loop continues until the judge accepts,
`done_judge.max_invocations` is reached, or `max_verify_attempts` is exhausted.
Every judge call emits `JudgeDecision`
with `session_id`, `iteration`, `verdict`, `reasoning`, `next_step`, and
`judge_duration_ms`, plus optional `trigger`.

Set top-level `done_judge.max_invocations` (alias `max_feedback`) to a positive
integer to cap repeated done-judge vetoes. Once reached, the loop stops with
`status: "verify_capped"` and `stop_reason: "done_judge_cap_reached"`, and the
result carries `{done_judge: {invocations, vetoes, max_invocations,
cap_reached}}`. Set it to `0` to disable the terminal cap.

Use `done_judge.cadence` to gate the judge. Omit it to preserve the default:
every completion candidate is judged. `every: N` judges turns `N`, `2N`, and so
on; `max_invocations` caps total done-judge calls; `min_iterations_before_first`
skips the first K turns; `when` accepts `"always"`, `"stalled"`, or a closure
that receives the same loop-state shape as `loop_control`.
When `when: "stalled"` is configured, a stall warning can fire the judge
directly. A `done` verdict stops the loop with `stalled_done_judge` before the
repeated tool call is dispatched; a `continue` verdict leaves the existing
stall feedback injection path in place. The corresponding `JudgeDecision`
event carries `trigger: "stalled"`.

```harn
import { AgentLoopOptions } from "std/agent/options"

const judged_opts: AgentLoopOptions = {
  loop_until_done: true,
  done_judge: {
    cadence: {every: 5, when: "always", max_invocations: 3},
  },
}
agent_loop(task, system, judged_opts)
```

`when: "stalled"` does not fire on ordinary completion candidates. It is the
policy hook used by stall diagnostics so completion checks happen on a signal
rather than a fixed "are you done?" prompt.

### Input guardrail (`agent_input_guardrail`)

`agent_input_guardrail(classifier?, options?)` (from `std/agent/guardrails`)
builds the input-side bookend for `agent_completion_gate`: a cheap classifier
that runs before the first main `agent_loop` model turn. The returned bundle
spreads into loop options as `input_guardrail`. A tripwire records an
`input_guardrail_verdict` event, writes a zero-token assistant explanation, and
stops the loop with `status: "input_guardrail"` and
`stop_reason: "input_guardrail_tripwire"`.

```harn,ignore
import { agent_input_guardrail } from "std/agent/guardrails"

const guardrail_opts = agent_input_guardrail(
  { payload -> return cheap_policy_classifier(payload.user_message) },
  {confidence_threshold: 0.8},
)
agent_loop(task, system, base_opts + guardrail_opts)
```

For scripts that want an explicit preflight verdict instead of loop composition,
use `agent_input_guardrail_check(task, classifier?, options?)`, which returns the
same `{tripwire, reason, label, confidence}` shape.

### Completion gate (`agent_completion_gate`)

`agent_completion_gate(options)` (from `std/agent/judge`) builds a ready-made
done-time gate by **composing** the seams above: it returns a `verify_completion`
closure carrying a deterministic veto ladder plus an optional bounded LLM judge
on the `verify_completion_judge` / `done_judge` seam. Spread it into your loop
options. It generalizes the completion-verification policy proven out in
burin-code — the source-write evidence requirement, the per-session veto budget,
and AND-of-oracles verify composition — while keeping every **domain fact** a
host callback (the "Harn owns orchestration policy; hosts supply facts" split).

The gate never keys on a done-sentinel string: it decides purely from write and
verifier facts. Its default veto ladder (first match wins):

| Reason | Result | Condition |
| --- | --- | --- |
| `no_source_write` | veto (soft) | task requires a source change, but only cosmetic / zero source writes so far |
| `verification_after_write_red` | veto (**strict**) | a source write with a red verifier — never released by the budget |
| `verified_after_write` / `verified` | allow | verifier is green |
| `missing_verification` | veto (**strict**) | source written, verifier configured, not yet run — never released by the budget (a source write always needs a fresh green verifier) |
| `no_workspace_write` | allow | task does not require a source change |
| `veto_budget_exhausted` | allow | a soft veto after `max_vetoes` (default 3) — an attributable end |

Each deterministic gate decision emits a `judge_decision` event with
`trigger: "verify_completion"`, `confirm`, and the stable `reason` above. When
the veto budget converts a soft veto into an allow, the event uses
`reason: "veto_budget_exhausted"` and also carries `converted_from` with the
original veto class.

Only **source** writes count as progress toward "done" — a cosmetic final write
(a comment, a `.md` typo) is not evidence, so it cannot flip an already-passed
run back to unverified, and a run that only wrote cosmetics cannot claim done.

Host-fact callbacks (all optional): `facts(ctx)` returns
`{source_write_count?, cosmetic_write_count?, writes?, verify?, requires_write?}`;
`classify_write(path, diff?)` labels one write `"source"`/`"cosmetic"`/…;
`verify_command()` runs the verifier oracle. With **no** callbacks the gate
degrades to judge-only mode and surfaces the degraded state on the returned bundle
(`_completion_gate.facts_available = false`) rather than fabricating a pass.

```harn,ignore
import { agent_completion_gate } from "std/agent/judge"

agent_loop(task, system, base_opts + agent_completion_gate({
  facts: fn(ctx) { return host_completion_facts(ctx.session_id) },
  verify_command: fn() { return host_run_verify() },
  judge: true,          // optional bounded LLM judge, capped at 5 by default
}))
```

Presets can carry a default gate: the `completion_gate` pack row holds a
`CompletionGateOptions` spec that a consumer lowers with
`agent_completion_gate(...)`.

See [Completion gate (`std/agent/judge`)](../stdlib/agent-judge.md) for the full
reference: the option table, the fact types, the veto ladder, and the bounded
judge.

## Editing source from inside an agent loop

Agent loops that mutate code should reach for the AST-precise primitives in
[`std/edit`](../stdlib/edit.md) before freeform text patches. The cookbook
chapter [Precise edits with AST tools](../cookbook.md#precise-edits-with-ast-tools)
walks through the decision tree (replace → `edit_apply_node`, add →
`edit_insert_at_anchor`, rename → `edit_rename_symbol`, preview →
`edit_dry_run`, text fallback → `edit_safe_text_patch`) and ships a
`system_reminder` snippet you can lift into `agent_loop`'s session-start
hook so the model carries the same decision tree into every coding turn.

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
const daemon = daemon_spawn({
  name: "reviewer",
  task: "Watch for trigger events and summarize the latest change.",
  system: "You are a careful reviewer.",
  provider: "mock",
  persist_path: ".harn/daemons/reviewer",
  event_queue_capacity: 256,
})

daemon_trigger(daemon, {kind: "file_changed", path: "src/lib.rs"})
const snap = daemon_snapshot(daemon)
log(snap.pending_event_count)
daemon_stop(daemon)
const resumed = daemon_resume(".harn/daemons/reviewer")
```

These wrappers preserve queued trigger events across stop/resume. If a daemon is
stopped while a trigger is mid-flight, that trigger is re-queued and replayed on
resume instead of being lost.

### Context callback

`context_callback` lets you keep the full recorded transcript for replay and
debugging while showing the model a smaller or rewritten prompt-visible
history on each turn.

The callback receives one argument:

```harn,ignore
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
import { AgentLoopOptions } from "std/agent/options"

fn hide_old_assistant_turns(ctx) {
  let kept = []
  let latest_assistant = nil
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

const callback_opts: AgentLoopOptions = {
  loop_until_done: true,
  context_callback: hide_old_assistant_turns,
}
const result = agent_loop(task, "You are a coding assistant.", callback_opts)
```

### Post-turn callback

`post_turn_callback` runs after a tool-calling turn completes. Use it when the
workflow should react to the tool outcomes directly instead of waiting for the
model to emit another message.

The callback receives:

```harn,ignore
{
  session_id: string,
  iteration: int,
  has_tool_calls: bool,
  dispatch: list | dict | nil,
  tool_results: list,
  tool_count: int,
  tool_names: list,
  successful_tool_names: list,
  rejected_tool_names: list,
  session_successful_tools: list,
  session_rejected_tools: list,
  text: string,
  visible_text: string,
}
```

Each `tool_results` entry has:

```harn,ignore
{tool_name: string, ok: bool, status: string, rendered_result: string, error: string?}
```

It may return:

- a `string` to inject as the next user-visible message
- a `bool` where `true` stops the current stage immediately after the turn
- a `dict` with optional `message`, `feedback_kind`, `stop`, `stop_reason`,
  `next_options`, and `llm_options` fields. `message` is injected as runtime
  feedback. `feedback_kind` labels the resulting `feedback_injected` event and
  ACP `feedbackKind`; omit it to use `"post_turn"`. `terminal_callback` rich
  verdicts use the same field and default to `"terminal_callback"`.
  `stop_reason` overrides the default `"post_turn_stop"` reason when `stop` is
  true. `next_options` merges into the next loop iteration's options;
  `llm_options` merges into the next LLM call's `llm_options` dict.

Example: after a required read succeeds, ask the model to synthesize the final
answer with no more native tool calls:

```harn
fn finalize_after_read(turn) {
  if turn?.session_successful_tools?.contains("read_file") {
    return {
      message: "You have the required file evidence. Produce the final answer now.",
      llm_options: {tool_choice: "none"},
    }
  }
  return ""
}
```

### Example with retry

```harn
import { AgentLoopOptions } from "std/agent/options"

const retry_opts: AgentLoopOptions = {
  loop_until_done: true,
  max_iterations: 30,
  max_nudges: 5,
  provider: "anthropic",
  model: "claude-sonnet-5",
}
retry 3 {
  const result = agent_loop(task, "You are a coding assistant.", retry_opts)
  log(result.text)
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
- **JSONL seeding**: `agent_session_seed_from_jsonl(path, opts?)`
  creates a new session from an `llm_transcript.jsonl` sidecar. It
  imports exact prompt-visible `message` events or older full request
  snapshots, optionally checks `provider` / `model`, and supports
  `truncate_to_last` plus `drop_tool_calls` for oversized histories.
  Hosts can also attach external-session provenance with
  `source_agent`, `source_session_id`, `source_label`,
  `source_provenance`, and `recommend_compaction`. Provider-response-only
  sidecars require `validate: false` because they lack user and
  tool-result turns.

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
| `allowed_tools` | `list<string>` | Allowlist applied to the tool surface on activation |
| `prompt` | string | Body woven into the active-skill system-prompt block |
| `disable-model-invocation` | bool | When `true`, the matcher skips the skill entirely |
| `user-invocable` | bool | Placeholder for host UI (not consumed by the runtime today) |
| `mcp` | `list<string>` | MCP servers the skill wants booted (consumed by host integrations) |
| `on_activate` / `on_deactivate` | fn | Closures invoked on transition |

### Example

```harn,ignore
import { AgentLoopOptions } from "std/agent/options"

skill ship {
  description "Ship a production release"
  when_to_use "User says ship/release/deploy"
  paths ["infra/**", "Dockerfile"]
  allowed_tools ["deploy_service"]
  prompt "Follow the deploy runbook. One command at a time."
}

const ship_opts: AgentLoopOptions = {
  provider: "anthropic",
  tools: tools(),
  skills: ship,
  working_files: ["infra/terraform/cluster.tf"],
}
const result = agent_loop(
  "Ship the new release to production",
  "You are a staff deploy engineer.",
  ship_opts,
)
```

The loop emits one `skill_matched` event per match pass (including
zero-candidate passes so replayers see the boundary), one
`skill_activated` per activated skill, and one `skill_scope_tools`
event per activation whose `allowed_tools` narrowed the surface. When
`tool_surface_narrowing` removes unused tools between turns, the loop
also emits `skill_narrow` with `removed_tools`, `remaining_tools`, the
narrowing reason, policy details, removed-tool details, and kept-tool
details.

The default narrowing policy is safe by class: only tools classified as
`read_only` are prunable. Tools classified as `mutating`, `approval`,
`session_control`, `progress`, or `result_polling` remain visible even after
long discovery windows, and host/custom tools with missing annotations are
classified as `unknown` and kept. Host surfaces should annotate each tool with
`annotations.side_effect_level` (`none`, `read_only`, `workspace_write`,
`process_exec`, or `network`) plus a `kind` such as `read`, `search`, `edit`,
or `execute`. Use `tool_surface_narrowing: {mode: "aggressive"}` only when a
session intentionally wants usage-only pruning across all classes; callers can
still override `prune_classes`, `keep_classes`, `unknown_tool_policy`, and
`hard_keep` for narrower policies.

## Delegated workers

For long-running or parallel orchestration, Harn exposes a worker/task
lifecycle directly in the runtime.

```harn
const worker = spawn_agent({
  name: "research-pass",
  task: "Draft a summary",
  node: {
    kind: "subagent",
    mode: "llm",
    model_policy: {provider: "mock"},
    output_contract: {output_kinds: ["summary"]}
  }
})

const done = wait_agent(worker)
log(done.status)
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
const worker = spawn_agent({
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

Worker `options.resume_when` accepts the shared `ResumeConditions` shape used by
self-parking agents: optional `trigger`, `timeout`, and `on_event` fields.
`parse_resume_conditions(...)` validates that shape without spawning a worker;
`trigger` is checked by the same `std/triggers` trigger-spec parser used by
`trigger_register(...)`, while invalid fields raise `HARN-SUS-002` with the
failing field path.

Worker lifecycle builtins:

| Function | Description |
|---|---|
| `spawn_agent(config)` | Start a worker from a workflow graph or delegated stage |
| `sub_agent_request(task, options?)` | Build the normalized child-agent request used by `sub_agent_run` |
| `sub_agent_run(task, options?)` | Run an isolated child agent loop and return a single clean result envelope to the parent |
| `agent_lifecycle_tools(registry?, options?)` | Add model-facing lifecycle tools to a registry |
| `send_input(handle, task)` | Re-run a completed worker with a new task, carrying transcript/artifacts forward when applicable |
| `suspend_agent(worker, reason?, options?)` | Cooperatively suspend a worker, persist a resumable snapshot, and return `status: "suspended"` with `suspension` metadata |
| `resume_agent(worker_or_snapshot, resume_input?, continue_transcript?)` | Resume a suspended worker, optionally with new input; set `continue_transcript=false` to resume from the prior summary plus new input only |
| `agent_stop(worker, options?)` | Stop a worker. `{graceful: true}` returns a normalized handoff artifact plus recursively folded child handoffs before emitting `WorkerStopped`; omitted or `false` preserves hard cancel |
| `parse_resume_conditions(conditions?)` | Validate `trigger`, `timeout`, and `on_event` resume conditions for self-park and `spawn_agent({options: {resume_when}})` |
| `agent_await_resumption(reason, conditions?)` | Normalize the lifecycle-tool request used by `agent_loop` and daemon idle; `agent_loop` performs the actual suspension when the model calls the tool |
| `wait_agent(handle_or_list)` | Wait for one worker or a list of workers to finish |
| `close_agent(handle)` | Cancel a worker and mark it terminal |
| `list_agents()` | Return summaries for all known workers in the current runtime |

### Agent Lifecycle Tools

`agent_loop(...)` automatically exposes `agent_await_resumption` as a model tool.
When an agent is running as a worker, that tool is structural: the loop validates
optional `conditions` with `parse_resume_conditions(...)`, calls the same suspend
path as `suspend_agent(...)`, returns `status: "suspended"` to the parent, and
does not dispatch the tool as an ordinary handler result.

Top-level loops use the same result shape. If a root `agent_loop(...)` parks,
Harn persists a resumable worker snapshot and returns
`{status: "suspended", handle, reason, initiator: "self", ...}` to the direct
caller. The CLI can cold-restore that snapshot with:

```bash
harn run --resume .harn/workers/worker_...json
```

Parent-side lifecycle control is opt-in. Pass `subagents: true` or
`subagent_tools: true` in `agent_loop` options, or call
`agent_lifecycle_tools(registry, {subagents: true})`, to add
`subagent_pause(handle, reason)` and
`subagent_resume(handle, input?, continue_transcript? = true)`, and
`subagent_stop(handle, graceful? = true, reason?)`. Graceful stop returns
`{status: "stopped", handoff, children, handoffs, worker}` for parent takeover;
`graceful: false` keeps the old hard-cancel behavior.

### `sub_agent_run`

Use `sub_agent_run(...)` when you want a full child `agent_loop` with its own
session and narrowed capability scope, but you do not want the child transcript
to spill into the parent conversation history. `sub_agent_request(...)` exposes
the Harn-authored request normalization when callers need to inspect the tool
selection and child options before execution.

```harn,ignore
const result = sub_agent_run("Find the config entrypoints.", {
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
  log(result.data.paths)
} else {
  log(result.error.category)
}
```

The parent transcript only records the outer tool call and tool result. The
child keeps its own session and transcript, linked by `session_id` / parent
lineage metadata.

Pending parent `system_reminder` events are filtered into the child handoff
before the child loop starts. `propagate: "all"` reminders continue through
descendant sub-agents, `propagate: "session"` reaches direct children only, and
`propagate: "none"` remains local to the parent. Inherited reminders appear in
the child transcript with `source: "inherited"` and `originating_agent_id`.

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

`agent_loop(...)`, `sub_agent_run(...)`, and `spawn_agent(...)` accept
`approval_policy` for declarative allow/ask/deny gating before a tool runs.
Use `approval_policy.rules` for typed matching over tool name/kind,
side-effect level, declared paths, commands, URLs/domains/methods, MCP identity,
agent/persona/mode, capability operation, and repeated-call counts. Deny wins
over ask, ask wins over allow, and unmatched tools are approved. Active
approval policies deny sensitive filenames such as `.env` and private keys by
default, and declared host-absolute paths outside the workspace require an
explicit `external_roots` allowance. Ask decisions call
`session/request_permission`; the host request and the transcript event both
carry a `policyDecision` receipt with matched rule and rationale.

`agent_loop(...)`, `sub_agent_run(...)`, and `spawn_agent(...)` also accept a
`permissions` dict for per-agent dynamic policy. `allow` and `deny` entries can
be tool-name glob lists, argument pattern lists, or Harn predicates over the tool
args. For path-bearing tools, `std/tools.path_scope(...)` returns a matcher that
checks configured path argument keys against the active session
`workspace_anchor`; mounted roots can be filtered by `mount_modes` (for example,
`["extend"]` for writable roots). `on_escalation` receives a
`PermissionRequest` and may return `{grant: "once"}`, `{grant: "session"}`,
`true`, or `false`. Permission decisions are recorded as `PermissionGrant`,
`PermissionDeny`, and `PermissionEscalation` transcript events, while parent
`policy` ceilings still intersect with child declarations.

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
const worker = spawn_agent({
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
