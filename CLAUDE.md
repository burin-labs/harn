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
cargo test -p harn-vm

# Run a single Rust test by name
cargo test -p harn-vm test_parallel_basic

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
as errors. Pre-commit hooks (`.githooks/pre-commit`) run fmt + clippy +
markdown lint automatically. Set up with: `git config core.hooksPath .githooks`

## Architecture

Two execution backends:

- **Interpreter** (default): source → Lexer → Parser → TypeChecker →
  Interpreter (async, tree-walking). Full feature support including
  true concurrency via `tokio::task::spawn_local`.
- **VM** (`--vm` flag): source → Lexer → Parser → TypeChecker → Compiler →
  VM (bytecode, explicit call frames). Concurrency features execute
  sequentially (spawn defers to await, parallel loops run in sequence).

The interpreter is `!Send` — runs inside `tokio::task::LocalSet`.

### Workspace crates

- **harn-lexer** — Tokenizer with span tracking (byte offsets + line/column).
  Token types in `token.rs`, scanning in `lexer.rs`.
- **harn-parser** — AST definition (`ast.rs` with `SNode = Spanned<Node>`),
  recursive-descent parser (`parser.rs`), static type checker
  (`typechecker.rs`), diagnostic renderer (`diagnostic.rs`).
- **harn-runtime** — Tree-walking async interpreter (`interpreter.rs`),
  value types (`value.rs`), scoped environments (`environment.rs`),
  error types with spans and suggestions (`error.rs`).
- **harn-stdlib** — Builtin functions: core I/O (`lib.rs`), JSON (`json.rs`),
  LLM calls (`llm.rs`), async builtins (`async_builtins.rs`),
  HTTP client with retries (`http.rs`), structured logging (`logging.rs`),
  tool registry with JSON Schema support (`tools.rs`).
- **harn-vm** — Bytecode compiler (`compiler.rs`), chunk/opcode definitions
  (`chunk.rs`), and stack-based VM (`vm.rs`). 35+ opcodes including
  concurrency (Parallel, ParallelMap, Spawn) and deadline enforcement
  (DeadlineSetup, DeadlineEnd).
- **harn-fmt** — AST-based code formatter. Canonical 2-space indent style.
- **harn-lint** — Linter with 5 rules: unused-variable, unreachable-code,
  mutable-never-reassigned, empty-block, shadow-variable.
- **harn-cli** — CLI entry point. Subcommands: `run`, `test`, `repl`,
  `version`, `fmt`, `lint`, `init`.
- **harn-lsp** — Language Server Protocol implementation.
- **harn-dap** — Debug Adapter Protocol implementation.
- **harn-wasm** — WASM target (excluded from workspace, built with
  wasm-pack).

### Key design patterns

**AST spans**: All AST nodes are `SNode = Spanned<Node>` carrying source
`Span` with byte offsets and line/column. Errors include source location
for rustc-style diagnostic rendering.

**Gradual type system**: The typechecker in `typechecker.rs` uses
`InferredType = Option<TypeExpr>` — `None` means unknown/untyped. Type
annotations are optional. The checker tracks enums for match
exhaustiveness warnings and infers types through enum constructs and
property access.

**VM concurrency model**: The VM is synchronous. `spawn` stores closures
for deferred execution — `await` executes them, `cancel` drops them
without running. `parallel`/`parallel_map` execute closure iterations
sequentially. This matches interpreter semantics for cancel but not
for true parallelism.

### Conformance tests

Tests live in `conformance/interpreter/` and `conformance/errors/`. Each
test is a `.harn` file paired with a `.expected` or `.error` file. The
CLI `test` command executes each `.harn` file and compares trimmed output.
Error tests check that the expected error text is a substring of the
actual error. This is the primary way to verify language behavior.

To add a new conformance test, create both `name.harn` and `name.expected`
(or `name.error`) in the appropriate directory.

### Language spec

`spec/HARN_SPEC.md` is the authoritative language specification.
`spec/AST.md` documents AST node types. Consult these when making
parser or interpreter changes.

### Tree-sitter grammar

`tree-sitter-harn/grammar.js` defines the tree-sitter grammar used by
the LSP for syntax highlighting.
