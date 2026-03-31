# Harn

[![CI](https://github.com/burin-labs/harn/actions/workflows/ci.yml/badge.svg)](https://github.com/burin-labs/harn/actions/workflows/ci.yml)

Harn is a programming language and runtime for orchestrating coding agents.
It is designed to be the orchestration boundary between product code and
provider/runtime code: products declare workflows, policies, capabilities,
and UI hooks, while Harn owns transcripts, context assembly, retries,
tool routing, persistence, replay, and provider normalization.

## Install

From a GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/burin-labs/harn/main/install.sh | sh
```

With Cargo:

```bash
cargo install harn-cli
```

From source:

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
cargo install --path crates/harn-cli
```

## Quick Start

```bash
harn init my-project
cd my-project
harn run main.harn
harn test tests/
```

Simple LLM call:

```harn
let result = llm_call(
  "Explain quicksort in two sentences.",
  "You are a concise CS tutor."
)
println(result.visible_text)
```

Persistent agent loop with tools:

```harn
let result = agent_loop(
  "Fix the failing test and verify the change.",
  "You are a senior engineer.",
  {
    persistent: true,
    tools: ["read_file", "search", "edit", "run"],
    max_iterations: 24
  }
)

println(result.status)
println(result.visible_text)
```

## What Ships In Harn v0.4.30

- Typed workflow graphs via `workflow_graph(...)` and `workflow_execute(...)`
  with explicit nodes, edges, validation, policy attachment, map/join style
  stages, and resumable execution.
- Typed artifacts and resources as the real context boundary. Context
  selection is artifact-aware, budget-aware, and policy-driven rather than
  raw prompt concatenation.
- Durable run records with persisted stage transcripts, artifacts, policy
  decisions, verification outcomes, and CLI inspection/replay/eval entrypoints.
- Provider-normalized LLM output with `visible_text`, `private_reasoning`,
  `tool_calls`, `blocks`, `provider`, `stop_reason`, and transcript events.
- Structured transcript lifecycle support: continue, fork, compact,
  summarize, render public-only output, or render full execution history.
- Workflow meta-editing builtins such as `workflow.inspect`, clone/insert/
  replace/rewire operations, per-node model/context/transcript policy edits,
  diff, validate, and commit-style validation.
- Capability ceiling enforcement for workflows and sub-orchestration:
  internal plans may narrow capabilities but cannot exceed the host ceiling.
- ACP/bridge queued-user-message handling modes for agent execution:
  interrupt immediately, inject after the current operation, or defer until
  the agent yields back to the human.

## Why This Matters

Without a runtime boundary like Harn, application code tends to accumulate:

- provider-specific message/response parsing
- transcript compaction and summarization logic
- tool dispatch and retry behavior
- workflow branching and repair loops
- provenance, replay, and eval fixtures
- host/editor queue semantics

Harn moves those concerns into a typed runtime layer so a host app such as
Burin can stay focused on:

- capabilities it wants to expose
- top-level policy ceilings
- workflow templates and product defaults
- UI/session integration

## Workflow Runtime Example

```harn
let graph = workflow_graph({
  name: "review_and_repair",
  entry: "plan",
  nodes: {
    plan: {
      kind: "stage",
      mode: "llm",
      task_label: "Planning task",
      model_policy: {model_tier: "small"},
      context_policy: {include_kinds: ["summary", "resource"], max_tokens: 1200}
    },
    implement: {
      kind: "stage",
      mode: "agent",
      tools: ["read_file", "edit", "run"],
      model_policy: {model_tier: "mid"},
      retry_policy: {max_attempts: 2}
    },
    verify: {
      kind: "verify",
      mode: "agent",
      tools: ["run"],
      verify: {assert_text: "PASS"}
    }
  },
  edges: [
    {from: "plan", to: "implement"},
    {from: "implement", to: "verify"},
    {from: "verify", to: "implement", branch: "failed"}
  ]
})

let artifacts = [
  artifact({
    kind: "resource",
    title: "Editor selection",
    text: read_file("src/lib.rs"),
    source: "workspace"
  })
]

let run = workflow_execute(
  "Refactor the parser error message and verify it.",
  graph,
  artifacts,
  {max_steps: 8}
)

println(run.status)
println(run.path)
println(run.run.stages)
```

## Transcript And Artifact Model

`llm_call(...)` and `agent_loop(...)` now return a canonical schema that
separates human-visible output from internal execution state:

- `visible_text`: safe assistant-visible text
- `private_reasoning`: provider reasoning metadata when available
- `tool_calls`: normalized tool intent
- `blocks`: canonical structured blocks across providers
- `provider`: normalized provider identity
- `transcript`: persisted transcript state with `messages` and `events`

Artifact records are durable typed objects with provenance:

```harn
let note = artifact({
  kind: "analysis_note",
  title: "Parser regression risk",
  text: "The lexer span mapping affects diagnostics and tree-sitter tests.",
  source: "review",
  relevance: 0.9,
  metadata: {owner: "runtime"}
})

let focused = artifact_select([note], {
  include_kinds: ["analysis_note"],
  max_tokens: 200
})
```

## Host Integration

Run Harn as an ACP backend:

```bash
harn acp
harn acp agent.harn
```

Inspect persisted run records:

```bash
harn runs inspect .harn-runs/<run>.json
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
```

Queued human messages can be delivered to an in-flight agent through host
notifications:

- `interrupt_immediate`: stop the current deliberation boundary and inject now
- `finish_step`: inject after the current tool/operation boundary
- `wait_for_completion`: defer until the agent yields control

## Documentation

- [Docs book](docs/src/introduction.md)
- [Workflow runtime guide](docs/src/workflow-runtime.md)
- [LLM calls and agent loops](docs/src/llm-and-agents.md)
- [MCP and ACP integration](docs/src/mcp-and-acp.md)
- [CLI reference](docs/src/cli-reference.md)
- [Builtin reference](docs/src/builtins.md)
- [Language spec](spec/HARN_SPEC.md)

## Development

```bash
make fmt
make lint
make test
make conformance
make all
```

The workspace includes:

- `harn-lexer`: scanner/tokenizer
- `harn-parser`: parser, AST, type checker, diagnostics
- `harn-vm`: compiler, interpreter, LLM/runtime/orchestration layer
- `harn-fmt`: formatter
- `harn-lint`: linter
- `harn-cli`: CLI, ACP, A2A, conformance runner
- `harn-lsp`: language server
- `harn-dap`: debugger adapter
- `tree-sitter-harn`: syntax grammar for editor integrations
