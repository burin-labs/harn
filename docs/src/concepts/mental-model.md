# Mental model

A Harn conversation is a stack of nested boxes. The outer boxes contain the
inner ones; sessions sit alongside the stack and outlive any single box.

```text
Pipeline                          <- the .harn program itself
└── Workflow                      <- optional typed graph above raw calls
    └── Stage                     <- one node in that graph
        └── Agent loop            <- "keep going until done"
            └── Iteration         <- one model request-response cycle
                ├── LLM call      <- one HTTP-ish request to a model
                └── Tool call(s)  <- 0..N inside that iteration

Orthogonal to all of the above:
    Session                       <- the durable conversation handle
    └── Transcript                <- {messages, events, assets}
    Worker / Subagent             <- a delegated agent with its own loop
```

Each layer adds exactly one capability over the layer below it. If you don't
need the capability, don't reach for the layer.

## Layer by layer

**LLM call.** One request to a model. The atom. `llm_call(prompt, system, opts)`
returns a dict with `text`, `tool_calls`, token counts, and a transcript
fragment. No looping, no completion detection.

**Tool call.** The model emitted "please run `read_file(...)`". Tool calls
happen *inside* an iteration; one iteration can contain several. They're
recorded in the transcript as their own events.

**Iteration.** One round-trip: prompt-out, response-in, plus whatever tool
dispatch happens before the next prompt-out. The agent loop counts iterations
and stops when `max_iterations` is hit or the model signals completion.

**Agent loop.** Run iterations until done. `agent_loop(prompt, system, opts)`
owns completion detection, tool dispatch, budget enforcement, and the terminal
`status` (`done`, `stuck`, `suspended`, `budget_exhausted`, `provider_error`,
`idle`, `watchdog`, `failed`).

**Stage.** A named node in a workflow graph. Wraps an agent loop, an `llm_call`,
a verification step, a join, or a sub-agent. Gives the unit a type contract and
a position in the graph.

**Workflow.** A typed, inspectable, replayable graph of stages.
`workflow_execute(task, graph, artifacts, opts)`. Lives above the raw calls so a
multi-stage agent isn't just a script that calls `agent_loop` four times.

**Pipeline.** The top-level `.harn` program with `fn main(harness)` and the
harness lifecycle callbacks (`PreFinish`, `on_finish`, `OnUnsettledDetected`,
`PostFinish`). Not itself agentic — it's the container in which agents, workers,
and workflows run.

## The orthogonal axis: sessions

A session is the durable container for a conversation. It owns the transcript,
knows its parent and children (sessions can fork), pins a system prompt and a
model, and survives across many agent loop invocations.

You can:

- Run several agent loops against one session and they share the transcript.
- Fork a session for counterfactual exploration without disturbing the original.
- Compact a session's transcript when it grows too long for the context window.
- Snapshot a session for replay and audit.
- Close a session when you're done.

Sessions are independent of loops. A single agent loop creates an implicit
session if you don't pass one; a long-running daemon may use one session across
hundreds of loops.

## The other orthogonal axis: workers

A worker is an agent running in its own execution context, with its own
transcript, its own loop, and its own snapshot. You `spawn_agent` to create one,
`suspend_agent` to pause it cooperatively at the next iteration boundary,
`resume_agent` to wake it (optionally with new input), and
`agent_await_resumption` from inside the worker for self-park.

Workers exist so that a parent pipeline can delegate work without coupling its
own loop to the child's. They're the unit of parallelism, of background daemons,
and of multi-agent orchestration.

## What this model is good at

- Predicting where a value comes from. If you have a `dict` in hand and want to
  know what produced it, walk up the layers: this is a tool call result; it
  lives inside an iteration; the iteration is part of an agent loop; the loop
  ran inside a stage of a workflow; the workflow ran in a pipeline.
- Reasoning about cost and latency. Cost lives at the LLM-call layer; the loop
  layer multiplies it by iteration count; the worker layer multiplies again by
  parallelism.
- Choosing the right abstraction. The decision guide is in [Choosing an agent
  abstraction](./abstraction-ladder.md): climb the layers only when you need
  what the next one adds.

## What this model deliberately doesn't model

- The protocol layer. MCP, ACP, and A2A live *underneath* `llm_call` and
  *outside* the pipeline boundary. They're transport, not orchestration. See
  [protocol support](../protocol-support.md).
- Retries and middleware. Both are handled by composing functions around
  `llm_call` (the [handler middleware](../stdlib/llm-handlers.md) pattern), not
  by adding a new layer to the stack.
- Memory and skills. Both ride on the session/transcript axis.

If you ever find yourself wondering "where does this concept live in the stack,"
check the [glossary](./glossary.md) first — every name in Harn is documented to
one of these layers.
