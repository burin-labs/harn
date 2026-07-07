# LLM streaming and transcripts

## Streaming responses

`llm_stream` returns a channel that yields response chunks as they
arrive. Iterate over it with a `for` loop:

```harn
const stream = llm_stream("Tell me a story", "You are a storyteller")
for chunk in stream {
  log(chunk)
}
```

`llm_stream` accepts the same options as `llm_call` (provider, model,
max_tokens). The channel closes automatically when the response is
complete.

`llm_stream_call` is the script-facing streaming variant of `llm_call`.
It returns a first-class `Stream` of chunk dicts instead of a channel of
raw strings:

```harn
const chunks = llm_stream_call("Tell me a story", nil, {provider: "openai"})
for chunk in chunks {
  log(chunk.visible_delta)
  if chunk.partial.contains("REFUSAL") {
    break
  }
}
```

Each chunk has `{delta, visible_delta, partial, role, finish_reason}`.
`delta` is the provider text delta, `visible_delta` and `partial` hide
open internal `<think>` blocks, and the terminal chunk carries
`finish_reason` when the provider reports one. Dropping the stream
aborts the background LLM request. The existing `stream` option on
`llm_call` and `llm_stream_call` still only controls provider transport
selection; it does not change `llm_call`'s return type.

When an app-level persona asks the model to keep a private notebook in tagged
text, use `std/agent/stream` instead of filtering chunks inline:

```harn,ignore
import {agent_stream_call} from "std/agent/stream"

const result = agent_stream_call(prompt, system, {
  provider: "openai",
  model: "gpt-5-mini",
  private: {open_tag: "<secret>", close_tag: "</secret>"},
  on_delta: { delta, _event, _state -> print(delta) },
})
```

`agent_private_stream_delta(...)` holds back enough suffix text to detect
opening private tags split across chunks, `agent_private_stream_finish(...)`
flushes only safe visible suffixes, and `agent_stream_call(...)` returns a
terminal `{ok, status, text, visible_text}` envelope on both success and stream
interruption.

When the harness runs a full `agent_loop` (tools, transcript, completion policy)
rather than a single `llm_stream_call`, use the loop's own
[`on_delta` streaming seam](agent_loop.md#streaming-visible-text-deltas) instead
of dropping to a raw stream. Each per-turn call is issued through the streaming
transport, and the closure sees one delta per chunk of the assistant's visible
text — fold each delta through `agent_private_stream_delta` from
`std/agent/stream` inside the callback to mask a `<secret>` span while it renders.
The callback is observational and the turn still returns a complete result, so
tool dispatch is unaffected; providers that do not stream fall back to a single
full-text delta:

```harn,ignore
agent_loop("summarize the diff", nil, {
  provider: "anthropic",
  model: "claude-sonnet-5",
  on_delta: { delta -> print(delta) },
})
```

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
const first = llm_call("Plan the work", nil, {provider: "mock"})

const second = llm_call("Continue", nil, {
  provider: "mock",
  transcript: first.transcript
})

const compacted = transcript_compact(second.transcript, {
  keep_last: 4,
  summary: "Planning complete."
})
```

Use `transcript_summarize()` when you want Harn to create a fresh summary with
an LLM, or `transcript_compact()` when you want the runtime compaction engine
outside the `agent_loop` path. `transcript_compact()` accepts the same
`CompactionPolicy` instruction fields as agent-loop auto-compaction, so hosts
can route `/compact <instructions>` through one audited path.

Transcript helpers also expose the canonical event model:

```harn
const visible = transcript_render_visible(result.transcript)
const full = transcript_render_full(result.transcript)
const events = transcript_events(result.transcript)
```

Use these when a host app needs to render human-visible chat separately from
internal execution history.

For chat/session lifecycle, `std/agents` now exposes a higher-level workflow
session contract on top of raw transcripts and run records:

```harn
import "std/agents"

const result = task_run("Write a note", some_flow, {provider: "mock"})
const session = workflow_session(result)
const forked = workflow_session_fork(session)
const archived = workflow_session_archive(forked)
const resumed = workflow_session_resume(archived)
const persisted = workflow_session_persist(result, ".harn-runs/chat.json")
const restored = workflow_session_restore(persisted.run.persisted_path)
```

Each workflow session also carries a normalized `usage` summary copied from the
underlying run record when available:

```harn
log(session?.usage?.input_tokens)
log(session?.usage?.output_tokens)
log(session?.usage?.total_duration_ms)
log(session?.usage?.call_count)
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
  let tools = tool_registry()
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

const graph = workflow_graph({
  name: "review_and_repair",
  entry: "act",
  nodes: {
    act: {kind: "stage", mode: "agent", tools: review_tools()},
    verify: {kind: "verify", mode: "agent", tools: tool_select(review_tools(), ["run"])}
  },
  edges: [{from: "act", to: "verify"}]
})

const run = workflow_execute(
  "Fix the failing test and verify the change.",
  graph,
  [],
  {max_steps: 6}
)
```

This keeps orchestration structure, transcript policy, context policy,
artifacts, and retries inside Harn instead of product code.
