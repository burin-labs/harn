# Benchmark the portable kernel

Use `harn bench portable` to measure the canonical compiler, artifact decoder,
and native execution kernel separately. The command runs pure executions with
no host grants, checks that repeated compilation produces identical artifact
bytes, checks that every iteration produces the same terminal value, and can
distribute independent dispatches across operating-system threads.

## Prepare an input

The benchmark accepts one JSON value as the entry function's input. For the
standalone reducer in `crates/harn-wasm/demo/reducer.harn`, save this as
`/tmp/reducer-input.json`:

```json
{
  "state": {"count": 0, "history": [], "label": "portable"},
  "event": {"kind": "increment", "amount": 1}
}
```

## Run a native benchmark

```console
harn bench portable crates/harn-wasm/demo/reducer.harn \
  --entry reduce \
  --entry-kind function \
  --input /tmp/reducer-input.json \
  --compile-iterations 30 \
  --iterations 500 \
  --threads 4 \
  --json \
  --output /tmp/portable-kernel-benchmark.json
```

`--threads` measures separate executions that share one decoded, immutable
program artifact. It does not split one execution across threads. Each worker
gets isolated execution state and the command fails if any terminal value
differs from the first result. Use `--threads 1` when comparing instruction
cost without scheduler contention, then repeat with representative thread
counts to measure host throughput. The accepted range is 1 through 256; when
there are fewer dispatch iterations than requested threads, the command starts
one worker per iteration.

The command reports the first compile separately as the cold compile sample for
that process, then compiles the same source repeatedly for steady-state
statistics. Parallel dispatch workers wait at a barrier after they are ready;
batch wall time and throughput begin when the coordinator releases that
barrier. Per-dispatch samples still measure each independent kernel call.

Portable receipts deliberately replace the full-VM profiler for this command.
`harn bench portable` rejects `--profile`, `--profile-json`, `HARN_PROFILE`, and
`HARN_PROFILE_JSON` instead of accepting flags whose data would describe a
different execution boundary.

The receipt uses schema `harn.portable_kernel.benchmark.v1`; its checked JSON
Schema is `spec/schemas/portable-kernel-benchmark.v1.schema.json`. It is
generated with `make gen-portable-benchmark-schema` from the kernel-owned Rust
receipt contract and limits; `make check-portable-benchmark-schema` prevents a
consumer-facing file from becoming a second owner. The closed
Rust receipt type, validation limits, statistics, provenance, and terminal
digest live in `harn-kernel`. The CLI serializes that type directly, and the
browser worker must round-trip its measurements through the same Wasm-backed
type before publishing them. A schema parity test compares every object field
set and validates both target variants. The receipt records:

- target and build/runtime provenance, including artifact, semantic ABI, and
  opcode ABI versions or fingerprints;
- source path, entry name, and kind;
- artifact byte length and digest;
- sample and worker counts;
- initialization latency when the target has an initialization boundary;
- first and repeated compile statistics;
- decode statistics when decode is independently measurable;
- first and repeated dispatch statistics, batch wall time, and throughput; and
- a digest of the terminal value.

Repeated statistics use the kernel's canonical R-7 percentiles and population
standard deviation. Their JSON fields are `iterations`, `min_ms`, `mean_ms`,
`p50_ms`, `p95_ms`, `max_ms`, `stddev_ms`, and `total_ms`; receipt envelope
fields use camelCase.

The host measures these durations with a monotonic clock. Clock access is not
granted to the Harn program and is not embedded in the artifact.

## Measure the browser path

First produce the release adapter and run its worker conformance checks:

```console
make wasm-check
```

Then start the local server with `make wasm-demo` and open
`http://127.0.0.1:8765/demo/benchmark.html`. A dedicated module worker records
Wasm initialization, first and repeated compilation, and 500 calls through the
same generated `compile` and `start` exports used by the reducer worker. The
page only presents the worker's result. Read the JSON receipt from the page or
`window.__HARN_BENCHMARK__`.

Every repeated compile must produce the same artifact bytes and digest. Every
start receives the same immutable artifact and JSON input and must produce the
same terminal value. The worker sends timing samples through the Wasm adapter's
bounded projection of `harn_kernel::BenchmarkStatistics`, so native and browser
receipts use one percentile and standard-deviation definition.

Browser instances run in dedicated Web Workers. To measure parallel browser
throughput, create several workers and give each worker the same artifact bytes
and independent input state. Portable Kernel v1 does not require
`SharedArrayBuffer`, Wasm atomics, JavaScript Promise Integration, or Wasm
threads.

## Interpret the numbers

Do not combine initialization, compile, decode, and dispatch into one latency
claim. Browser initialization is a deployment cost; first compilation is a cold
frontend cost; repeated compilation shows frontend steady state.

Target boundaries differ. Native `dispatch` calls the kernel with an already
decoded `ProgramArtifact`, `DataValue`, and `GrantSet`; native decode is reported
separately. Browser `dispatch` measures the public `start(artifactBytes,
inputJson, grantsJson)` adapter boundary, so it includes artifact decoding,
JSON/grant adaptation, kernel execution, and result projection. Treat it as
adapter-start latency, not as an exact comparison with decoded native dispatch.

Compare like-for-like release builds on the same machine and browser, and
preserve the receipt with the revision under test. Measurements from an earlier
candidate were removed after the compiler/executor changed; publish new numbers
only after rebuilding and exercising the final candidate.

## Choose the right profiling surface

`harn bench portable` measures the compiler/artifact/kernel boundary. It does
not emit full-VM phase profiles, user timing spans, provider timings, or tool
timings.

- Use `harn bench <file> --profile` or `--profile-json` to aggregate full native
  VM categories over repeated executions.
- Use `harn time run <file>` to attribute one full run to parse, typecheck,
  bytecode compilation, setup, main execution, and module work.
- Use [`std/timing`](./stdlib/timing.md) to add named application spans to a
  hostful Harn run and export them through the normal profile and OTel paths.
- Use `harn bench replay` for deterministic transcript and permission-fidelity
  measurements. It measures a different contract from portable dispatch.

Keeping these surfaces separate prevents host observation policy from becoming
part of portable execution semantics.
