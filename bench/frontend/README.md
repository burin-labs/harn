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
