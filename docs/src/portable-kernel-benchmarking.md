# Benchmark the portable kernel

Use `harn bench portable` to measure the canonical compiler, artifact decoder,
and native execution kernel separately. The command runs pure executions with
no host grants, checks that repeated compilation produces identical artifact
bytes, checks that every iteration produces the same terminal value, and can
distribute independent dispatches across operating-system threads.

## Prepare an input

The benchmark accepts one JSON value as the entry function's input. For the
package reducer in `crates/harn-wasm/demo/package-root.harn`, save this as
`/tmp/reducer-input.json`:

```json
{
  "state": {"count": 0, "history": [], "label": "portable"},
  "event": {"kind": "increment", "amount": 1}
}
```

## Run a native benchmark

```console
harn bench portable crates/harn-wasm/demo/package-root.harn \
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
same generated `compilePackage` and `start` exports used by the reducer worker. The
page only presents the worker's result. Read the JSON receipt from the page or
`window.__HARN_BENCHMARK__`.

Every repeated compile must produce the same artifact bytes and digest. Every
start receives the same immutable artifact and JSON input and must produce the
same terminal value. The worker sends timing samples through the Wasm adapter's
bounded projection of `harn_kernel::BenchmarkStatistics`, so native and browser
receipts use one percentile and standard-deviation definition.

Browser instances run in dedicated Web Workers. To measure parallel browser
throughput, create several workers and give each worker the same artifact bytes
and independent input state. The portable kernel does not require
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

## Measured cutover snapshot

These measurements are evidence for this implementation boundary, not a
performance promise. They were collected on 2026-08-02 on an Apple M5 Pro
(18 logical CPUs, 48 GiB), macOS 26.5.1, Chrome 150.0.7871.187, and
`wasm-pack 0.15.0`. Both adapters used release builds; their measured boundaries
still differ as described above.

| Release browser module | Raw Wasm | gzip -9 | SHA-256 |
|---|---:|---:|---|
| Portable kernel browser module | 3,505,267 bytes | 1,231,341 bytes | `d1618ef72d386dbc738ec4693118044d87baad9e0ced2d6d423e6f74a24e457d` |

The shared Unicode regex, secret catalog, hashing, package compiler, and safety
machinery add 1,120,115 raw bytes and 407,484 gzip bytes over the earlier pure
reducer baseline. No file, process, network, clock, randomness, or model import
appears in the module; `make wasm-audit-imports` enforces that boundary.

One fresh dedicated-worker receipt for the generated two-module package
reported:

| Browser measurement | Observed value |
|---|---:|
| Wasm initialization | 57.1 ms |
| First package compile | 827.3 ms |
| Repeated package compile p50 / p95 | 42.05 / 2,476.25 ms |
| First execute/start | 1,480.1 ms |
| Repeated execute/start p50 / p95 | 4.10 / 35.46 ms |
| 500-call batch wall time | 10,916.6 ms |
| Batch throughput | 45.8 starts/s |

That manual Chrome receipt overlapped a CPU-heavy release build; its large
compile and dispatch outliers are an observed contention result, not a clean
latency baseline. A native release receipt collected after the build, for the
same source, input, 30 repeated compiles, 500 dispatches, and four host threads,
reported 3.526 ms first compile, 2.021 / 2.379 ms repeated compile p50 / p95,
1.141 / 1.428 ms decode p50 / p95, 0.0053 / 0.0077 ms dispatch p50 / p95, and
290,114 dispatches/s. Both adapters produced the exact 2,256-byte artifact with
digest `42765c0eb8bd02a0ed15435f19743f838c0cfc40566647448660425c6f693c5a`.
The matching bytes are parity evidence; the timing difference reflects distinct
adapter boundaries and browser contention.

The browser `start(artifactBytes, inputJson, grantsJson)` measurement includes
artifact decoding, validation, JSON conversion, grant adaptation, execution,
and result projection on every call. Preserve complete receipts and repeat
trials before using these observations for optimization decisions.
