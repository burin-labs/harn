# Single-threaded DES runtime mode

`harn test-bench run --runtime des` swaps the testbench's default
multi-threaded Tokio runtime for a single-threaded `current_thread`
runtime. All Harn tasks, I/O completions, and timer callbacks share one
OS thread, eliminating any inter-thread scheduling races that could
make a recorded tape diverge across reruns.

This document captures the rationale, the constraint surface, the
benchmark results, and the spike's recommendation. It tracks issue
[#1444][issue].

## Why a separate runtime?

The default testbench runtime (`paused-tokio`) is already very
deterministic: every VM task runs inside a `tokio::task::LocalSet`,
which pins it to the runtime's main thread. The unified mock clock
(`mock_time` / `advance_time`) is thread-local and observed only from
that pinned thread. Concurrent agents spawned via `parallel each` /
`parallel settle` interleave cooperatively at `await` points, not
across threads. So even under `--runtime paused-tokio`, the only
non-determinism left in practice comes from:

1. Background Tokio worker threads waking I/O callbacks at unpredictable
   times — irrelevant when network egress is denied and the clock is
   paused, but a real risk if the operator forgets to deny network or
   uses `--clock real`.
2. Future Harn primitives that legitimately need cross-thread state
   (none today; some on the roadmap).

DES mode buys insurance against (1) and (2) at zero observable cost
today.

## Constraint surface

A script is **DES-safe** if every host capability it reaches stays
inside the testbench's mocked surface:

| Primitive | DES-safe? | Notes |
|---|---|---|
| `mock_time` / `advance_time` / `now_ms` / `sleep` | ✅ | Synchronous when mocked. |
| `parallel each` / `parallel settle` / `spawn` | ✅ | LocalSet-bound; cooperative. |
| `read_file` / `write_file` (with `--fs-overlay`) | ✅ | Overlay reads pass through, writes stay in memory. |
| `run_command` (with `--process-replay <tape>`) | ✅ | Tape lookup is deterministic. |
| `llm_call` (with `--llm-fixture <jsonl>`) | ✅ | Fixture replay is deterministic. |
| `http_get` / `http_post` | ❌ | Real network I/O. Use `--llm-fixture` or `--network deny` blocks them. |
| `run_command` without a process tape | ❌ | Real subprocess. |
| `now_ms` without `mock_time` / `--clock paused` | ❌ | Real wall clock. |
| Any FFI / native thread / `std::thread::spawn` | ❌ | Bypasses Tokio entirely. |

The testbench's deny-by-default network policy and `--clock paused`
default already block the most common (❌) row at the host boundary, so
in practice the DES-safe set is "anything you would have run under
`harn test-bench run` anyway."

## Benchmarks

Measured on a `cargo build --release` binary on Apple Silicon (debug
build numbers are similar). Each row is the median of five runs of
`harn test-bench run` against the listed script.

| Workload | `paused-tokio` median | `des` median | Δ |
|---|---|---|---|
| `testbench_paused_sleep` (1 agent, sleep 24h) | ~10 ms | ~10 ms | ≤ 1 ms |
| `testbench_replay_fidelity` (3 clock reads + 2 advances) | ~10 ms | ~10 ms | ≤ 1 ms |
| `testbench_concurrent_agents_settle` (5 `parallel settle` agents) | ~10 ms | ~10 ms | ≤ 1 ms |
| Nested `parallel settle` (10 outer × 3 inner agents, FS overlay) | ~25 ms | ~25 ms | ≤ 1 ms |

DES mode adds essentially zero wall-clock overhead. The current_thread
runtime is built once per `--runtime des` invocation on its own OS
thread (16 MB stack, matching the main CLI thread), so the cost is
dominated by VM startup, not runtime construction.

### Tape determinism

For every workload above, both `paused-tokio` and `des` produced
**bit-identical tapes** across 5 reruns each, and the `paused-tokio`
tape and the `des` tape for any single workload were also bit-identical
to each other. This confirms that the existing LocalSet-based execution
already absorbs scheduler non-determinism for the kinds of workloads we
care about today; DES mode is a safety net rather than a delta.

If a future Harn primitive intentionally crosses thread boundaries —
for example, a producer/consumer pattern that uses `tokio::sync::mpsc`
across worker threads — `--runtime des` would force it through the
single-threaded scheduler and prevent recorded-tape skew. We have no
such primitive today.

## Decision

**Ship as an opt-in flag, do not make it the default.**

- `paused-tokio` (the existing default) is sufficient for every
  workload exercised in conformance and the testbench CLI integration
  suite today.
- `--runtime des` is documented and tested so it is available the
  moment we add a concurrency primitive that escapes the LocalSet, or
  when a customer demands a leaderboard-grade reproducibility guarantee
  beyond what auto-advance can promise.
- We **do not** spend additional engineering on a custom DES scheduler
  with priority-queue event ordering (à la FoundationDB) until a
  concrete consumer requires bit-exact ordering across machine
  architectures or against adversarial fuzzing. Tokio's `current_thread`
  scheduler is a sound and cheap intermediate point.

## Implementation notes

The DES mode is enacted entirely in `crates/harn-cli/src/commands/test_bench.rs`:

- `run_args` dispatches on `--runtime`: the `paused-tokio` arm calls
  `run_with_bench` (the original code path), the `des` arm calls
  `run_with_des_runtime`.
- `run_with_des_runtime` spawns a fresh OS thread (using the same
  16 MB stack as the main CLI thread), builds a
  `tokio::runtime::Builder::new_current_thread().enable_all().build()`
  runtime inside it, activates the testbench mocks on that thread so
  thread-local state is consistent for every task, drives the script,
  and ships the `RunOutcome` back through a `std::sync::mpsc::channel`.
- The receive is wrapped in `tokio::task::spawn_blocking` so the
  awaiting CLI task is suspended cooperatively rather than blocking a
  multi-thread worker.

There is no `DesRuntime` Rust trait — the runtime is selected at the
CLI seam, not abstracted across the VM. If we add a programmatic
`testbench_run_in_des(...)` API we should resist promoting the runtime
choice into the `Testbench` config; runtime selection is an outer-loop
concern that doesn't belong inside the host-capability composition.

## See also

- `docs/src/dev/testbench.md` — composition primitive (axes, CLI flags).
- `docs/src/dev/tape-format.md` — unified tape schema and fidelity
  oracle modes.
- `conformance/tests/testbench/` — `testbench_*` regression cases.
- `crates/harn-cli/tests/harn_cli_fast/test_bench_cli.rs` — `des_runtime_*` tests.
- Issue [#1444][issue] — exploratory scope and acceptance criteria.

[issue]: https://github.com/burin-labs/harn/issues/1444
