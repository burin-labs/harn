# bench/frontend — compiler front-end phase benchmarks

Criterion benchmarks that time each front-end phase — lex, parse,
typecheck, and bytecode compile — in isolation over a deterministic
synthetic module, so a regression in one phase cannot hide inside an
end-to-end compile number.

```bash
cargo bench -p harn-frontend-perf --bench bench_frontend_phases
```

The corpus is generated in-code (`synthetic_module`) from the constructs
that dominate real Harn sources: typed `fn`s with defaults, dict and list
literals, string interpolation, `if`/`for`/`match` control flow, closures
over collection pipelines, and struct/enum/type declarations. Being
synthetic keeps the crate free of large checked-in fixtures and makes the
token mix stable across releases; it intentionally trades away
representativeness of any single real file, so treat these numbers as
regression signal, not as absolute compile-latency claims.

House method for before/after tables (same as `bench/vm`): three
alternating baseline/candidate passes on an idle machine, report the
per-phase medians, treat <5% as noise.

## End-to-end cold compile

The phase benchmarks time one module in isolation, so they cannot see work
repeated across a module graph. `scripts/bench_cold_compile.sh` times the whole
batch path instead: `harn precompile --warm-cache` over the repository's own
multi-module trees (`personas`, `scripts`, `tests`, `examples`), one process
per tree, each with an empty cache. Each run prepares the embedded stdlib
catalog and then every entry, imported module, and interface.

```bash
cargo build --release --bin harn
scripts/bench_cold_compile.sh --bin base=/path/to/old/harn --bin cand=target/release/harn
```

Binaries alternate every round, and the report gives per-tree and total
medians of wall, user, and system seconds. Some test sources in these trees
carry type errors; `--keep-going` counts them as failed, so compare the failed
counts as well as the times.
