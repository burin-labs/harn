# Testbench mode

Testbench mode is the composition primitive that wires Harn's
deterministic substrate — virtual time, mocked LLMs, filesystem
overlay, recorded subprocesses, and a deny-by-default network — behind
a single CLI surface (`harn test-bench`) and a single Rust API
(`harn_vm::testbench::Testbench`).

It is the answer to the question "how do I run this `.harn` script
hermetically?". Production wires the real implementations of every
host capability; tests and demos pick a configuration and get an audit
trail of everything that crossed the host boundary.

## Host capabilities

A Harn pipeline reaches the outside world through five host
capabilities. Testbench mode lets the operator override every one of
them, leaving production behavior untouched.

| Capability | Default | Testbench override |
|---|---|---|
| Wall-clock + monotonic time | Real `tokio::time` | `MockClock` — `now_ms()`, `sleep(...)`, cron, and the trigger dispatcher all honor it |
| LLM responses | Configured providers | JSONL fixture replay (same format as `harn run --llm-mock`) or scripted recording |
| Filesystem (read/write/append/delete) | Real disk | Read-through, copy-on-write `OverlayFs` with diff emission |
| Subprocess invocations | Real `std::process::Command` | `ProcessTape` records `(program, args, cwd) → (stdout, stderr, exit, virtual Δt)` for replay; `WasiToolchain` runs WASM modules under wasmtime with `clock_time_get` and `poll_oneoff` virtualized into the mock clock |
| Network egress | Configured `HARN_EGRESS_*` policy | Deny-by-default; `--allow-host` opens specific destinations |

Every override is opt-in. Activating one axis does not change the
others.

## CLI

### `harn test-bench run`

```sh
harn test-bench run examples/cron-rollup.harn \
    --clock paused --start-at 1767225600000 \
    --llm-fixture llm.jsonl \
    --fs-overlay ./worktree \
    --process-record process.tape \
    --network deny --allow-host github.com \
    --emit-diff fs.diff -- arg1 arg2
```

Flag reference:

| Flag | Behavior |
|---|---|
| `--clock paused` (default) | Pin the unified mock clock; `sleep(...)` advances it. `--clock real` skips this layer |
| `--start-at <unix_ms>` | Initial wall-clock time. Defaults to `2026-01-01T00:00:00Z` |
| `--llm-fixture <path>` | Replay scripted LLM responses (same JSONL format as `harn run --llm-mock`) |
| `--llm-record <path>` | Capture executed responses for a future replay |
| `--fs-overlay <dir>` | Mount the COW overlay rooted at `dir` |
| `--process-record <path>` / `--process-replay <path>` | Record or replay subprocess invocations |
| `--process-wasi <dir>` | Resolve subprocesses against a directory of WASI (`wasm32-wasi`) modules — see [WASI subprocess sandbox](#wasi-subprocess-sandbox) |
| `--network deny` (default) / `--network real` | Egress policy |
| `--allow-host <h-or-cidr>` | Whitelist a destination. Repeatable |
| `--emit-diff <path>` | Write a unified-style diff of overlay writes to `path` |

The default flag-set composes to "run hermetically; fail loud on any
leak":

```sh
harn test-bench run script.harn
```

is equivalent to `--clock paused --network deny`, with no LLM/FS/process
overrides.

### `harn test-bench replay`

```sh
harn test-bench replay script.harn --process-tape run.tape
```

Replays a prior `--process-record` tape. The script must request the
same `(program, args, cwd)` tuples in the same order; divergence
fails the run.

## Rust API

The CLI is a thin wrapper over `harn_vm::testbench::Testbench`:

```rust
use harn_vm::testbench::Testbench;

let session = Testbench::builder()
    .paused_clock_at_ms(1_767_225_600_000)
    .replay_llm("fixtures/llm.jsonl")
    .fs_overlay("./worktree")
    .replay_subprocesses("fixtures/process.tape")
    .deny_network()
    .build()
    .activate()?;

// run a Harn pipeline through the existing VM entry points...

let finalize = session.finalize()?;
println!("fs diff: {} change(s)", finalize.fs_diff.len());
```

The `TestbenchSession` returned from `activate()` is RAII-scoped:
dropping it tears down every override and restores the prior thread
state. `finalize()` persists recorded LLM/process tapes (when in
record mode) and returns the structured artifacts.

## Subprocess modes

The testbench has three composable subprocess modes; pick the one that
matches the trade your test wants to make.

| Mode | Flag | Native binary support | Subprocess clock virtualization |
|---|---|---|---|
| **Real** | (default) | yes | none — real wall clock |
| **Record / Replay** | `--process-record` / `--process-replay` | yes | parent's observation only — child reads real clock during recording, replay re-injects the recorded Δt into the parent's clock |
| **WASI** | `--process-wasi <dir>` | no — only WASM modules | full — `clock_time_get` and `poll_oneoff` clock subscriptions read/advance the testbench `MockClock` |

### Record/replay time leak

Subprocesses spawned in record mode are spawned by the host kernel and
observe real wall-clock time. Recorded tapes capture the duration the
*parent* observed via the unified clock and replay it into the parent's
clock — but a script that depends on a subprocess' internal timing
(e.g. a `sh -c 'date +%s'` round-trip) will see the real clock and may
diverge between record and replay. WASI mode is the answer when that
matters; record/replay is the answer when the tool can't be compiled to
WASM.

### WASI subprocess sandbox

`--process-wasi <dir>` resolves every subprocess invocation against
`<dir>/<program>.wasm`. Programs that match are run inside wasmtime
with the testbench's mock clock virtualized into `clock_time_get` and
`poll_oneoff`, so a 24-hour `sleep` inside the WASI tool returns
immediately while the parent's testbench clock advances by 24 hours.

```sh
harn test-bench run script.harn --process-wasi ./wasm-toolchain/
```

#### What's virtualized

- `wasi_snapshot_preview1::clock_time_get` — both `CLOCK_REALTIME` and
  `CLOCK_MONOTONIC` return the testbench mock clock, in nanoseconds.
- `wasi_snapshot_preview1::poll_oneoff` — clock subscriptions
  (relative or absolute) advance the mock clock by their timeout and
  resolve immediately. `std::thread::sleep` and `tokio::time::sleep`
  inside the WASM module both compile down to this path on the
  `wasm32-wasi` target, so neither blocks the host thread.
- **Filesystem**: a fresh temp directory mounted at `/`. Any files the
  module writes are merged into the active overlay before
  `command_output` returns, so the parent observes them in
  `OverlayFs::diff()`.
- **Network**: socket imports are not linked. Any WASM module that
  tries to dial a socket fails at link time with a deterministic error
  — the same `deny-by-default` posture as the host network policy, but
  enforced one layer deeper.

#### Limits

- Only WASI preview 1 modules (`wasm32-wasi`) are supported. Native
  binaries (`git`, `gh`, `bash`) are not compiled to WASI; they fall
  through to the host spawn path. A directory like
  `--process-wasi ./toolchain/` can contain a partial set of tools —
  invocations whose program has no matching `.wasm` use the underlying
  subprocess mode (real spawn, or recorded tape if `--process-record` /
  `--process-replay` is also active).
- `poll_oneoff` FD-read/write subscriptions return `ERRNO_NOTSUP`. A
  module that blocks on stdin polling cannot run; pass input via args
  or pre-stage files in the overlay.
- The preopened `/` directory starts empty. Tools that need to read
  files from the workspace overlay should be invoked after the
  relevant files have been materialized to the host filesystem the
  overlay mirrors.
- The `wasmtime` runtime adds ≈20 MB to the published `harn` binary;
  the feature is gated behind the `testbench-wasi` Cargo feature for
  library consumers that don't need it.

#### When to reach for it

WASI mode is the right answer when a test depends on a subprocess
*observing* the same virtual time as its parent — agent-loop scenarios
that simulate hours of work in milliseconds, deterministic eval suites
where the tool reads `time.time()` for retry backoff, anything where
record/replay's duration-only capture would lose information. For
arbitrary native tooling, record/replay remains the workhorse.

## Filesystem overlay semantics

The overlay is a copy-on-write layer in front of a real worktree. The
sandbox enforcement — `enforce_fs_path` — runs *before* the overlay
hook, so a write that would normally be rejected by the workspace-root
policy is still rejected in testbench mode. Reads and writes to paths
*outside* the overlay's root fall through to the real filesystem; the
overlay is bounded to the worktree the operator declared.

`OverlayFs::diff()` returns one entry per change. `render_unified_diff`
formats them as a `git apply`-style hunk list:

```text
--- /dev/null
+++ b/new-file.txt
+hello
--- a/existing.txt
+++ b/existing.txt
-old content
+new content
--- a/doomed.txt
+++ /dev/null
-content
```

Binary content is rendered via `String::from_utf8_lossy`, so the
unified output is informational, not necessarily reapplicable for
non-utf8 files. The structured `diff()` value retains exact bytes.

## Defaults that fail loud

The testbench is opinionated about its defaults so a single
`harn test-bench run script.harn` is a meaningful signal:

- **Paused clock** — wall-clock-based assertions are deterministic.
- **Deny network** — accidental egress fails the run.
- **Empty LLM fixture queue** — calls without a recorded response
  surface a clear "no script installed" error instead of falling
  through to the real provider.

Operators opt into looser defaults explicitly (`--clock real`,
`--network real`, `--llm-fixture <path>`) when the test under
development calls for it.

## Conformance coverage

Testbench mode has first-class coverage in the conformance suite under
`conformance/tests/testbench/`. Run them with:

```sh
cargo run --bin harn -- test conformance --filter testbench
```

The conformance runner activates the testbench session automatically
when sidecar files are present next to the `.harn` test:

| Sidecar | Effect |
|---|---|
| `<name>.process-tape.json` | Activates subprocess replay against the tape; a `cwd: null` entry acts as a wildcard for portable fixtures |
| `<name>.fs-overlay/` (directory) | Mounts the directory as the overlay root for the run; `testbench_fs_diff()` returns the in-memory diff |
| `<name>.testbench-tape` | Records a fresh unified tape during the run and compares it byte-for-byte against the fixture via the fidelity oracle |

Any sidecar's presence also activates a paused clock pinned at
`2026-01-01T00:00:00Z` so `now_ms()`, `sleep(...)`, and recorded
durations stay deterministic across runs.

Two script-side builtins are wired for tests that need to introspect the
testbench from inside a pipeline:

- `testbench_is_active()` — `true` when a mock clock is currently
  installed.
- `testbench_fs_diff()` — list of `{path, kind, content?}` dicts
  describing every overlay change made so far. Returns an empty list
  when no overlay is active.

## Relationship to other surfaces

- `harn run --llm-mock` is a strict subset: it activates only the LLM
  axis. `harn test-bench run --llm-fixture` does the same plus pins the
  clock and denies network egress.
- The unified mock clock (`mock_time(...)` / `advance_time(...)` /
  `unmock_time()` script builtins) is the same clock testbench mode
  pins; mixing the two is supported.
- `OrchestratorHarness` accepts a `Clock`; testbench mode pre-installs
  the same `MockClock` trait the harness uses.

See also:

- `docs/src/dev/tape-format.md` for the unified event tape schema and
  the fidelity oracle that compares two tapes.
- `docs/src/dev/testing.md` for approved test-pattern guidance.
- `crates/harn-vm/src/testbench/` for the Rust source of every
  composable axis.
- Issue [#1440](https://github.com/burin-labs/harn/issues/1440) for the
  composition primitive's design rationale.
- Issue [#1441](https://github.com/burin-labs/harn/issues/1441) for the
  recording/replay tape format and fidelity oracle.
- Issue [#1443](https://github.com/burin-labs/harn/issues/1443) for the
  WASI subprocess sandbox.
