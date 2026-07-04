# perf/ — performance suites and baselines

Criterion suites, `.harn` micro-fixtures, budget gates, and recorded
baselines, grouped by subsystem:

| Directory | What it covers |
| --- | --- |
| `vm/` | Interpreter microbenchmarks (dispatch, inline caches, env clones) + `BASELINE.md` |
| `cli/` | CLI cold-start budget gate for `.harn`-ported subcommands (`budgets.toml`, `baselines/main.json`) |
| `orchestration/` | Hook dispatch, workflow-bundle export, Rust↔`.harn` boundary crossing, transcript projection |
| `llm/` | LLM-layer benchmarks |
| `postgres/` | Postgres-backed runtime benchmarks |

## Status note for the VM-heavier re-architecture (2026-07)

The stage-loop inversion re-architecture moves more execution into the
VM: stage loops, per-tool-call middleware, post-turn governors, and
compaction/projection policies all run as `.harn` code. Two facts frame
the perf work for that migration:

- **The interpreter itself is not the risk.** The VM perf roadmap
  (epic [#2095](https://github.com/burin-labs/harn/issues/2095) and all
  eight children) fully shipped: ArcStr strings, boxed variant slimming,
  inline caches on every hot opcode, `Rc<Regex>` caching, dispatch
  splits. `perf/vm/` pins those wins.
- **The risk surface is the `harn_entry` boundary**
  (`crates/harn-vm/src/stdlib/harn_entry.rs`): every Rust→`.harn` call
  pays a child-VM clone plus a module-cache lookup, and — when the
  parent VM's cache misses the target module — a full module
  re-instantiation that is thrown away on return
  (`modules.rs` `instantiate_module`). Typed crossings additionally pay
  a JSON double-marshal + serde deserialize. The migration multiplies
  crossing counts (per tool call, per turn, per stage attempt).

These suites are the **regression gates for the stage-loop inversion
wave** (phase H-W2). Run them before/after each inversion PR:

- `perf/orchestration/bench_harn_entry_crossing.rs` — typed vs by-name
  crossing cost, warm vs cold parent module cache.
- `perf/orchestration/bench_transcript_projection.rs` — per-turn
  projection cost at ~10k/50k/100k-token transcripts.
- `perf/orchestration/bench_hook_dispatch.rs` — 1/8/32/128 hook fan-out
  (directly models per-turn governors).
- `scripts/bench_cli_cold_start.sh` — cold-start budget gate against
  `perf/cli/baselines/main.json` (pre-migration baseline recorded; see
  `perf/cli/README.md`).

```bash
cargo bench -p harn-orchestration-perf --bench bench_harn_entry_crossing
cargo bench -p harn-orchestration-perf --bench bench_transcript_projection
./scripts/bench_cli_cold_start.sh --no-build
```

The optimization levers themselves (parent-cache pre-warm or a
process-global instantiated-module cache, `VmValue`-direct hot seams,
stdlib AOT extension) intentionally land *with* the migration waves they
protect, not here — this directory only carries the measurement
infrastructure and the pre-migration numbers.
