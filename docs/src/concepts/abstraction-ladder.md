# Choosing an agent abstraction

Harn ships several agent primitives at different heights on the stack. Use the
lowest one that covers what you need. Climbing higher costs you control and code
transparency; climbing too low costs you machinery you'd otherwise get for free.

## The three rungs, in one line

The whole ladder collapses to three primary rungs, chosen by *how many goals*
the work has:

> [`llm_call`](../llm/llm_call.md) = **one request** <
> [`agent_loop`](../llm/agent_loop.md) = **one goal** (one transcript, run to
> completion) < [`workflow`](../workflow-runtime.md) = **more than one goal,
> attempt, or model**.

Two rules keep you on the right rung:

- **Never hand-write a `while` around `llm_call`.** If a single goal needs
  several model round-trips — call a tool, read the result, decide what's next —
  that *is* `agent_loop`. Rolling your own loop re-implements completion
  detection, budgets, transcript management, and status semantics that the loop
  already owns. (The rare exceptions are in [When to write your own
  loop](#when-to-write-your-own-loop).)
- **Lift to a `workflow` only when the *shape* matters** — a second goal, a
  retry-with-feedback attempt loop, a different model per stage, a
  verify/join/fork, or replay and audit. One agent doing one job is never a
  workflow.

### `agent_preset` is not a rung

[`agent_preset(kind, options?)`](../llm/agent_loop.md#presets-how-you-build-agent_loop-options) is an
**options-builder for `agent_loop`**, not a tier above or below it. It resolves
per-kind fill-nil defaults (provider, budget, model ladder, completion gate,
lanes/overlays…) and returns a plain `agent_loop` options dict — you still call
`agent_loop` with the result. Reach for it to *configure* the `agent_loop` rung
consistently, never as a substitute for choosing a rung.

### Model ladders are not a rung either

`models:` / `ladder:` on `llm_call` (and `agent_loop`) is a cheap-first,
escalate-on-*transport*-failure fallback *within a single request* — see
[Model ladders](../docs/llm/harn-quickref.md#model-ladders-models--ladder). It
changes which model answers, not which rung you are on. A ladder never advances
on a schema-validation failure (that re-asks the same rung), and it is mutually
exclusive with an explicit `model:`/`routing:`.

## The ladder

| Reach for | When | What you get | What you give up |
|---|---|---|---|
| [`llm_call`](../llm/llm_call.md) | One question, one answer. Classification, summarization, extraction, completions. | Direct control over tokens, cache, schema. Cheapest. | No looping, no automatic tool dispatch, no completion detection. |
| [`llm_call_structured`](../llm/llm_call.md#llm_call_structured) | Same as above, but the answer must match a schema. | Validated JSON, safe and result-envelope variants. | One extra schema-validation pass. |
| [`agent_loop`](../llm/agent_loop.md) | The model needs several iterations — calling tools, reading results, deciding what to do next. | Tool dispatch, completion sentinels, budgets, status outcomes, transcript management, profiles, skills, daemon mode. | More machinery; opinionated about what "done" means. |
| [`agent_turn`](../llm/agent_loop.md) | Same as `agent_loop` but you want a judge to decide completion. | Loop + automatic `done_judge` + per-iteration judge decisions. | Extra LLM calls for the judge. |
| [`spawn_agent`](../agent-lifecycle.md) / [`sub_agent_run`](../agent-lifecycle.md) | A *separate* agent should run, possibly in parallel or background, with its own transcript and possibly a different model. | Independent execution context, suspend/resume, snapshots, joins. | Coordination overhead — handles, resume conditions, wait points. |
| [`workflow_execute`](../workflow-runtime.md) | The orchestration shape itself matters — multiple stages, conditional branches, joins, replay, audit, typed contracts. | Typed graph, validated topology, per-stage results, replay, structured artifacts. | Up-front graph definition. Overkill for "one agent does one job." |
| [`tree_of_thoughts`](../llm/ensemble.md) | Deliberate branching search where you score and prune candidates. | Deterministic BFS/DFS/beam with caller-defined `expand`/`evaluate`/`is_terminal`. | You write the search semantics. |
| Handler middleware ([`std/llm/handlers`](../stdlib/llm-handlers.md)) | Cross-cutting concerns under every LLM call: retry, cache, rate limit, circuit-breaker. | A composable middleware chain at the call boundary. | One more layer to read when debugging. |

## The five-step decision

1. **One shot?** → `llm_call`. If you need JSON, `llm_call_structured`.
2. **Loop until done?** → `agent_loop`.
3. **Loop until a *judge* says done?** → `agent_turn`.
4. **Need a parallel or backgrounded helper agent?** → `spawn_agent` or
   `sub_agent_run`.
5. **Need typed, inspectable, replayable orchestration over many stages?** →
   `workflow_execute`.

If your answer to all five is "yes, sort of," start with `agent_loop` and lift
to a workflow when the orchestration shape genuinely starts to matter — usually
around the third or fourth stage.

## Two common anti-patterns

**Building a workflow when an agent loop would do.** A workflow with three
stages where every stage is `kind: "stage", mode: "agent"` and the only edges
are linear is just an agent loop with extra YAML. Use a workflow when stages
differ in kind (verify, join, fork, subagent) or when you need replay-aware
artifact passing.

**Building an agent loop when an `llm_call` would do.** If your "agent" makes
one model call, parses the result, and returns, it doesn't need a loop. The loop
machinery adds latency, transcript management, and status semantics you're not
using.

## The placement contract: where cross-cutting mechanisms live

The rungs answer *how many goals*. A second question — *where does each
cross-cutting concern live?* — has one canonical answer per concern, so you
always import the same module rather than re-deriving the behavior inline. Every
one of these is a plain stdlib module that composes onto `agent_loop` options
(or, for a preset, is bundled by a [pack row](../llm/agent_loop.md#presets-how-you-build-agent_loop-options)):

| Concern | Lives in | Reach for |
|---|---|---|
| **"Are we actually done?"** completion gate | [`std/agent/judge`](../llm/agent_loop.md#completion-gate-agent_completion_gate) | `agent_completion_gate(options)` — a deterministic veto ladder plus an optional bounded LLM judge, spread into `agent_loop`. |
| **Pace / budget governors** | [`std/agent/governors`](../stdlib/governors.md) | `with_governance(...)`, `governor_decision(...)` — bound cost and cadence. |
| **Progress / stall detectors** (unified) | [`std/agent/stall`](../stdlib/governors.md#unified-detectors) | `agent_stall_initial_state()` + `agent_stall_observe_tool_calls(...)` / `agent_stall_no_net_progress(...)` — ping-pong, no-net-progress, and repeated-verified-pass detection in one place. |
| **Tool-surface narrowing** (lanes) | [`std/agent/lanes`](../stdlib/agent-lanes-overlays.md) | `lane_policy(rows, task, opts)` — classify the task, hide the tools it can't need. |
| **Prompt overlays** (data-driven nudges) | [`std/agent/overlays`](../stdlib/agent-lanes-overlays.md#overlays-data-driven-prompt-nudges) | `with_overlay(opts, rows, mode)` — fill-nil prompt fragments, never overriding explicit input. |
| **Auto-compaction** (*when* to compact) | [`std/agent/autocompact`](../llm/agent_loop.md#agent-loop-compaction) | `compaction_policy(...)`, `agent_autocompact_if_needed(session, opts)` — keep the transcript under the context ceiling. |
| **Compaction pins** (*what* to preserve) | [`std/agent/pins`](../stdlib/agent-pins-goal.md) | `pin(kind, content)`, `with_pin_roots(opts, pins)`, `pin_compaction_policy(pins)` — a typed pin taxonomy that survives compaction by construction and doubles as reachability-GC roots. Pins *feed* auto-compaction's preservation; they don't decide when it runs. |
| **Goal object** (structured objective + convergence) | [`std/agent/goal`](../stdlib/agent-pins-goal.md) | `goal(spec)`, `with_goal(opts, g)`, `goal_check(g, facts)`, `goal_reloop(g)` — machine-checkable success criteria, a done-judge composed from `std/agent/judge`, and a bounded re-loop. The durable *what*; not a per-turn surface. |
| **Running-notes recitation** (scratchpad) | `std/agent/scratchpad` | `agent_scratchpad_options(...)`, `agent_scratchpad_recitation_fragment(session, opts)` — re-surface the goal and running notes at the prompt tail each turn. The per-turn *recitation surface* for the goal object above. |
| **Default mutation toolset** | [`std/agent/host_tools`](../llm/tools.md#default-mutation-tools) | `agent_edit_tools(registry?, opts?)` — the canonical `write_file` / `edit_file` / `create_directory` / `delete_path` set; customize through the existing middleware seams. |
| **Retry with feedback** (attempt loop) | [`std/workflow` stage `retry_policy`](../workflow-runtime.md#retry-with-feedback) | `retry_policy: {max_attempts, feedback}` or a `repair_prompt_builder` closure — thread findings into the next attempt. This is a *workflow* concern (more than one attempt at a goal). |
| **Bundling several of the above** | [`std/agent/presets`](../llm/agent_loop.md#presets-how-you-build-agent_loop-options) | `agent_preset(kind, options?)` — one fill-nil pack ships a budget, provider, model ladder, completion gate, lanes, and overlays together. |

The rule of thumb: **if you're about to write governor / detector / gate /
lane / overlay / compaction logic inline in a loop body, import the module for
it instead.** These modules are the home; the loop is the caller.

## When to write your own loop

Rarely. The cases that justify hand-rolling on top of `agent_dispatch_tool_call`
and `agent_parse_tool_calls`:

- You need a custom completion detector that doesn't fit `done_sentinel` or
  `done_judge`.
- You're implementing a research pattern (tree search, voting, debate) where the
  loop body isn't "ask, dispatch, append".
- You're building a different *kind* of agent — one that doesn't talk back, only
  emits actions.

If you find yourself wanting `agent_loop` minus *one* feature, file an issue.
The loop is meant to be the right answer for ~95% of multi-iteration agents, and
missing features usually mean the loop hasn't grown an option it should have.

## What about the chat surface?

For interactive user-facing chats, prefer
[`agent_chat_loop`](../llm/agent_loop.md#interactive-chat-loops) over running
`agent_loop` in your own input loop. It preserves one session across user turns,
routes slash commands, and handles the `wait_for_user` terminal tool.

## See also

- [Build your first workflow](../tutorials/build-your-first-workflow.md) — the
  hands-on path up these rungs: one `llm_call`, then an `agent_loop`, then a
  `workflow_stages` with a verify stage and retry-with-feedback, runnable end to
  end on the mock provider.
- [The expressiveness spectrum](./expressiveness-spectrum.md) — the same task
  solved at five escalating levels of control, and why you never pay for
  machinery you don't use.
- [Glossary](./glossary.md) — one-line definitions for every rung and container
  (LLM call, iteration, agent loop, stage, workflow, session…).
- [Coming from elsewhere](./sota-comparison.md) — what LangGraph / OpenAI / ACP
  call the same ideas.
- [Migrating to 0.10](../migrations/v0.10.md) — the removed resilience options
  (`llm_retries`, `llm_backoff_ms`, `transcript_policy`) and their replacements.
