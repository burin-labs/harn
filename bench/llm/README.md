# LLM microbenchmarks

This directory contains opt-in Criterion benchmarks for LLM runtime hot paths.

Run the options roundtrip benchmark:

```bash
make bench-llm
```

Or run the Criterion target directly:

```bash
cargo bench -p harn-llm-perf --bench bench_llm_options_roundtrip -- --output-format bencher
```

`llm_options_roundtrip` measures the existing option extraction path for
representative option dicts with 1, 5, 25, and 100 top-level keys. Criterion
reports per-call timing as `ns/iter`. Set a per-run `CARGO_TARGET_DIR` when
running benchmarks from multiple worktrees.
