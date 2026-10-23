# Run VM benchmarks

Run these commands from a Harn checkout after `make setup`.

Measure the fixtures under `bench/vm` with a release build:

```sh
scripts/bench_vm.sh --iterations 20
```

The runner builds Harn through `make build-release`, then parses and compiles
each fixture once. It reads iteration timings from `harn bench --output` and
prints minimum, mean, and maximum times in milliseconds.

Set `HARN_BIN` to measure a specific executable. This skips the build.
Use `--no-build` to use the checkout's existing release executable when
`HARN_BIN` is unset.

Measure complete `harn run` commands, including process startup:

```sh
scripts/bench_vm.sh --no-build --cold-start --startup-runs 5
scripts/bench_vm.sh --no-build --warm-start --startup-runs 5
```

Each cold run gets a fresh, private bytecode cache. Warm runs share a private
cache after one unmeasured warmup. The runner removes its own caches after the
measurement. It doesn't clear your normal cache.

To collect profiles during loop measurements, pass `--profile-json-dir DIR`.
Profiling adds timing work, so compare runs with the same profile setting.

To compare means against an existing Markdown benchmark table, pass
`--baseline FILE`. The table needs the fixture name in its first column and
the mean time in its fourth column. Missing or zero baselines print `-`.

See the [CLI reference](../cli-reference.md#harn-bench) for individual fixture
benchmarks and the JSON timing fields.
