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
- writes a per-worktree temp Cargo `target-dir` when `CODEX_WORKTREE_PATH` is
  set so parallel Codex worktrees stay isolated
- installs repo-local markdown tooling plus Node dependencies for the portal,
  `tree-sitter-harn/`, and `editors/vscode/` when `npm` is available
- builds `crates/harn-cli/portal-dist` when `npm` is available
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

### Test tiers

The workspace splits tests into two tiers and keeps pre-push targeted so local
pushes do not repeatedly pay for broad workspace compilation that CI already
runs:

**Fast suite** — `make test`

In-process, deterministic, zero subprocess-per-test. Wall-clock budget: <2 min
on a warm cache. Runs on every PR and when you opt into the broader local
pre-push gate with `HARN_PREPUSH_FULL_TESTS=1`. The nextest
`default` and `ci` profiles exclude the `harn-cli` integration test binaries
(those live in `crates/harn-cli/tests/` and spawn the compiled `harn` binary as
a subprocess).

**Slow E2E suite** — `make test-e2e`

Subprocess-spawning binary surface tests: CLI invocation, signal handling, MCP
server launch, real `ProcessHandle` smoke, orchestrator drain/replay, etc. Uses
the nextest `e2e` profile, which targets `package(harn-cli) and kind(test)`.
Runs on:

- Schedule — nightly at 3 AM UTC (`.github/workflows/e2e.yml`)
- `e2e` PR label — add the label to opt in before merge
- Merge-queue — the `e2e` job in `ci.yml` runs before a PR lands

### Preferred Rust test path

`make test` is the default Rust workspace test entry point. When
`cargo-nextest` is installed, it runs `cargo nextest run --workspace` for
better cross-binary parallelism and bounded timeouts. When nextest is not
installed, it falls back to `cargo test --workspace`.

`make setup` already installs `cargo-nextest`; if you need to add it
manually:

```bash
cargo install cargo-nextest --locked
make test       # fast suite
make test-e2e   # slow E2E suite (requires nextest)
```

The workspace `.config/nextest.toml` applies a 15 s slow-test threshold by
default and a 60 s hard termination cap. Tests that legitimately need more
time (the LLM transport tests, E2E subprocess tests) have targeted overrides.

If you need the baseline Cargo behavior explicitly, use:

```bash
make test-cargo
```

Useful shortcuts:

```bash
make check       # alias for make all
make bench-vm    # opt-in interpreter microbenchmark suite
make portal      # launch the local Harn observability portal
make setup       # rerun repo bootstrap
make test-cargo  # force plain cargo test --workspace
make test-e2e    # run slow E2E / smoke suite
```

This runs:

- `cargo fmt` -- Rust formatting
- `harn fmt --check` -- Harn file formatting
- `cargo clippy -- -D warnings` -- Lint (warnings are errors)
- `make lint-no-rust-prompt-prose` -- prompt ownership ratchet
- `make lint-no-xfail-regression` -- conformance xfail count ratchet
- `markdownlint-cli2` -- Markdown lint
- `harn lint` -- Harn linter on conformance tests
- `make test` -- Rust workspace tests (`cargo nextest` when available)
- `harn test conformance` -- Conformance test suite

Model-facing prompt prose lives in `crates/harn-stdlib/src/stdlib/**/*.harn.prompt`.
Rust may carry only short diagnostics, tests, parser/protocol constants, and
provider API strings.

## Interpreter microbenchmarks

The VM microbenchmark suite is opt-in and is not part of `make all`. It is
intended for before/after measurements when changing interpreter behavior,
opcode handlers, or stdlib collection dispatch:

```bash
make bench-vm
```

The target runs deterministic fixtures under `perf/vm/` in release mode using
the existing `harn bench` command. For repeatable local comparisons, run it a
few times on the same machine with the same iteration count and compare the
average wall time values:

```bash
./scripts/bench_vm.sh --iterations 20 --baseline perf/vm/BASELINE.md
```

Local CPU load and thermal state can move results by several percent, so treat
small differences as noise unless they reproduce consistently. When running
benchmarks from multiple worktrees, set a per-run `CARGO_TARGET_DIR` to avoid
build contention.

Pre-commit hooks (`.githooks/pre-commit`) run fmt + clippy + highlight keyword
regeneration + markdown lint automatically. Pre-push hooks
(`.githooks/pre-push`) run targeted package `cargo check --tests` for changed
crates, affected Harn formatting/lint checks, generated-file drift checks, and
markdown/actionlint/portal lint when touched. Set `HARN_PREPUSH_FULL_TESTS=1`
to run `make test` from pre-push before code leaves your machine. Both hooks
bootstrap the portal frontend dependencies through
`./scripts/ensure_portal_deps.sh` before running portal lint, and the repo-root
`npm run portal:*` commands reuse the same bootstrap path.

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
# Add a new test (pick the subdirectory that matches the feature area, e.g. language/, stdlib/)
echo 'pipeline default() { println("hello") }' > conformance/tests/language/my_test.harn
echo 'hello' > conformance/tests/language/my_test.expected

# Run it
cargo run --bin harn -- test conformance --filter my_test

# Show timing without the verbose failure dump
cargo run --bin harn -- test conformance --timing --filter my_test
```

Tests live under `conformance/tests/` (passing) and `conformance/errors/`
(expected failures), each organized into feature-area subdirectories — the
runner discovers `.harn` files recursively, so just drop new tests into
the subdirectory that best matches their area.

## Writing tests

Wall-clock waits are banned in test files. Do not use `std::thread::sleep`,
`tokio::time::sleep` (outside a `start_paused = true` test), `Instant::now()`
polling loops, `SystemTime::now()`, or short `recv_timeout` calls. The
`make lint-test-patterns` step in CI enforces this.

Use the approved alternatives instead:

- `tokio::time::pause()` + `advance()` for simulating time in async tests
- `EventLog::subscribe()` + `tokio::time::timeout` for waiting on events
- `OrchestratorHarness` for orchestrator tests that do not need real subprocesses

See [`docs/src/dev/testing.md`](docs/src/dev/testing.md) for detailed guidance,
common pitfalls, and the opt-out procedure for cases where a wall-clock wait is
genuinely unavoidable.

## Code style

- Clippy warnings are treated as errors -- fix all warnings before committing
- Harn files use 2-space indent (enforced by `harn fmt`)
- Rust files use standard `rustfmt` defaults
- Avoid adding comments unless the logic is non-obvious

## Maintaining the changelog

Keep `CHANGELOG.md` focused on the actively maintained release range. When a
future major-version cut makes older release headers mostly archaeological,
move the already-condensed older entries into a versioned archive such as
`CHANGELOG-pre-0.6.md` and leave a one-line link near the top of
`CHANGELOG.md`.

## Key references

- [Language spec](spec/HARN_SPEC.md) -- authoritative language specification
- [AST docs](spec/AST.md) -- AST node types
- [Builtin reference](docs/builtins.md) -- all built-in functions
- [Language basics](docs/language-basics.md) -- syntax guide

## License

By contributing, you agree that your contributions will be licensed under the
same dual MIT/Apache-2.0 license as the project.
