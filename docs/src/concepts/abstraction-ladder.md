# Choosing an agent abstraction

Harn ships several agent primitives at different heights on the stack. Use the
lowest one that covers what you need. Climbing higher costs you control and code
transparency; climbing too low costs you machinery you'd otherwise get for free.

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
