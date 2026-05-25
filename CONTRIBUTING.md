# Contributing to Harn

This guide covers the local setup, checks, and conventions for Harn contributors.

## Getting started

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
./scripts/dev_setup.sh
```

This script:

- configures `.githooks` as the repo hook path
- installs `cargo-nextest`, `sccache`, and `actionlint` when their toolchains are available
- enables the sccache rustc wrapper via a local `.cargo/config.toml`
- writes a per-worktree temp Cargo `target-dir` when `CODEX_WORKTREE_PATH` is
  set so parallel Codex worktrees stay isolated
- installs repo-local markdown tooling plus Node dependencies for the portal,
  `tree-sitter-harn/`, and `editors/vscode/` when `npm` is available
- builds `crates/harn-cli/portal-dist` when `npm` is available
- reuses the repo-root portal bootstrap path for `npm run portal:*` commands
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

**Fast suite**: `make test`

In-process, deterministic, zero subprocess-per-test. Wall-clock budget: <2 min
on a warm cache. Runs on every PR and when you opt into the broader local
pre-push gate with `HARN_PREPUSH_FULL_TESTS=1`. The nextest
`default` and `ci` profiles exclude the `harn-cli` integration test binaries
(those live in `crates/harn-cli/tests/` and spawn the compiled `harn` binary as
a subprocess).

**Slow E2E suite**: `make test-e2e`

Subprocess-spawning binary surface tests: CLI invocation, signal handling, MCP
server launch, real `ProcessHandle` smoke, orchestrator drain/replay, etc. Uses
the nextest `e2e` profile, which targets `package(harn-cli) and kind(test)`.
Runs on:

- Schedule: nightly at 3 AM UTC (`.github/workflows/e2e.yml`)
- `e2e` PR label: add the label to opt in before merge
- Merge queue: the `e2e` job in `ci.yml` runs before a PR lands

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

`make all` runs:

- `cargo fmt`: Rust formatting
- `harn fmt --check`: Harn file formatting
- `cargo clippy -- -D warnings`: lint, with warnings treated as errors
- `make lint-no-rust-prompt-prose`: prompt ownership ratchet
- `make lint-no-xfail-regression`: conformance xfail count ratchet
- `markdownlint-cli2`: Markdown lint
- `harn lint`: Harn linter on conformance tests
- `make test`: Rust workspace tests (`cargo nextest` when available)
- `harn test conformance`: conformance suite

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

## Demo gate

Every PR that introduces a new public Harn primitive must also register
a `harn demo` scenario exercising it. The
[`Demo gate` workflow](.github/workflows/demo-gate.yml) enforces this on
every `pull_request` event.

### What counts as "a new public primitive"

The detection logic lives in
[`.github/scripts/demo-gate.sh`](.github/scripts/demo-gate.sh) and
flags additions to:

- **Stdlib builtins** — `crates/harn-vm/src/stdlib/**/*.rs`: a new
  `vm.register_builtin(...)`, `SyncBuiltin::new(...)`,
  `async_builtin!(...)`, or `register_builtin_group(...)` call.
- **Host capabilities** — `crates/harn-vm/src/stdlib/host.rs` (a new
  `("capability", "operation") =>` arm in
  `dispatch_builtin_host_operation`) or
  `crates/harn-hostlib/src/**/*.rs` (a new
  `pub(super) const BUILTIN_*: &str = "hostlib_..."` constant).
- **Orchestrator surfaces** —
  `crates/harn-cli/src/commands/orchestrator/**/*.rs`: a new
  `pub fn` or `pub async fn`.
- **Language constructs** — `crates/harn-parser/src/parser/**/*.rs`: a
  new `fn parse_*` rule.

When the gate detects any of the above, the PR must also touch at least
one file under `crates/harn-cli/assets/demo/**` (a new scenario
directory, a tape addition, or a script extension).

Pure refactors that move existing builtins between files without adding
new ones still trip the additive-line detection — use the
[opt-out](#opting-out) below in that case.

### Authoring a demo scenario

The bundled scenarios under
[`crates/harn-cli/assets/demo/`](crates/harn-cli/assets/demo) are the
templates. Each scenario is a directory with two files:

- `scenario.harn` — the `.harn` script. Define a top-level
  `pipeline default(_task) { ... }` that exercises the primitive and
  emits a structured receipt on stdout. Use `__io_println(...)` for
  human-readable narration and `json_stringify(receipt)` for the
  machine-readable envelope a smoke test asserts on.
- `tape.jsonl` — the `--llm-mock` replay fixture. One JSONL record per
  expected LLM call: `{"match":"*pattern*","consume_match":true,"text":"...","model":"...","provider":"..."}`.
  For failover scenarios, an `"error":{"category":"rate_limit",...}`
  record stands in for an upstream error response.

Wire the scenario into the CLI by:

1. Adding a `Scenario { ... }` entry to the `SCENARIOS` const in
   [`crates/harn-cli/src/commands/demo.rs`](crates/harn-cli/src/commands/demo.rs)
   with `include_str!` references to both files.
2. Adding a `#[test]` in
   [`crates/harn-cli/tests/demo_cli.rs`](crates/harn-cli/tests/demo_cli.rs)
   that runs the scenario end-to-end and asserts on the receipt
   envelope and any per-task markers.

Examples to mirror:

- **Persona supervision + structured receipts**:
  [`merge-captain`](crates/harn-cli/assets/demo/merge-captain/scenario.harn).
- **Human-in-the-loop clarifying questions**:
  [`review-captain`](crates/harn-cli/assets/demo/review-captain/scenario.harn).
- **`parallel each` + cost attribution**:
  [`provider-race`](crates/harn-cli/assets/demo/provider-race/scenario.harn).
- **`routing_policy` failover + verifier escalation**:
  [`routing-policy`](crates/harn-cli/assets/demo/routing-policy/scenario.harn).

Verify locally before pushing:

```bash
cargo run --bin harn -- demo --list                # the new scenario should appear
cargo run --bin harn -- demo <id> --no-record      # offline-tape replay
cargo nextest run -p harn-cli --test demo_cli      # in-process smoke
```

### Opting out

If the PR is hygiene-only (formatting, dependency bumps, docs, generated
files) or a pure refactor that doesn't add a primitive surface, add the
`no-demo-needed` label. The workflow re-reads labels on
`labeled`/`unlabeled` events, so the gate flips green within a minute of
the label landing.

Use the opt-out sparingly. If you're unsure whether your PR introduces a
primitive, ship a demo — the cost of an extra scenario is much lower
than a primitive shipping into a release without a runnable example.

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
echo 'pipeline default() { log("hello") }' > conformance/tests/language/my_test.harn
echo 'hello' > conformance/tests/language/my_test.expected

# Run it
cargo run --bin harn -- test conformance --filter my_test

# Show timing without the verbose failure dump
cargo run --bin harn -- test conformance --timing --filter my_test
```

Tests live under `conformance/tests/` (passing) and `conformance/errors/`
(expected failures). The runner discovers `.harn` files recursively, so add
new tests to the feature-area subdirectory that best matches the behavior.

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

- Clippy warnings are treated as errors; fix all warnings before committing
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

- [Language spec](spec/HARN_SPEC.md): authoritative language specification
- [AST docs](spec/AST.md): AST node types
- [Builtin reference](docs/src/builtins.md): all built-in functions
- [Language basics](docs/src/language-basics.md): syntax guide

## License

By contributing, you agree that your contributions will be licensed under the
same dual MIT/Apache-2.0 license as the project.
