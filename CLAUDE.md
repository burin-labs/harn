# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Harn?

Harn is a pipeline-oriented programming language for orchestrating AI coding agents. It features pipelines, first-class functions, pattern matching, enums, async/concurrency primitives (channels, mutexes, atomics), and LLM builtins.

## Build & Run Commands

```bash
# Build everything
cargo build

# Run a .harn file
cargo run -- run examples/hello.harn

# Run conformance test suite (the primary test mechanism)
cargo run -- test conformance

# Run Rust unit tests
cargo test

# Run tests for a specific crate
cargo test -p harn-runtime
cargo test -p harn-parser

# REPL
cargo run -- repl

# Build WASM target (excluded from workspace)
cd crates/harn-wasm && wasm-pack build
```

## Quality Commands

```bash
# Run all checks (format, lint, test, conformance)
make all

# Clippy lints (treats warnings as errors)
make lint

# Auto-format
make fmt

# Format check (CI mode, no changes)
make fmt-check
```

Always run `make lint` before committing — clippy warnings are treated as errors.

## Architecture

The execution pipeline is: **source → Lexer → Parser → TypeChecker → Interpreter**

### Workspace Crates

- **harn-lexer** — Tokenizer. `Lexer::new(source).tokenize()` produces a token stream. Token types in `token.rs`, scanning logic in `lexer.rs`.
- **harn-parser** — AST definition (`ast.rs`), recursive-descent parser (`parser.rs`), and static type checker (`typechecker.rs`).
- **harn-runtime** — Tree-walking async interpreter (`interpreter.rs`), value types (`value.rs`), scoped environments (`environment.rs`), error types (`error.rs`). The interpreter is `!Send` — must run inside `tokio::task::LocalSet`.
- **harn-stdlib** — Builtin functions registered on the interpreter: core I/O (`lib.rs`), JSON (`json.rs`), LLM calls (`llm.rs`), async builtins like `sleep`/`spawn`/channels (`async_builtins.rs`).
- **harn-vm** — Bytecode compiler and VM (alternative execution backend). Chunk format, compiler, and VM.
- **harn-cli** — CLI entry point. Subcommands: `run`, `test`, `repl`, `version`.
- **harn-lsp** — Language Server Protocol implementation.
- **harn-dap** — Debug Adapter Protocol implementation.
- **harn-wasm** — WASM build target (excluded from workspace, built separately with wasm-pack).

### Conformance Tests

Tests live in `conformance/interpreter/` and `conformance/errors/`. Each test is a `.harn` file paired with a `.expected` (for interpreter tests) or `.error` (for error tests) file. The CLI `test` command runs these by executing each `.harn` file and comparing output against the expected file. This is the primary way to verify language behavior.

### Language Spec

`spec/HARN_SPEC.md` is the authoritative language specification. `spec/AST.md` documents the AST node types. Consult these when making parser or interpreter changes.

### Tree-sitter Grammar

`tree-sitter-harn/grammar.js` defines the tree-sitter grammar used by the LSP for syntax highlighting.
