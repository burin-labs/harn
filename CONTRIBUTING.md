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

Heavy setup phases are fingerprinted under `.codex/dev-setup/`, so rerunning
the script in an already prepared checkout is normally a fast no-op. Set
`HARN_DEV_SETUP_FORCE=1` to refresh every phase.

Codex app-managed worktrees use the tracked local environment config at
`.codex/environments/environment.toml`, which delegates to this same setup
script.
Claude Code uses the tracked project settings at `.claude/settings.json` to run
`scripts/claude-dev-setup-once.sh` on new sessions. The hook delegates to this
same setup script once per dependency fingerprint and writes logs under
`.claude/dev-setup/`.

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

### Stale bytecode cache

If tests fail in ways that don't match your edits — especially right after
switching branches or Rust versions — clear the on-disk Harn bytecode cache.
It keys on a compiler fingerprint, but bypassing it rules the cache out:

```bash
rm -rf ~/.cache/harn/bytecode   # or run with HARN_BYTECODE_CACHE=0 to bypass
```

### Test tiers

The workspace splits tests into two tiers and keeps pre-push targeted so local
pushes do not repeatedly pay for broad workspace compilation that CI already
runs:

**Fast suite**: `make test`

In-process, deterministic, zero subprocess-per-test. Wall-clock budget: <2 min
on a warm cache. Runs on every PR and when you opt into the broader local
pre-push gate with `HARN_HOOKS_FULL_LOCAL=1 HARN_PREPUSH_FULL_TESTS=1`. The nextest
`default` and `ci` profiles exclude the `harn-cli` integration test binaries
(those live in `crates/harn-cli/tests/` and spawn the compiled `harn` binary as
a subprocess).

**Slow E2E suite**: `make test-e2e`

Subprocess-spawning binary surface tests: CLI invocation, signal handling, MCP
server launch, real `ProcessHandle` smoke, orchestrator drain/replay, etc. Uses
the nextest `e2e` profile, which targets `package(harn-cli) and kind(test)`.
The target passes `--run-ignored all`, so tests marked `#[ignore]` because they
are slow binary-surface coverage still run in this suite.
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

Pre-commit hooks (`.githooks/pre-commit`) run cheap staged guards, markdown and
workflow lint when touched, and a read-only `cargo fmt --check`. Pre-push hooks
(`.githooks/pre-push`) enforce signed commits, merge-queue safety, and cheap
drift guards. Required CI owns compilation, tests, Harn formatting/linting,
generated mirrors, and portal lint. Set `HARN_HOOKS_FULL_LOCAL=1` to opt into
the targeted build-backed local gates; combine it with
`HARN_PREPUSH_FULL_TESTS=1` to run `make test` too. The full local portal gate
bootstraps dependencies through `./scripts/ensure_portal_deps.sh`; repo-root
`npm run portal:*` commands reuse the same bootstrap path.

Harn-authored checks run through `scripts/harn_bin.sh`, which reuses
`$HARN_BIN` or the worktree `target/debug/harn` when that binary is newer than
the Rust/Cargo inputs that relink the executable. If the binary is missing or
stale, the wrapper rebuilds it once and every Make/hook Harn command shares the
fresh path. To prewarm explicitly, run `scripts/ci_warm_harn_bin.sh`; to force a
fresh executable, remove `target/debug/harn` or unset `HARN_BIN`.

## Demo gate

Every PR that introduces a new public Harn primitive must also register
a `harn demo` scenario exercising it. The `Demo gate` job in the
[`PR gates` workflow](.github/workflows/pr-gates.yml) enforces this on
every `pull_request` event. The gate is diff-driven: it **auto-detects**
whether your change adds a watched primitive and passes on its own when
it doesn't, so most PRs need no action here and no label.

### What counts as "a new public primitive"

The detection logic lives in
[`.github/scripts/demo-gate.sh`](.github/scripts/demo-gate.sh) and
flags additions to:

- **Stdlib builtins** — `crates/harn-vm/src/stdlib/**/*.rs`: a new
  `#[harn_builtin]` annotation. See
  [Adding a stdlib builtin](#adding-a-stdlib-builtin) for the canonical
  proc-macro pattern.
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

You only need the `no-demo-needed` label when the gate **actually fires** —
i.e. the detector flagged a primitive addition you've decided not to demo
(a pure refactor that moves builtins between files, for example). For
hygiene-only PRs that don't touch a primitive surface at all, the gate
already passes on its own; adding the label is unnecessary noise.

When you do add it, the gate re-reads labels on `labeled`/`unlabeled`
events and flips green within a minute. The re-run no longer cancels other
in-flight checks, so a label change won't leave a stray "cancelled" status
on the PR.

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

## Adding a stdlib builtin

New stdlib builtins are registered with the `#[harn_builtin]` proc-macro
(crate: `harn-builtin-macros`). One annotation per Rust handler produces
both the runtime entry and the parser `BuiltinSignature`, so there is no
separate Rust-side signature table to keep in sync.

```rust
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Sync builtin: short signature in Harn syntax, return type after `->`.
#[harn_builtin(sig = "bytes_to_hex(input: bytes) -> string", category = "bytes")]
fn bytes_to_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // ... implementation returning Ok(VmValue::String(...)) ...
}

/// Async builtin: declare `kind = "async"` and write an `async fn`.
#[harn_builtin(
    sig = "with_autonomy_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_autonomy_policy_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    // ... await your async work, return Ok(...) ...
}

/// Aliases share the impl + signature; each emits its own parser entry.
#[harn_builtin(
    sig = "render(template: string, vars: dict?) -> string",
    aliases = ["render_prompt"],
    category = "strings"
)]
fn render_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // ... `render` and `render_prompt` both dispatch here ...
}

/// Runtime-only: registered on the VM but suppressed from the parser
/// signature set. Use for double-underscore internals (`__harn_*`) that
/// scripts must not call directly.
#[harn_builtin(
    sig = "__harn_with_execution_policy_override(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope",
    runtime_only = true
)]
async fn harn_with_execution_policy_override_impl(
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    // ... internal-only handler body ...
}

/// Each module collects its emitted `*_DEF` statics into a single slice
/// and exposes a small registrar that drains it. The slice is named
/// `MODULE_BUILTINS` by convention so `stdlib::all_builtin_defs()` can
/// concatenate every module's slice in one place.
pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &BYTES_TO_HEX_IMPL_DEF,
    // ... one entry per `#[harn_builtin]` fn in this file ...
];

pub(crate) fn register_bytes_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}
```

Wire-up checklist:

1. Annotate the handler with `#[harn_builtin(sig = "...", category = "...")]`.
   The proc-macro emits a sibling `static <FN_NAME_UPPER>_DEF: VmBuiltinDef`.
2. Append `&<FN_NAME_UPPER>_DEF` to the module's `MODULE_BUILTINS: &[&VmBuiltinDef]`.
3. Add `out.extend_from_slice(<module>::MODULE_BUILTINS);` to
   `stdlib::all_builtin_defs()` in
   [`crates/harn-vm/src/stdlib.rs`](crates/harn-vm/src/stdlib.rs) (keep
   the list alphabetical by module).
4. Make sure your module's `register_*_builtins(vm)` is called from one of
   `register_core_stdlib`, `register_io_stdlib`, or
   `register_agent_stdlib_{before,after}_llm` in the same file.
5. Add or update a `harn explain <name>` snapshot / conformance fixture
   if the builtin is publicly callable.

A handful of names (`len`, `split`, method-dispatched helpers, etc.) that
the harn-parser unit tests reference directly without installing the
macro slice still live as parser-only shadows in
[`crates/harn-parser/src/builtin_signatures/signatures/stdlib.rs`](crates/harn-parser/src/builtin_signatures/signatures/stdlib.rs).
Prefer `#[harn_builtin(parser_only = true)]` in the VM crate for new
parser-only entries; only touch the parser-side shadow when you're
adding a name the VM stdlib genuinely doesn't expose.

### Captured-state pattern (for builtins that need per-VM handles)

A few builtins need access to per-VM state (`MetadataState`,
`CheckpointState`, `pool` registry, etc.) that closures used to capture
directly. The proc-macro shape is a free fn, so those modules use a
`thread_local!` cell installed by the module's `register_<module>_builtins`
function and read inside the macro-emitted handler via
`with_state(fn_name, |state| { ... })`. See `crates/harn-vm/src/checkpoint.rs`
and `crates/harn-vm/src/metadata.rs` for canonical examples. The Harn VM
runs single-threaded per execution, so this matches the old
`Rc<RefCell<State>>` semantics 1:1.

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

Day to day: drop a `changelog.d/<id>.<category>.md` fragment per PR; see
[`changelog.d/README.md`](changelog.d/README.md) for the format and
[`AGENTS.md`](AGENTS.md) for the soft CI gate. Direct edits to
`## Unreleased` in `CHANGELOG.md` remain accepted (legacy path).

At a major (or 0.x-equivalent) cut, keep `CHANGELOG.md` focused on the
active release range by snipping the prior series into a versioned
archive under `changelog/archive/` such as
`changelog/archive/CHANGELOG-pre-0.8.md` (detailed per-patch notes for
v0.6.0 – v0.7.62) or `changelog/archive/CHANGELOG-pre-0.6.md` (condensed
pre-launch summaries) and leaving a link near the top of `CHANGELOG.md`. The
`harn-bump-fleet/lib/changelog.harn::changelog_archive_below_version`
helper produces the same split layout deterministically.

## Key references

- [Language spec](spec/HARN_SPEC.md): authoritative language specification
  (generated single-file assembly; edit the per-chapter sources in `spec/chapters/`)
- [AST docs](spec/AST.md): AST node types
- [Builtin reference](docs/src/builtins.md): all built-in functions
- [Language basics](docs/src/language-basics.md): syntax guide

## License

By contributing, you agree that your contributions will be licensed under the
same dual MIT/Apache-2.0 license as the project.
