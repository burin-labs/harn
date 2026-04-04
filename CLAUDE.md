# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

## Setup

```bash
make setup          # run scripts/dev_setup.sh for first-time setup
make install-hooks  # configure git to use .githooks/
```

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

# Portal (embedded web UI)
cargo run --bin harn -- portal
```

## Quality Gates

```bash
make fmt          # cargo fmt --all
make lint         # clippy with -D warnings
make test         # cargo test --workspace
make conformance  # run the conformance suite
make all          # fmt, then lint + test + conformance (use -j for parallel)
```

`make all` is the main release-quality aggregate check. It also runs
`make lint-md` (markdownlint), `make lint-harn` (harn check on conformance
tests), and `make fmt-harn` (harn fmt --check on conformance tests).

## Publishing

Use `scripts/publish.sh` for crate releases — it publishes all 8 crates
to crates.io in dependency order with rate-limit retry logic. Supports
`--dry-run` and `--allow-dirty` flags.

For the full maintainer ritual, prefer:

```bash
./scripts/release_gate.sh audit
./scripts/release_gate.sh full --bump patch --dry-run
```

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

Note: `harn-wasm` is excluded from the workspace and built separately
with `cd crates/harn-wasm && wasm-pack build`.

### Stdlib registration

Builtins are registered in three tiers in `crates/harn-vm/src/stdlib/stdlib.rs`:

- `register_core_stdlib()` — pure/deterministic (types, math, strings, JSON,
  datetime, regex, crypto, sets, shapes, testing)
- `register_io_stdlib()` — OS access (filesystem, process, logging, tracing)
- `register_agent_stdlib()` — network/async (concurrency, tools, agents,
  HTTP, LLM, MCP)

`stdlib_builtin_names()` creates a temporary VM, registers all builtins,
and extracts names (plus opcode-level keywords like `spawn`, `await`,
`cancel`). The linter and LSP consume this list — there is no separate
hardcoded name list.

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

## Conformance Tests

Tests live in `conformance/tests/` as paired `.harn` + `.expected` files.
The `.harn` file contains Harn source; the `.expected` file contains the
exact expected stdout (e.g. `[harn] 5`). Shared helpers live in
`conformance/tests/lib/`. Run a single test with:

```bash
cargo run --bin harn -- test conformance --filter <test_name>
```

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
