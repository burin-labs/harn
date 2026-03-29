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

# Run a .harn file
cargo run --bin harn -- run examples/hello.harn

# Run conformance test suite (the primary test mechanism)
cargo run --bin harn -- test conformance

# Run Rust unit tests
cargo test

# Run tests for a specific crate
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
# Run all checks (format, lint, test, conformance) — use -j for parallel
make all -j

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

Single execution backend: source -> Lexer -> Parser -> TypeChecker ->
Compiler -> VM (async bytecode, explicit call frames). True concurrency
via `tokio::task::spawn_local` for `parallel`, `parallel_map`, and
`spawn`/`await`/`cancel`.

### Workspace crates

- **harn-lexer** -- Tokenizer with span tracking (byte offsets + line/column).
  Token types in `token.rs`, scanning in `lexer.rs`.
- **harn-parser** -- AST definition (`ast.rs` with `SNode = Spanned<Node>`),
  recursive-descent parser (`parser.rs`), static type checker
  (`typechecker.rs`), diagnostic renderer (`diagnostic.rs`).
- **harn-vm** -- The sole execution engine. Modular structure:
  `value.rs` (VmValue, VmEnv, errors), `chunk.rs` (opcodes, bytecode),
  `compiler.rs` (AST -> bytecode, pipe placeholder desugaring),
  `vm.rs` (async execution loop), `stdlib.rs` (100+ builtin functions),
  `stdlib_modules.rs` (embedded std/text, std/collections .harn modules),
  `store.rs` (persistent key-value store backed by .harn/store.json),
  `metadata.rs` (project metadata store for `.burin/metadata/` shards),
  `http.rs` (HTTP client with retries),
  `llm.rs` (LLM calls for Anthropic/OpenAI/Ollama, agent_loop with
  tool support returning `{status, text, iterations, duration_ms}`),
  `mcp.rs` (MCP client: tools, resources, and prompts),
  `bridge.rs` / `bridge_builtins.rs` (JSON-RPC host delegation).
  45+ opcodes including TailCall, GetPropertyOpt, MethodCallOpt,
  Slice, concurrency, imports, enums, and deadlines. In bridge mode,
  unknown builtins are automatically delegated to the host via
  `builtin_call` JSON-RPC. Stdlib .harn files live in `stdlib/` at
  the repo root and are embedded via `include_str!`.
- **harn-fmt** -- AST-based code formatter. Canonical 2-space indent style.
- **harn-lint** -- Linter with 6 rules: unused-variable, unused-parameter,
  unreachable-code, mutable-never-reassigned, empty-block, shadow-variable.
- **harn-cli** -- CLI entry point. Subcommands: `run`, `test`, `repl`,
  `version`, `fmt`, `lint`, `init`, `acp`, `serve`.
  `acp.rs` (ACP JSON-RPC server with builtin delegation,
  `terminal/*` and `fs/*` support),
  `a2a.rs` (Agent-to-Agent HTTP server with Agent Card).
- **harn-lsp** -- Language Server Protocol implementation. Features:
  completion, hover, go-to-definition, references, rename, document
  symbols, workspace symbols, signature help, semantic tokens, code
  actions (quick-fix for lint warnings).
- **harn-dap** -- Debug Adapter Protocol implementation. Supports
  breakpoints (including conditional), stepping, variable inspection,
  expression evaluation (dot-access, subscripts, len/type_of),
  exception breakpoints.
- **harn-wasm** -- WASM target (excluded from workspace, built with
  wasm-pack). Contains its own minimal sync interpreter for browser use.

### Key design patterns

**AST spans**: All AST nodes are `SNode = Spanned<Node>` carrying source
`Span` with byte offsets and line/column. Errors include source location
for rustc-style diagnostic rendering.

**Gradual type system**: The typechecker in `typechecker.rs` uses
`InferredType = Option<TypeExpr>` -- `None` means unknown/untyped. Type
annotations are optional. Supports structural typing: dict literals
with string keys infer `Shape` types, enabling compile-time checking
of `{name: string, age: int}` shape annotations with width subtyping.
Also supports `list<T>`, `dict<K, V>`, union types, and type aliases.
The checker tracks enums for match exhaustiveness warnings.

**VM concurrency model**: The VM is async (runs inside a tokio
`LocalSet`). `spawn` creates real async tasks via
`tokio::task::spawn_local`, `await` joins them, `cancel` aborts them.
`parallel`/`parallel_map` fork child VMs for true concurrent execution.
Async builtins (HTTP, LLM, sleep, channels) are natively awaited in
the execution loop.

### agent_loop tool support

`agent_loop` returns a dict `{status, text, iterations, duration_ms,
tools_used}`. It supports tool execution via text-based `<tool_call>`
XML tags (default) or native function calling (`tool_format: "native"`).
Tools can be passed as string name lists (e.g. `["read", "search",
"edit"]`), `tool_registry` objects, or raw tool definition dicts.
Tool arguments are normalized before dispatch (`normalize_tool_args`),
and read-only tools (`read_file`, `list_directory`) are handled locally
in the VM via `handle_tool_locally` to reduce bridge latency.

In ACP mode, `register_agent_loop_with_bridge()` (in `llm.rs`) overrides
the native text-only `agent_loop` so that tool calls are executed via
host delegation through the bridge. This is wired up in `acp.rs` during
`execute_chunk`.

Built-in tool schemas are available for: `read_file`, `search`, `edit`,
`run`, `outline`, `web_search`, `web_fetch`, `lsp_hover`,
`lsp_definition`, `lsp_references`, `list_directory`.

### Conformance tests

Tests live in `conformance/tests/` and `conformance/errors/`. Each
test is a `.harn` file paired with a `.expected` or `.error` file. The
CLI `test` command executes each `.harn` file and compares trimmed output.
Error tests check that the expected error text is a substring of the
actual error. This is the primary way to verify language behavior.

To add a new conformance test, create both `name.harn` and `name.expected`
(or `name.error`) in the appropriate directory.

### Language spec

`spec/HARN_SPEC.md` is the authoritative language specification.
`spec/AST.md` documents AST node types. Consult these when making
parser or VM changes.

### Tree-sitter grammar

`tree-sitter-harn/grammar.js` defines the tree-sitter grammar used by
the LSP for syntax highlighting.
