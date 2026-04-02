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

Remote MCP OAuth:

```bash
harn mcp redirect-uri
harn mcp login https://mcp.notion.com/mcp
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
fn coding_tools() {
  var tools = tool_registry()
  tools = tool_define(tools, "read", "Read a file", {
    parameters: {path: {type: "string", description: "Path to read"}},
    returns: {type: "string"},
    handler: nil,
  })
  tools = tool_define(tools, "search", "Search project files", {
    parameters: {pattern: {type: "string", description: "Pattern to search"}},
    returns: {type: "string"},
    handler: nil,
  })
  tools = tool_define(tools, "edit", "Edit a file", {
    parameters: {path: {type: "string", description: "Path to edit"}},
    returns: {type: "string"},
    handler: nil,
  })
  tools = tool_define(tools, "run", "Run a command", {
    parameters: {command: {type: "string", description: "Command to run"}},
    returns: {type: "string"},
    handler: nil,
  })
  return tools
}

let result = agent_loop(
  "Fix the failing test and verify the change.",
  "You are a senior engineer.",
  {
    persistent: true,
    tools: coding_tools(),
    max_iterations: 24
  }
)

println(result.status)
println(result.visible_text)
```

`tool_define(...)` also preserves extra config keys such as `policy`, so
workflow runtimes can enforce capability metadata attached directly to the tool
registry instead of duplicating the same ceilings in separate node config.

## Core Capabilities

- Typed workflow graphs via `workflow_graph(...)` and `workflow_execute(...)`
  with explicit nodes, edges, validation, policy attachment, map/join style
  stages, and resumable execution.
- Delegated worker lifecycle builtins via `spawn_agent(...)`, `send_input(...)`,
  `resume_agent(...)`, `wait_agent(...)`, `close_agent(...)`, and `list_agents()`,
  with child run lineage, persisted worker snapshots, and host-visible worker
  lifecycle events.
- Explicit continuation policy for delegated workers: artifact carryover,
  transcript fork/reset/compaction, workflow resume control, and normalized
  `worker_result` artifacts.
- Runtime schema helpers for structured LLM I/O: `schema_check(...)`,
  `schema_parse(...)`, JSON Schema export, and schema composition helpers.
- Design-by-contract and project orchestration helpers: `require ...`,
  metadata/scanner runtime builtins, and `import "std/project"` for
  freshness-aware project state assembly inside Harn itself.
- Isolated execution substrate via directory-scoped command builtins
  (`exec_at`, `shell_at`) plus the `std/worktree` module for git worktree
  creation, status, diff, shell execution, and cleanup. Worker execution
  profiles can now pin delegated runs to a cwd, env overlay, or managed
  worktree so background execution is reproducible instead of ambient-cwd
  dependent.
- Stronger preflight behavior via `harn check`: import graph resolution,
  literal template/render path validation, import symbol collision detection,
  and host capability contract validation all fail before runtime.
  `render(...)` resolves relative to the module source tree (including inside
  imported modules) instead of the ambient process cwd. Literal delegated
  execution roots, `exec_at(...)` / `shell_at(...)` directories, and unknown
  `host_invoke(...)` capability/operation pairs are also checked before launch.
- Runtime-local typed host mocking for tests via `host_mock(...)`,
  `host_mock_clear()`, and `host_mock_calls()`, so `.harn` conformance and VM
  tests can exercise host-backed flows without requiring a live bridge host.
- Eval suite manifests and baseline comparisons via `eval_suite_manifest(...)`,
  `eval_suite_run(...)`, and `harn eval <manifest.json>`, so grouped replay
  regression suites are first-class runtime data instead of external scripts.
- Typed artifacts and resources as the real context boundary. Context
  selection is artifact-aware, budget-aware, and policy-driven rather than
  raw prompt concatenation.
- Host-facing artifact helpers for workspace files, snapshots, editor
  selections, command/test/verification outputs, and diff/review decisions,
  so product code can pass structured state into Harn without rebuilding
  artifact taxonomy or provenance conventions.
- Durable run records with persisted stage transcripts, artifacts, policy
  decisions, verification outcomes, delegated child lineage, and
  inspection/replay/eval entrypoints including recursive run-tree loading.
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
- Remote MCP over stdio and HTTP, including OAuth metadata discovery, stored
  bearer tokens for standalone CLI use, and automatic token reuse for HTTP MCP
  servers declared in `harn.toml`.
- Runtime semantic cleanup for older surfaces: repeated `catch e { ... }`
  bindings now work within the same enclosing block, and float division keeps
  IEEE `NaN`/`Infinity` behavior instead of raising runtime errors.
- Formatter width handling now wraps oversized comma-separated forms
  consistently across calls, list literals, dict literals, enum payloads, and
  struct-style construction instead of leaving long single-line output intact.
- Tool lifecycle hooks via `register_tool_hook(...)`: pre-execution deny/modify
  and post-execution result interception for agent tool calls, with glob-pattern
  matching on tool names.
- Automatic transcript compaction in agent loops: microcompaction snips oversized
  tool outputs, auto-compaction triggers at configurable token thresholds, and
  `compact_strategy` supports default LLM summarization, truncate fallback, or
  custom Harn closure-based compaction. The same pipeline is exposed directly as
  `transcript_auto_compact(...)`.
- Daemon agent mode (`daemon: true`): agents stay alive waiting for
  host-injected messages instead of terminating on text-only responses, with
  adaptive idle backoff and explicit bridge wake/resume signaling.
- Per-agent capability policies with argument-level constraints: `agent_loop`
  accepts a `policy` dict to scope tool permissions, including `tool_arg_constraints`
  for pattern-matching on tool arguments.
- Adaptive context assembly with deduplication and microcompaction via
  `select_artifacts_adaptive(...)`, plus `estimate_tokens(...)` and
  `microcompact(...)` utility builtins.
- Host-aware static preflight: `harn check` can load host-specific capability
  schemas and alternate bundle roots from `harn.toml` or CLI flags so host
  adapters and bundled template layouts validate cleanly.
- Mutation-session audit metadata for workflows, delegated workers, and bridge
  tool gates so hosts can group write-capable operations under one trust
  boundary without forcing one edit-application UX.
- String method aliases for case normalization: `.lower()`, `.upper()`,
  `.to_lower()`, and `.to_upper()`.

## Trust Boundary

Harn owns orchestration and provenance. Hosts own concrete mutation UX.

- Harn owns workflow execution, transcript lifecycle, replay/eval, worker
  lineage, artifact provenance, and mutation-session audit metadata.
- Hosts own approvals, patch/apply UX, concrete file mutations, and editor
  undo/redo semantics.

For autonomous or background edits, the recommended default is worktree-backed
execution plus explicit host approval for destructive operations.

## Release Workflow

Maintainers can run the comprehensive docs audit, verification gate, version
bump flow, and publish ritual through:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

`scripts/publish.sh` remains the crates.io publisher used by the gate.

## Why This Matters

Without a runtime boundary like Harn, application code tends to accumulate:

- provider-specific message/response parsing
- transcript compaction and summarization logic
- tool dispatch and retry behavior
- workflow branching and repair loops
- provenance, replay, and eval fixtures
- host/editor queue semantics

Harn moves those concerns into a typed runtime layer so a host app can stay
focused on:

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
      tools: coding_tools(),
      model_policy: {model_tier: "mid"},
      retry_policy: {max_attempts: 2}
    },
    verify: {
      kind: "verify",
      verify: {
        command: "cargo test --workspace --quiet",
        expect_status: 0,
        assert_text: "test result: ok"
      }
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

`verify` nodes can either run an explicit command as shown above or use an
agent/LLM mode when verification should stay provider-driven.

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
- [Hosted language spec](docs/src/language-spec.md)

## Development

```bash
make fmt
make lint
make test
make conformance
harn test conformance tests/worktree_runtime.harn
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
