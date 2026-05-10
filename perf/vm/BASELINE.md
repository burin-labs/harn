# VM microbenchmark baseline

Recorded: 2026-05-10

Environment:

- Hardware: Apple M5 Pro
- OS: Darwin 25.4.0 arm64
- Rust: rustc 1.95.0
- Harn: 0.8.4
- Source: post-#1426 (set_subscript fast path + native dict builder helpers)

Method:

- Built once with `cargo build --release --bin harn`.
- Ran `./scripts/bench_vm.sh --no-build --iterations 20` three times back-to-back.
- `mean_avg_ms` is the average of each pass's `avg_ms`; this is the value used
  by `scripts/bench_vm.sh --baseline perf/vm/BASELINE.md` for comparisons.
- `best_min_ms` and `worst_max_ms` are the lowest and highest per-iteration
  wall times observed across the three passes.

| benchmark | suite_runs | iterations_per_run | mean_avg_ms | best_min_ms | worst_max_ms | avg_ms_samples |
|---|---:|---:|---:|---:|---:|---|
| agent_tool_dispatch | 3 | 20 | 46.84 | 42.99 | 54.97 | 45.54, 44.22, 50.75 |
| arithmetic_loop | 3 | 20 | 87.97 | 82.84 | 104.91 | 83.62, 83.69, 96.60 |
| comparison_loop | 3 | 20 | 142.72 | 133.97 | 161.48 | 137.71, 134.69, 155.75 |
| dict_merge_loop | 3 | 20 | 31.68 | 29.06 | 38.73 | 31.03, 30.34, 33.67 |
| dict_property_read | 3 | 20 | 70.88 | 65.70 | 81.23 | 67.87, 67.98, 76.80 |
| dict_subscript_assign | 3 | 20 | 15.91 | 14.19 | 18.63 | 14.61, 15.50, 17.62 |
| filter_nil_loop | 3 | 20 | 17.72 | 16.39 | 20.79 | 16.74, 17.06, 19.36 |
| function_call_loop | 3 | 20 | 75.16 | 69.41 | 86.65 | 72.42, 70.71, 82.35 |
| list_iteration | 3 | 20 | 17.49 | 15.84 | 20.85 | 16.38, 16.60, 19.49 |
| list_map_filter | 3 | 20 | 298.64 | 274.12 | 365.38 | 283.53, 278.63, 333.75 |
| local_variable_lookup | 3 | 20 | 136.20 | 127.57 | 165.68 | 130.18, 132.99, 145.44 |
| method_call_dispatch | 3 | 20 | 60.19 | 53.15 | 94.77 | 54.86, 65.99, 59.72 |
| recursive_countdown | 3 | 20 | 21.80 | 17.44 | 48.36 | 17.93, 28.62, 18.85 |
| string_interpolation_loop | 3 | 20 | 10.67 | 8.81 | 31.11 | 8.93, 13.30, 9.77 |
| struct_field_read | 3 | 20 | 82.75 | 72.27 | 129.32 | 73.55, 94.57, 80.14 |
