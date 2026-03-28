# Contributing to Harn

Thanks for your interest in contributing to Harn! This guide covers the basics.

## Getting started

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
git config core.hooksPath .githooks
cargo build
```

## Running checks

Before submitting a PR, run the full check suite:

```bash
make all
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
automatically.

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
