# LLM streaming and transcripts

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

## Partial deltas and usage

Streaming transports emit text deltas as soon as the provider sends them. Native
tool-call streams also surface partial argument deltas in agent trace events:
`raw_input` when the bytes parse as JSON, or `raw_input_partial` while the JSON
object is still incomplete.

Final token usage is recorded after the provider response completes. Read it
from the `llm_call` / `agent_loop` result, from `llm_usage()`, or from the
workflow session usage summary shown below.

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
an LLM, or `transcript_compact()` when you want the runtime compaction engine
outside the `agent_loop` path.

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

`std/agents` also exposes worker helpers for delegated/background orchestration:
`worker_request(worker)`, `worker_result(worker)`, `worker_provenance(worker)`,
`worker_research_questions(worker)`, `worker_action_items(worker)`,
`worker_workflow_stages(worker)`, and `worker_verification_steps(worker)`.

For durable persona handoff, prefer a typed artifact over copying the child or
parent transcript forward. Use `handoff(...)` to normalize a structured
handoff payload, `handoff_artifact(...)` to carry it through the workflow
artifact channel, and `handoff_context(...)` when a receiver needs a prompt-safe
summary of the transferred task/evidence/budget fields. The handoff artifact is
the product; the transcript stays on the source side of the boundary.

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
