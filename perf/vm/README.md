# VM microbenchmarks

This directory contains deterministic `.harn` fixtures for the opt-in
interpreter performance suite. The fixtures avoid network access, filesystem
mutation, sleeps, host integration, and LLM calls so they measure core VM work
rather than provider or environment latency.

Run the suite in release mode:

```bash
make bench-vm
```

The runner builds `target/release/harn` once, then runs `harn bench` over every
fixture:

```bash
./scripts/bench_vm.sh --iterations 20
```

To compare against the checked-in local baseline:

```bash
./scripts/bench_vm.sh --iterations 20 --baseline perf/vm/BASELINE.md
```

`BASELINE.md` records the current local baseline as an average across several
full suite passes. The comparison column uses the baseline table's
`mean_avg_ms` value.

The Criterion VM clone-on-call probe is separate from the fixture runner:

```bash
cargo bench -p harn-vm-perf --bench bench_vmenv_clone
```

It measures the internal non-module closure call environment path at 0, 5, 25,
and 100 captured names. Criterion reports time per call; the benchmark also
prints allocation operations per call for each capture count.

The fixture suite also has an allocation-counting Criterion harness:

```bash
cargo bench -p harn-vm-perf --bench bench_vm_fixtures
```

It runs the checked-in `.harn` fixtures in-process and prints allocation
operations and allocated bytes per fixture run.

The fixture set covers core interpreter ops (arithmetic, function calls,
struct/dict/list reads), the option-builder pipelines that connector
helpers and `agent_dispatch_tool_batch` exercise on every call
(`dict_merge_loop`, `dict_subscript_assign`, `filter_nil_loop`), and a
representative agent-tool dispatch loop (`agent_tool_dispatch`).
[`docs/src/dev/vm-stdlib-perf-notes.md`](../../docs/src/dev/vm-stdlib-perf-notes.md)
captures the analysis behind the current allocation budget.

This suite is intentionally not part of `make all`; local CPU load, thermal
state, and target cache state are too noisy for a default correctness gate. For
before/after VM optimization work, run the suite several times on the same
machine with the same `--iterations` value, compare average wall time, and treat
changes under roughly 5-10% as noise unless they reproduce consistently.

When running benchmarks from multiple worktrees, set a per-run target directory
to avoid build contention:

```bash
CARGO_TARGET_DIR=/tmp/harn-bench-target ./scripts/bench_vm.sh --iterations 20
```

## Focused VM microbenchmarks

Use the Criterion microbench layer when working on interpreter internals and
you need a small, repeatable signal for a specific hot path. It complements the
fixture suite above; it does not replace the end-to-end `harn bench` smoke
coverage.

```bash
make bench-vm-micro
./scripts/bench_vm_micro.sh property_inline_cache_hits
./scripts/bench_vm_micro.sh -- method_inline_cache_hits
```

`scripts/bench_vm_micro.sh` runs `cargo bench -p harn-vm-perf --bench
bench_vm_hot_paths` with a fresh temporary `CARGO_TARGET_DIR` by default so
parallel worktrees do not fight over the shared Cargo target lock. Pass
`--target-dir /tmp/harn-vm-microbench-target` or set `CARGO_TARGET_DIR` when
you intentionally want to reuse compiled dependencies across runs.
The script also defaults `CARGO_PROFILE_BENCH_LTO=false` and
`CARGO_PROFILE_BENCH_CODEGEN_UNITS=16` so targeted runs do not spend minutes in
link-time optimization before measuring a single hot path. Override those env
vars for final trend-capture runs when you want the full Cargo bench profile.

The microbench suite covers focused VM hot paths:

- non-module closure call environment setup (`bench_vmenv_clone`)
- closure call setup through the VM dispatch loop
- direct builtin/native call setup
- runtime parameter validation for typed user calls
- property inline-cache hits for dicts, structs, lists, and strings
- method inline-cache hits for list/string/dict/set helpers
- list callback dispatch through `.filter` and `.map`
- std/collections dict helper builtins
- bytecode-cache freeze/serialize and adjacent-artifact load paths

Each Criterion benchmark emits one JSON object per allocation probe on stderr:

```json
{"suite":"vm_hot_paths","benchmark":"property_inline_cache_hits","iterations":25,"allocations_per_iteration":123.0,"allocated_bytes_per_iteration":4567.0}
```

Criterion timing estimates remain machine-readable under
`$CARGO_TARGET_DIR/criterion/<group>/<benchmark>/new/estimates.json`, so PR
comments or local comparison scripts can consume allocation JSONL and timing
JSON independently instead of failing on noisy wall-clock deltas.
