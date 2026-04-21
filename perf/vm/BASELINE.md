# VM microbenchmark baseline

Recorded: 2026-04-20 20:13:55 PDT

Environment:

- Hardware: Apple M5 Pro
- OS: Darwin 25.4.0 arm64
- Rust: rustc 1.95.0 (59807616e 2026-04-14)
- Harn: 0.7.26
- Source: `c35e0b25` plus this PR's benchmark-suite changes
- Target dir: `/tmp/harn-issue-402-target`

Method:

- Built once with `cargo build --release --bin harn`.
- Ran `./scripts/bench_vm.sh --no-build --iterations 20` three times.
- `mean_avg_ms` is the average of each pass's `avg_ms`; this is the value used
  by `scripts/bench_vm.sh --baseline perf/vm/BASELINE.md` for comparisons.
- `best_min_ms` and `worst_max_ms` are the lowest and highest per-iteration
  wall times observed across the three passes.

| benchmark | suite_runs | iterations_per_run | mean_avg_ms | stddev_avg_ms | best_min_ms | worst_max_ms | avg_ms_samples |
|---|---:|---:|---:|---:|---:|---:|---|
| arithmetic_loop | 3 | 20 | 89.94 | 0.90 | 87.21 | 99.14 | 90.46, 88.67, 90.68 |
| dict_property_read | 3 | 20 | 86.11 | 2.40 | 79.20 | 109.22 | 89.47, 84.06, 84.79 |
| function_call_loop | 3 | 20 | 111.77 | 0.93 | 108.04 | 117.28 | 111.92, 110.56, 112.83 |
| list_iteration | 3 | 20 | 24.17 | 0.74 | 22.60 | 29.42 | 23.72, 25.21, 23.57 |
| list_map_filter | 3 | 20 | 273.22 | 4.03 | 263.19 | 354.91 | 276.97, 267.63, 275.05 |
| local_variable_lookup | 3 | 20 | 184.80 | 0.90 | 179.13 | 198.77 | 185.77, 185.02, 183.61 |
| method_call_dispatch | 3 | 20 | 55.34 | 1.36 | 52.76 | 76.15 | 55.77, 56.75, 53.50 |
| recursive_countdown | 3 | 20 | 23.30 | 0.59 | 22.02 | 28.23 | 24.09, 23.15, 22.67 |
| string_interpolation_loop | 3 | 20 | 7.11 | 0.24 | 6.60 | 8.47 | 7.40, 7.12, 6.82 |
| struct_field_read | 3 | 20 | 134.46 | 1.42 | 128.70 | 146.13 | 136.27, 132.80, 134.32 |
