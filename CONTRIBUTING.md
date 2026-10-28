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
- writes a private Cargo `target-dir` under the developer cache for each
  worktree so parallel builds stay isolated
- compiles Harn's own crates with rustc's parallel front end (`-Zthreads=8`)
  through a workspace rustc wrapper, which roughly halves `harn-vm`'s compile;
  dependencies compile as on stable, CI applies the same wrapper through its
  Rust toolchain action, and `HARN_RUSTC_FRONTEND_THREADS=1` turns it off
- installs repo-local markdown tooling plus Node dependencies for the portal,
  `tree-sitter-harn/`, and `editors/vscode/` when `npm` is available
- builds `crates/harn-cli/portal-dist` when `npm` is available
- reuses the repo-root portal bootstrap path for `npm run portal:*` commands
- builds and verifies the canonical Harn CLI, then publishes a reusable
  dependency seed

Heavy setup phases are fingerprinted under `.codex/dev-setup/`, so rerunning
the script in an already prepared checkout is normally a fast no-op. Set
`HARN_DEV_SETUP_FORCE=1` to refresh every phase.

To refresh an existing source-addressed compiler-cache wrapper after pulling
wrapper changes, run `make refresh-cargo-wrapper`. This builds the wrapper
from the current checkout and updates only its existing Cargo configuration
entry. Hook settings, target paths, cache policy and dependency seeds stay
unchanged; missing or unmanaged configuration requires separate setup.

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

The first setup may compile the full CLI dependency graph. It allows up to
one hour for that build; ordinary binary-resolution commands retain their
ten-minute default. Set `HARN_BIN_CARGO_TIMEOUT_SECONDS` to override either
budget. Waiting for the shared Rust build lane is accounted for separately
from compilation.

Later builds reuse the worktree's artifacts. Fresh worktrees can restore a
compatible dependency seed, while compiler objects are shared through sccache.
Toolchain, dependency, feature, and source changes can invalidate that reuse.
Use the [testing guide](docs/src/dev/testing.md) to select the smallest check
that proves your change.

### Shared target cleanup

`scripts/prune_stale_targets.sh --host-maintenance` applies the shared target
retention and byte limits. Add `--dry-run` to inspect its decisions first.
Repeat `--protect-entry NAME` to reserve several exact entry basenames; quote
names containing spaces. Protections apply to both managed cache families and
outrank `--remove-entry NAME`, rank, age, and size. `HARN_TARGET_GC_PROTECT`
also reserves the calling worktree's target path or basename. Live processes
always survive. Retained protected bytes still count toward the ceiling, so
the summary reports `status=partial` if those entries leave a root oversized.
Named removals blocked by protection or a live process also report a partial
status, a pending count, and the affected names.

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

- Every push to `main` (`.github/workflows/e2e.yml`), on a self-hosted big
  runner when the fleet has one online, and on a hosted runner otherwise
- Schedule: nightly at 3 AM UTC (`.github/workflows/e2e.yml`)
- `e2e` PR label: add the label to opt in before merge
- Merge queue: the `e2e` job in `ci.yml` runs before a PR lands

A failing unattended run (main push or nightly) opens a deduplicated tracking
issue, so main is never called green while its last executed slow run is red.

### Preferred Rust test path

`make test` is the default Rust workspace test entry point. It requires
`cargo-nextest` and runs `cargo nextest run --workspace` so the fast suite
keeps nextest's process-per-test isolation, profile filters, and bounded
timeouts. It does **not** fall back to `cargo test`: that path has no profile
support (so it silently widens into the E2E tier) and runs tests as threads in
one process (so process-local env mutations race). See
[Deterministic test patterns](docs/src/dev/testing.md#supported-runner).

`make setup` already installs `cargo-nextest`; if you need to add it
manually:

```bash
cargo install cargo-nextest --locked
make test       # fast suite (requires nextest)
make test-e2e   # slow E2E suite (requires nextest)
```

The workspace `.config/nextest.toml` applies a 15 s slow-test threshold by
default and a 60 s hard termination cap. Tests that legitimately need more
time (the LLM transport tests, E2E subprocess tests) have targeted overrides.

If you intentionally need the baseline Cargo behavior (different isolation,
no profile filter), use:

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
- `make test`: Rust workspace tests (`cargo nextest`; required)
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

The target runs deterministic fixtures under `bench/vm/` in release mode using
the existing `harn bench` command. For repeatable local comparisons, run it a
few times on the same machine with the same iteration count and compare the
average wall time values:

```bash
./scripts/bench_vm.sh --iterations 20 --baseline bench/vm/BASELINE.md
```

Local CPU load and thermal state can move results by several percent, so treat
small differences as noise unless they reproduce consistently. When running
benchmarks from multiple worktrees, set a per-run `CARGO_TARGET_DIR` to avoid
build contention.

Pre-commit hooks (`.githooks/pre-commit`) run cheap staged guards, markdown and
workflow lint when touched, and a read-only `cargo fmt --check`. Pre-push hooks
(`.githooks/pre-push`) enforce signed commits, merge-queue safety, and cheap
drift guards. Those drift guards run whichever `harn` your `PATH` resolves, not
one built from the tree you are pushing, so an older `harn` can fail them for a
reason your branch did not cause: an unresolved import of a stdlib module that
only the newer binary ships, reported as though your diff were stale. If a
pre-push guard blames a file you did not touch, re-run the push with `HARN_BIN`
set to the binary `./scripts/harn_bin.sh --print` names. Required CI owns
compilation, tests, Harn formatting/linting, generated mirrors, and portal
lint. Set `HARN_HOOKS_FULL_LOCAL=1` to opt into
the targeted build-backed local gates; combine it with
`HARN_PREPUSH_FULL_TESTS=1` to run `make test` too. The full local portal gate
bootstraps dependencies through `./scripts/ensure_portal_deps.sh`; repo-root
`npm run portal:*` commands reuse the same bootstrap path.

Rust formatting is package-scoped when every staged Rust/Cargo path has a
workspace-package owner. Root manifests and unmatched repository topology fall
back to `cargo fmt --all`; Cargo remains the owner so malformed fixtures and
generated/seed `.rs` files outside its target set are not linted accidentally.

Hooks append profile-tagged timing rows to `~/.burin/hook-timings.ndjson`.
Inspect invocations with
`harn run --no-sandbox scripts/hook_timing_summary.harn`; add `-- --budgets` to
emit sample-gated (`n >= 4`) `p95*1.2` budget observations for the shared fleet
budget registry. Failed, legacy, and full opt-in hook runs never inflate the
fast-hook budget. Pre-commit rows report Rust formatting separately from the
remaining fast validators so slow-tail regressions retain an actionable owner.

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

- **Stdlib builtins** — `crates/harn-vm/src/stdlib/**/*.rs`,
  `crates/harn-builtins/src/**/*.rs`, or `crates/harn-flow/src/**/*.rs`: a new
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
   [`crates/harn-cli/tests/harn_cli_fast/demo_cli.rs`](crates/harn-cli/tests/harn_cli_fast/demo_cli.rs)
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
| `harn-kernel` | Canonical bytecode compiler, portable artifact, and deterministic execution kernel |
| `harn-vm` | Hostful async VM adapter and standard-library integration |
| `harn-fmt` | Code formatter |
| `harn-lint` | Linter (5 rules) |
| `harn-cli` | CLI entry point (run, test, repl, fmt, lint, init) |
| `harn-lsp` | Language Server Protocol implementation |
| `harn-dap` | Debug Adapter Protocol implementation |
| `harn-wasm` | Workspace-owned browser adapter (packaged with `make wasm-build`) |

## Adding a stdlib builtin

New stdlib builtins are registered with the `#[harn_builtin]` proc-macro
(crate: `harn-builtin-macros`). One annotation per Rust handler produces
both the runtime entry and the parser `BuiltinSignature`, so there is no
separate Rust-side signature table to keep in sync.

A new builtin family goes in its own module at the root of
`crates/harn-builtins/src/`, which compiles beside `harn-vm` instead of inside
it. Keep a family in `crates/harn-vm/src/stdlib/` only when `harn-vm` itself
calls into it, its runtime drives it (the trigger dispatcher, connectors, the
embedded agent stdlib), or it belongs to the language prelude. Binaries reach
every family through `harn-runtime`, so a new family needs no registration
outside its own module.

Declare how scripts can reach the builtin with `exposure`. Pure and internal
builtins imply empty effects. Harness methods and privileged wire calls require
an explicit `effects` list. The macro rejects effects on pure builtins.

```rust
use harn_vm::stdlib::macros::harn_builtin;
use harn_vm_core::value::{VmError, VmValue};

/// Sync builtin: short signature in Harn syntax, return type after `->`.
#[harn_builtin(exposure = "pure", sig = "bytes_to_hex(input: bytes) -> string", category = "bytes")]
fn bytes_to_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // ... implementation returning Ok(VmValue::String(...)) ...
}

/// Async builtin: declare `kind = "async"` and write an `async fn`.
#[harn_builtin(
    exposure = "harness.runtime.with_autonomy_policy",
    effects = ["state.mutate@const=runtime-policy"],
    sig = "with_autonomy_policy(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope"
)]
async fn with_autonomy_policy_impl(
    ctx: harn_vm_core::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    // ... await your async work, return Ok(...) ...
}

/// Aliases share the impl + signature; each emits its own parser entry.
#[harn_builtin(
    exposure = "pure",
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
    exposure = "runtime_internal",
    sig = "__harn_with_execution_policy_override(policy: dict, fn: closure) -> any",
    kind = "async",
    category = "runtime_scope",
    runtime_only = true
)]
async fn harn_with_execution_policy_override_impl(
    ctx: harn_vm_core::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    // ... internal-only handler body ...
}

// Installs every `#[harn_builtin]` declared in this module, in the `Pure`
// tier of ordered runtime initialization.
harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_bytes_builtins);
```

### Reading arguments

Turn `&[VmValue]` into typed Rust values through `stdlib::args`, not by
matching on `VmValue` at the call site. `Args` reads positional arguments
and `Options` reads the trailing option-bag dict:

```rust
use crate::stdlib::args::Args;

fn jwt_sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let sign = Args::new("jwt_sign", args);
    let algorithm = sign.string(0, "alg")?;        // &str, borrowed
    let claims = sign.dict(1, "claims")?;          // &DictMap
    let mut options = sign.options(2, "options")?; // empty when absent
    let kid = options.opt_string("kid")?;
    options.finish(&[])?;                          // reject unknown options
    // ...
}
```

Use the argument names from the builtin's own `sig`, so a diagnostic and
the documented signature agree. Pick the constructor that matches how the
builtin fails: `Args::new` for a wrong type that should bubble,
`Args::runtime` for a runtime error, and `Args::thrown` when scripts are
expected to `try` / `recover` the failure.

The reader owns the wording, so builtins do not write "must be a …"
messages themselves. When a message must name a type anyway, spell it the
way `type_of` does — `int`, not `integer`; `bool`, not `boolean`; `dict`,
not `record` or `object`. A test in `stdlib::args::drift_tests` walks the
stdlib source and fails on the other spellings; a message genuinely about
a JSON document or a file name opts out with a `// not-a-harn-type:`
comment.

`Args` and `Options` carry the accessors the stdlib uses today. Add the
one you need — the pattern is a few lines, and `Expected` composes the
type names from `TypeTag` so a new accessor cannot invent a spelling.

Wire-up checklist:

1. Annotate the handler with `#[harn_builtin(sig = "...", category = "...")]`.
   The proc-macro emits a sibling `static <FN_NAME_UPPER>_DEF: VmBuiltinDef`.
2. For a new module, install its declarations with `register_module_builtins`.
   It selects exactly that declaring module, without descendants. Composite
   installers call their child installers explicitly; stateful installers keep
   their initialization alongside registration.
3. The macro contributes definitions to `all_builtin_defs()` automatically.
   Do not add a parallel membership list.
4. Declare the module's installer. When it registers exactly the module's
   `#[harn_builtin]`s, write
   `harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_x_builtins);`;
   otherwise declare a hand-written installer beside it with
   `harn_vm_core::stdlib_installer!(Pure, register_x_builtins);`. Use `Pure`
   for deterministic builtins with no I/O and `Host` for everything else.
   Installers must not register a name another installer registers. A
   module that keeps thread-local state declares its reset with
   `harn_vm_core::stdlib_state_reset!(reset_x_state);`.
5. Read the arguments through `Args` / `Options` (see
   [Reading arguments](#reading-arguments)).
6. Add or update a `harn explain <name>` snapshot / conformance fixture
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

## Pull request titles and descriptions

Your pull request title and description are the parts of a change that outlive
it. Six months later, `git log` and the issue tracker are what a reader has.

### Title format

Write titles as `[Area] Sentence case description`.

```text
[VM] Enforce execution evidence ownership boundaries
[Stdlib] Serve compact tool descriptions to every renderer
[CI] Reserve the Nextest runner for offline replay
```

Use one area tag, in square brackets, at the front. Capitalize only the first
word of the description and any proper nouns. Do not end the title with a
period. Aim for under 72 characters so the title is readable in `git log
--oneline` and in the merge queue.

The area tags are a small top-level set, coarser than the `area/*` issue
labels. Pick the surface a reader would look in first:

| Tag | Covers | Related issue labels |
| --- | --- | --- |
| `[Parser]` | `harn-lexer`, `harn-parser`, `harn-ir`, type checking, diagnostics | `area/parser`, `area/typechecker` |
| `[VM]` | `harn-vm`, `harn-kernel`, bytecode, values, the runtime evaluator | `area/vm` |
| `[Stdlib]` | `harn-stdlib`, `harn-hostlib`, builtins, `.harn.prompt` assets | `area/stdlib` |
| `[Agent]` | Agent loop, transcripts, context assembly, compaction, judges | `area/learning`, `area/personas`, `area/supervision` |
| `[LLM]` | `harn-vm/src/llm`, provider clients, capabilities, turn options | `area/vm` |
| `[Models]` | The model catalog, aliases, and per-model capability entries | `area/vm` |
| `[CLI]` | `harn-cli`, `harn-fmt`, `harn-lint`, `harn-lsp`, `harn-dap`, editors | `area/cli` |
| `[Modules]` | `harn-modules`, imports, packages, version pinning | `area/modules`, `package-management` |
| `[Orchestrator]` | `harn serve`, triggers, connectors, flow control, sandboxing | `area/orchestrator`, `area/triggers`, `area/connectors`, `area/flow-control`, `area/sandbox` |
| `[ACP]` | ACP, A2A, MCP, `harn-serve` protocol surfaces, SDK bindings | `area/acp-a2a`, `area/interop` |
| `[Evals]` | `evals/`, `conformance/`, `bench/`, determinism and measurement | `area/evals`, `area/performance` |
| `[Docs]` | `docs/`, `spec/`, `README.md`, `website/`, this file | `documentation` |
| `[CI]` | `.github/`, `scripts/`, `Makefile`, hooks, supply chain | `github_actions` |
| `[Release]` | Version cuts, changelog assembly, publishing, install scripts | `production-readiness` |

If a change genuinely spans two areas, tag the one that owns the behavior, not
the one with the larger diff.

### Titles that must not carry a tag

Two title shapes are load-bearing and are exempt:

- **Release pull requests** must stay exactly `Release vX.Y.Z`.
  `.github/workflows/publish-release.yml` matches that commit subject before it
  tags and publishes. Adding a `[Release]` prefix breaks the release.
- **Bot pull requests** keep whatever their tool generates. Dependabot writes
  `chore(deps): ...`, and rewriting those titles fights the tool for no gain.

### Description format

Keep the description short and specific. Five sentences is usually enough.
Cover four things:

1. What changed, in terms of behavior a user, agent, or downstream repository
   sees.
2. Why it changed. Name the defect or the goal.
3. The one risk you are most worried about, or the honest blind spot.
4. How you verified the claim, at the level of the claim.

Leave out anything the pull request page already shows. The Files tab lists the
files. The Checks tab lists the test runs. A line saying "all tests pass" adds
nothing a reader cannot already see, and it says nothing about whether the
tests bind to the behavior you changed.

Here is a complete description that does the job:

```markdown
`ToolSchema.compact` documented a contract that no renderer read, so a tool
that declared it was still served its full description on every call. Every
renderer now serves the shortened text: the native wire payload and the
OpenAI function-schema projection share one `tool_summary` in Rust, and the
text catalog renders compact tools as one-liners. The risk is that
`tool_summary` in Rust and `__agent_tool_summary` in the stdlib are two
implementations of one policy, held in parity by tests on each side rather
than by a shared owner. A conformance fixture runs the same agent turn twice
against registries that differ only in the flag and reads the served byte
count off a real `provider_call_request`; it fails on the pre-fix binary with
`full=8585 compact=8585`, which is the exact comparison that measured zero in
the issue.

Closes #7767
```

If a change is hard to follow from prose alone, add a small Mermaid diagram in
a ` ```mermaid ` fence.

### Linking issues

A pull request that resolves an issue says which of the issue's listed sub-asks
it closes.

Use a closing keyword only when the pull request resolves the whole issue:
`Closes #N items: 1, 2, ...` naming every enumerated sub-ask, or
`Closes #N` when the issue is not enumerated. For partial work write
`Partial: #N items: 1, 3` or `Refs #N`, and never a closing keyword.

The reason the partial form avoids the keyword is mechanical, not stylistic.
GitHub builds the closing link from the keyword alone and ignores whatever
follows it, so `Closes #N items: 1, 3` closes the whole issue on merge while
reading as a partial close, and the sub-asks it did not land lose their owner.
The pull request's closing-reference field is the check, not its body text:

```sh
gh pr view <N> --json closingIssuesReferences
```

A partial pull request should report an empty list there. Close an issue only
once every listed sub-ask is verified on `main`. If a sub-ask survives the pull
request, rescope the issue or file the remainder so the work keeps an owner.

Keep write-ups repository-agnostic, per
[Keep write-ups repository-agnostic](#keep-write-ups-repository-agnostic).

## Request the full CI suite

Add `CI-Scope: full` on its own line in the pull request body to run every
CI family before merge. The merge queue inherits this declaration from its
pull requests. After editing an existing pull request, rerun CI to apply the
declaration. CI wiring, dependency manifests and lockfiles, toolchain pins,
generated protocol artifacts, and safety-domain changes also select the full suite.

Other pull requests and merge groups test their affected crates and reverse
dependencies. They defer Linux sandbox tests, macOS, the Windows cross-check,
publishable-crate verification, and stack-frame checks until the push to main.
Every main push runs the full suite, including release metadata changes.

Inspect the **Recover failed main CI** workflow after a main failure. It retries
failed jobs once. A passing retry opens a flaky-test issue. A persistent failure
opens a signed revert PR when the parent commit has green full-suite proof.
The revert goes through Smart Ship. Missing parent proof or a conflicting revert
opens an investigation issue instead of guessing the culprit.

## Keep write-ups repository-agnostic

Harn is a general agent-orchestration project. Issue bodies, PR descriptions,
code comments, docs, and test fixtures must not cite a downstream consumer's
private paths, hostnames, IP addresses, or evaluation vocabulary — those read as
Harn's own and send a public reader to files this repository does not have.

- Use RFC-2606/RFC-5737 placeholders in examples and fixtures
  (`example.internal`, `192.0.2.0/24`), or an RFC-1918 address that belongs to
  no real machine when a fixture needs a genuinely private range.
- Require an explicit environment variable rather than hard-coding a server a
  contributor cannot reach.
- Describe a consumer's corpus or program by shape ("a consumer-side eval
  corpus"), not by its path or its internal metric names. Consumer evaluation
  programs gate on their own metrics; Harn does not name them.

A named integration fact is fine where behavior depends on it — a downstream
host's artifact directory in the code-index ignore list is a real directory the
walker must skip. `scripts/check_public_product_names.sh` enforces the naming
half of this rule and carries the allowlist of those deliberate exceptions; add
to that allowlist rather than working around the scan. New public text (a pull
request's title, body, commit messages, and the lines it adds, plus issue
comments) must not name the downstream brand even as a bare word; older tracked
text is held only to the compound product names.

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
