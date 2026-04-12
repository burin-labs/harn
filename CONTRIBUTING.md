# Contributing to Harn

Thanks for your interest in contributing to Harn! This guide covers the basics.

## Getting started

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
./scripts/dev_setup.sh
```

This script:

- configures `.githooks` as the repo hook path
- installs `cargo-nextest` and `sccache` (for faster tests and cached builds)
- enables the sccache rustc wrapper via a local `.cargo/config.toml`
- installs repo-local markdown tooling plus Node dependencies for
  `tree-sitter-harn/` and `editors/vscode/` when `npm` is available
- runs `cargo check --workspace`

## Running checks

Before submitting a PR, run the full check suite:

```bash
make all
```

### Warm vs cold expectations

On a modern workstation with a populated target/ cache you should see:

- `cargo check --workspace`: ~0.1–5 s warm, ~30–90 s cold
- `cargo test --workspace --lib`: ~0.1–0.5 s warm (after the initial build)
- `cargo clippy --workspace --all-targets -- -D warnings`: ~1–20 s warm
- `cargo run --bin harn -- test conformance`: ~7–15 s
- Full `make all`: ~60–120 s warm, ~3–5 min cold

What triggers a cold rebuild:

- Editing `Cargo.toml` at the workspace or crate root (profile flips,
  dependency changes, feature flag changes)
- Toolchain bump (`rustup update` that installs a new stable)
- `cargo clean`
- Running `cargo fmt` on `Cargo.toml` files (rare, but it does re-stamp)

On macOS, Spotlight may index freshly-linked test binaries on first run,
adding ~30–60 s of stat traffic unrelated to cargo.

### Optional: nextest

For bounded test timeouts (nothing can wedge the suite indefinitely) and
better parallelism, install `cargo-nextest`:

```bash
cargo install cargo-nextest --locked
make test-fast
```

`make test-fast` invokes `cargo nextest run --workspace` when nextest is
available and falls back to `cargo test --workspace` otherwise. The
workspace `.config/nextest.toml` applies a 15 s slow-test threshold by
default and a 60 s hard termination cap — tests that legitimately need
more time (the LLM transport tests) have targeted overrides.

Useful shortcuts:

```bash
make check     # alias for make all
make portal    # launch the local Harn observability portal
make setup     # rerun repo bootstrap
```

This runs:

- `cargo fmt` -- Rust formatting
- `harn fmt --check` -- Harn file formatting
- `cargo clippy -- -D warnings` -- Lint (warnings are errors)
- `markdownlint-cli2` -- Markdown lint
- `harn lint` -- Harn linter on conformance tests
- `cargo test` -- Rust unit tests
- `harn test conformance` -- Conformance test suite

Pre-commit hooks (`.githooks/pre-commit`) run fmt + clippy + markdown lint
automatically. Pre-push hooks (`.githooks/pre-push`) run workspace tests,
Harn formatting checks, and markdown lint before code leaves your machine.

## Project structure

| Crate | Purpose |
|---|---|
| `harn-lexer` | Tokenizer with span tracking |
| `harn-parser` | Recursive-descent parser, AST, type checker |
| `harn-vm` | Async bytecode compiler and virtual machine |
| `harn-fmt` | Code formatter |
| `harn-lint` | Linter (5 rules) |
| `harn-cli` | CLI entry point (run, test, repl, fmt, lint, init) |
| `harn-lsp` | Language Server Protocol implementation |
| `harn-dap` | Debug Adapter Protocol implementation |
| `harn-wasm` | WebAssembly target (built separately with wasm-pack) |

## Adding conformance tests

Conformance tests are the primary way to verify language behavior. Each test
is a `.harn` file paired with a `.expected` (output match) or `.error`
(error substring match) file.

```bash
# Add a new test
echo 'pipeline default() { println("hello") }' > conformance/tests/my_test.harn
echo 'hello' > conformance/tests/my_test.expected

# Run it
cargo run --bin harn -- test conformance --filter my_test

# Show timing without the verbose failure dump
cargo run --bin harn -- test conformance --timing --filter my_test
```

Tests live in `conformance/tests/` (passing) and `conformance/errors/`
(expected failures).

## Code style

- Clippy warnings are treated as errors -- fix all warnings before committing
- Harn files use 2-space indent (enforced by `harn fmt`)
- Rust files use standard `rustfmt` defaults
- Avoid adding comments unless the logic is non-obvious

## Key references

- [Language spec](spec/HARN_SPEC.md) -- authoritative language specification
- [AST docs](spec/AST.md) -- AST node types
- [Builtin reference](docs/builtins.md) -- all built-in functions
- [Language basics](docs/language-basics.md) -- syntax guide

## License

By contributing, you agree that your contributions will be licensed under the
same dual MIT/Apache-2.0 license as the project.
