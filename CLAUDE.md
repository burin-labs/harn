# CLAUDE.md

This repository implements Harn, the agent harness language and runtime.

## What Harn Is

Harn is a language plus runtime for orchestrating coding agents. The key
design goal is to keep product applications thin: host apps should mainly
declare capabilities, workflow templates, policy ceilings, and UI/session
hooks, while Harn owns orchestration behavior.

That means the runtime now includes:

- LLM calls and persistent agent loops
- typed workflow graphs and workflow execution
- typed artifacts/resources for context assembly
- transcript lifecycle management and transcript event normalization
- run records, replay/eval surfaces, and provenance
- ACP/bridge host integration with queued human-message delivery policies
- capability-ceiling enforcement for nested orchestration

## Core Commands

```bash
# Build
cargo build

# Run a file
cargo run --bin harn -- run examples/hello.harn

# Run the conformance suite
cargo run --bin harn -- test conformance

# Run a targeted conformance case
cargo run --bin harn -- test conformance --filter workflow_runtime

# Rust tests
cargo test --workspace

# Type-check / lint / format
cargo run --bin harn -- check examples/hello.harn
cargo run --bin harn -- lint examples/hello.harn
cargo run --bin harn -- fmt --check examples/hello.harn

# ACP
cargo run --bin harn -- acp

# Run records
cargo run --bin harn -- runs inspect .harn-runs/<run>.json
cargo run --bin harn -- replay .harn-runs/<run>.json
cargo run --bin harn -- eval .harn-runs/<run>.json
```

## Quality Gates

```bash
make fmt
make lint
make test
make conformance
make all
```

`make lint` runs clippy with warnings denied. `make all` is the main
release-quality aggregate check.

## Architecture

Execution pipeline:

```text
source -> Lexer -> Parser -> TypeChecker -> Compiler -> VM
```

### Workspace crates

- `harn-lexer`: tokenization and span tracking.
- `harn-parser`: AST, parser, type checker, diagnostics.
- `harn-vm`: bytecode compiler, interpreter, stdlib, LLM providers,
  transcripts, orchestration runtime, ACP/MCP runtime integration.
- `harn-cli`: command-line interface, conformance runner, ACP server,
  A2A server, run-record inspection/replay/eval.
- `harn-lint`: static linting. Builtin names come from the VM, so linter
  builtin awareness stays aligned with runtime registration.
- `harn-fmt`: formatter.
- `harn-lsp`: language server.
- `harn-dap`: debugger.
- `tree-sitter-harn`: syntax grammar for editor integrations.

### Runtime modules worth knowing

- `crates/harn-vm/src/llm/`
  provider clients, response normalization, transcript helpers,
  agent loops, tool handling, replay fixtures.
- `crates/harn-vm/src/orchestration.rs`
  workflow graphs, artifact records, capability policies,
  run records, validation, execution helpers.
- `crates/harn-vm/src/stdlib/agents.rs`
  Harn-facing orchestration builtins.
- `crates/harn-vm/src/bridge.rs`
  host bridge, JSON-RPC integration, queued user messages.
- `crates/harn-vm/src/llm/conversation.rs`
  transcript lifecycle builtins.

## Alignment Rules

When changing Harn, check which layers can drift:

- Parser / lexer / tree-sitter only need changes for syntax changes.
- Interpreter / stdlib / CLI / docs must change for builtin or runtime changes.
- Linter builtin awareness comes from `harn_vm::stdlib::stdlib_builtin_names()`.
- Conformance tests are the primary executable spec for language/runtime behavior.

If you add or remove public builtins or commands, update:

- `README.md`
- `docs/src/*`
- `CHANGELOG.md`
- CLI help in `crates/harn-cli/src/main.rs`
- conformance coverage when appropriate

## Workflow And Transcript Model

Preferred public surface:

- `workflow_graph(...)`
- `workflow_validate(...)`
- `workflow_execute(...)`
- `artifact(...)`, `artifact_derive(...)`, `artifact_select(...)`
- `run_record_*`
- `transcript_*`

Compatibility helpers may remain, but avoid adding narrow wrappers when the
typed runtime surface already expresses the concept.

## Host / ACP Notes

ACP hosts can inject queued user messages during agent execution using
`user_message`, `session/input`, or `agent/user_message` notifications with
a `mode`:

- `interrupt_immediate`
- `finish_step`
- `wait_for_completion`

Harn is responsible for interpreting those delivery semantics inside the
agent loop, not the product host.

## Spec And Docs

- `spec/HARN_SPEC.md` is the language spec.
- `spec/AST.md` describes AST shapes.
- `docs/src/` is the mdBook source.
- `docs/dist/` is the generated site output when rebuilt.
