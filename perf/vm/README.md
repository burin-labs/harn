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
