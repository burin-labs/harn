# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## What is Harn?

Harn is a pipeline-oriented programming language for orchestrating AI coding
agents. It features pipelines, first-class functions, pattern matching, enums,
async/concurrency primitives (channels, mutexes, atomics), and LLM builtins.

## Build & run commands

```bash
# Build everything
cargo build

# Run a .harn file (interpreter, default)
cargo run --bin harn -- run examples/hello.harn

# Run via bytecode VM backend
cargo run --bin harn -- run --vm examples/hello.harn

# Run conformance test suite (the primary test mechanism)
cargo run --bin harn -- test conformance

# Run Rust unit tests
cargo test

# Run tests for a specific crate
cargo test -p harn-runtime
cargo test -p harn-parser
cargo test -p harn-lint

# REPL
cargo run --bin harn -- repl

# Format a .harn file
cargo run --bin harn -- fmt examples/hello.harn
cargo run --bin harn -- fmt --check examples/hello.harn

# Lint a .harn file
cargo run --bin harn -- lint examples/hello.harn

# Build WASM target (excluded from workspace)
cd crates/harn-wasm && wasm-pack build
```

## Quality commands

```bash
# Run all checks (format, lint, test, conformance)
make all

# Clippy lints (treats warnings as errors)
make lint

# Markdown lint
make lint-md

# Auto-format
make fmt

# Format check (CI mode, no changes)
make fmt-check
```

Always run `make lint` before committing — clippy warnings are treated
as errors. Pre-commit hooks run fmt + clippy + markdown lint automatically.

## Architecture

Two execution backends:

- **Interpreter** (default): source → Lexer → Parser → TypeChecker →
  Interpreter (async, tree-walking)
- **VM** (`--vm` flag): source → Lexer → Parser → TypeChecker → Compiler →
  VM (bytecode, explicit call frames)

The interpreter handles all features including async (spawn, parallel,
LLM calls). The VM handles sync features and is being extended.

### Workspace crates

- **harn-lexer** — Tokenizer with span tracking (byte offsets + line/column).
  Token types in `token.rs`, scanning in `lexer.rs`.
- **harn-parser** — AST definition (`ast.rs` with `SNode = Spanned<Node>`),
  recursive-descent parser (`parser.rs`), static type checker
  (`typechecker.rs`), diagnostic renderer (`diagnostic.rs`).
- **harn-runtime** — Tree-walking async interpreter (`interpreter.rs`),
  value types (`value.rs`), scoped environments (`environment.rs`),
  error types with spans and suggestions (`error.rs`). The interpreter
  is `!Send` — runs inside `tokio::task::LocalSet`.
- **harn-stdlib** — Builtin functions: core I/O (`lib.rs`), JSON (`json.rs`),
  LLM calls (`llm.rs`), async builtins (`async_builtins.rs`).
- **harn-vm** — Bytecode compiler and VM. Explicit call frame stack,
  exception handler stack for try/catch/throw, 30+ opcodes.
- **harn-fmt** — AST-based code formatter. Canonical 2-space indent style.
- **harn-lint** — Linter with 5 rules: unused-variable, unreachable-code,
  mutable-never-reassigned, empty-block, shadow-variable.
- **harn-cli** — CLI entry point. Subcommands: `run`, `test`, `repl`,
  `version`, `fmt`, `lint`.
- **harn-lsp** — Language Server Protocol implementation.
- **harn-dap** — Debug Adapter Protocol implementation.
- **harn-wasm** — WASM target (excluded from workspace, built with
  wasm-pack).

### AST spans

All AST nodes are `SNode = Spanned<Node>` carrying source `Span` with
byte offsets and line/column. Errors include source location for
rustc-style diagnostic rendering.

### Conformance tests

Tests live in `conformance/interpreter/` and `conformance/errors/`. Each
test is a `.harn` file paired with a `.expected` or `.error` file. The
CLI `test` command executes each `.harn` file and compares output. This
is the primary way to verify language behavior.

### Language spec

`spec/HARN_SPEC.md` is the authoritative language specification.
`spec/AST.md` documents AST node types. Consult these when making
parser or interpreter changes.

### Tree-sitter grammar

`tree-sitter-harn/grammar.js` defines the tree-sitter grammar used by
the LSP for syntax highlighting.
