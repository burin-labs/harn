# CLI reference

All commands available in the `harn` CLI.

To add a new subcommand or port an existing one off Rust, see
[Extending the CLI in `.harn`](./cli-extending-in-harn.md). For the
machine-readable side of `--json` modes, see the
[`harn --json` contract](./cli-json-contract.md).

## harn run

Execute a `.harn` file.

```bash
harn run <file.harn>
harn run --trace <file.harn>
harn run --profile --profile-json profile.json <file.harn>
harn run -e 'log("hello")'
harn run --deny shell,exec <file.harn>
harn run --allow read_file,write_file <file.harn>
harn run --no-sandbox <file.harn>
harn run --write-root /path/to/output main.harn
harn run --read-only-root /path/to/other-repo main.harn
harn run --yes <file.harn>
harn run --explain-cost <file.harn>
harn run --attest <file.harn>
harn run --attest --receipt-out receipt.json <file.harn>
harn run <bundle.harnpack>
harn run --dry-run-verify <bundle.harnpack>
harn run --allow-unsigned <bundle.harnpack>
harn run --resume .harn/workers/worker_...json
```

| Flag | Description |
|---|---|
| `--trace` | Print LLM trace summary after execution. Can also be set with `HARN_TRACE=1` |
| `--profile` | Print a categorical timing breakdown after execution. Can also be set with `HARN_PROFILE=1` |
| `--profile-json <path>` | Write the categorical timing breakdown as JSON. Can also be set with `HARN_PROFILE_JSON=<path>` |
| `--explain-cost` | Print static LLM token/cost estimates without executing the script |
| `-e <code>` | Evaluate inline code instead of a file |
| `--resume <handle-or-snapshot>` | Cold-restore a suspended top-level agent from its persisted worker snapshot |
| `--deny <builtins>` | Deny specific builtins (comma-separated) |
| `--allow <builtins>` | Allow only specific builtins (comma-separated) |
| `--no-sandbox` | Disable the default worktree filesystem/process sandbox and network side-effect ceiling |
| `--allow-process-network` | Permit spawned commands to open network sockets while retaining the worktree filesystem/process sandbox |
| `--write-root <path>` | Write to an extra filesystem root while keeping sandboxing enabled |
| `--read-only-root <path>` | Read from an extra filesystem root while keeping sandboxing enabled |
| `--yes` | Accept first-run provider setup prompts, including local Ollama config seeding |
| `--attest` | Emit a signed provenance receipt after execution |
| `--receipt-out <path>` | Write the receipt to a specific JSON path |
| `--attest-agent <id>` | Agent id used to load or create the Ed25519 signing key |
| `--json` | Emit a versioned NDJSON event stream on stdout instead of mixed pipeline output |
| `--quiet` | When `--json` is set, drop `stdout` and `stderr` events (transcript/tool/hook/persona/result still flow) |
| `--emit-summary-json` | Emit one terminal `run_summary` JSON object as a single NDJSON line; defaults to stderr |
| `--summary-file <path>` | Write `--emit-summary-json` output to a file instead of stderr |
| `--summary-fd <fd>` | Write `--emit-summary-json` output to an already-open Unix file descriptor |
| `--emit-phase-json` | Emit one terminal `run_phase` JSON object as a single NDJSON line; defaults to stderr |
| `--phase-file <path>` | Write `--emit-phase-json` output to a file instead of stderr |
| `--phase-fd <fd>` | Write `--emit-phase-json` output to an already-open Unix file descriptor |
| `--emit-rusage-json` | Emit one terminal `run_rusage` JSON object as a single NDJSON line; defaults to stderr |
| `--rusage-file <path>` | Write `--emit-rusage-json` output to a file instead of stderr |
| `--rusage-fd <fd>` | Write `--emit-rusage-json` output to an already-open Unix file descriptor |
| `--allow-unsigned` | When running a `.harnpack`, accept bundles that carry no Ed25519 signature (local-dev override) |
| `--dry-run-verify` | When running a `.harnpack`, verify the signature and replay into the cache without executing the entrypoint |

### `--json` event stream

`harn run --json <file>` writes one `JsonEnvelope` per line to stdout,
each carrying a typed `RunEvent`. The stream is strictly ordered via a
monotonic `seq` (starts at `1`) so a downstream agent can reconstruct
the run without parsing prose. The envelope's `schemaVersion` is `1`;
see `harn --json-schemas` for the catalog entry.

Event types (the `event_type` discriminator lives at
`data.event_type`):

| `event_type` | Payload |
|---|---|
| `stdout` | `{ payload: string }` — bytes written via `print`/`println`/`log`, verbatim. |
| `stderr` | `{ payload: string }` — bytes written via `eprint`/`eprintln`. |
| `transcript` | `{ agent_id?: string, kind: string, payload: object }` — one entry from the LLM-call transcript stream. |
| `tool_call` | `{ call_id, name, args, started_at }` — model-issued tool invocation. |
| `tool_result` | `{ call_id, ok: bool, result }` — outcome of a tool invocation. |
| `hook` | `{ name, phase, payload? }` — session lifecycle hook fired during the run. |
| `persona_stage` | `{ persona, stage, transition }` — persona-stage transition (`started` / `completed` / `handoff_started` / …). |
| `pack_run` | `{ bundle_hash, signature_verified: bool, key_id?: string, cache_hit: bool, dry_run_verify: bool }` — emitted once when `harn run <bundle.harnpack>` resolves a pack to execute. |
| `result` | `{ value, exit_code: int }` — terminal event for successful runs. |
| `error` | `{ error: { code, message, details? } }` — terminal event when a fatal error prevents a `result`. |

The stream is line-flushed per event. Streaming consumers can pipe
through `jq -c .` for live filtering:

```bash
harn run --json examples/hello.harn | jq -c '.data | {seq, event_type}'
```

### Post-run summary JSON

`harn run --emit-summary-json <file>` emits one raw JSON object after
the run finishes. This is a separate opt-in sink from `harn run --json`
so consumers that need aggregate metrics can keep the event stream
contract unchanged. By default the line is appended to stderr after
human diagnostics; use `--summary-file <path>` or `--summary-fd <fd>`
to isolate it from the script's own stderr.

Shape (`schema_version: 1`):

```json
{
  "schema_version": 1,
  "event": "run_summary",
  "wall_time_ms": 1234,
  "exit_code": 0,
  "llm": {
    "call_count": 2,
    "input_tokens": 1024,
    "output_tokens": 256,
    "time_ms": 480,
    "cost_usd": 0.0042
  },
  "profile": {
    "total_wall_ms": 1234,
    "by_kind": [],
    "residual_ms": 12,
    "top_llm_calls": [],
    "top_tool_calls": [],
    "steps": []
  }
}
```

`profile` is present only when the run enables profiling with
`--profile` or `--profile-json`. The LLM metrics are collected whenever
summary JSON is requested, even without `--trace`.

### Post-run phase JSON

`harn run --emit-phase-json <file>` emits one raw JSON object after
the run finishes. This uses the same fixed seven-row contract as
`harn time run --json`, but keeps the data on a separate sink so a
wrapper can spawn `harn run` and recover parse/typecheck/compile/setup
timings without changing stdout. Use `--phase-file <path>` or
`--phase-fd <fd>` to isolate the line from stderr.

Shape (`schema_version: 2`):

```json
{
  "schema_version": 2,
  "event": "run_phase",
  "phases": [
    { "name": "parse", "kind": "top_level", "duration_ms": 12, "input_bytes": 4096 },
    { "name": "typecheck", "kind": "top_level", "duration_ms": 80 },
    { "name": "bytecode_compile", "kind": "top_level", "duration_ms": 35, "cache": "miss" },
    { "name": "run_setup", "kind": "top_level", "duration_ms": 8 },
    { "name": "run_main", "kind": "top_level", "duration_ms": 1200, "events": 14 },
    { "name": "module_compile", "kind": "attribution", "duration_ms": 40, "events": 3 },
    { "name": "module_load", "kind": "attribution", "duration_ms": 75, "events": 8 }
  ]
}
```

The phase array is always in this order: `parse`, `typecheck`,
`bytecode_compile`, `run_setup`, `run_main`, `module_compile`, `module_load`.
On a bytecode-cache hit,
`parse` and `typecheck` stay present with `duration_ms: 0`, and the
`bytecode_compile` row flips to `"cache": "hit"`. The final two attribution
rows overlap setup/main and are not additive.

### Post-run rusage JSON

`harn run --emit-rusage-json <file>` emits one raw JSON object after
the run finishes containing the total `getrusage(RUSAGE_SELF)` CPU time
sample for the process during that run. Use `--rusage-file <path>` or
`--rusage-fd <fd>` to isolate the line from stderr.

Shape (`schema_version: 1`):

```json
{
  "schema_version": 1,
  "event": "run_rusage",
  "cpu_ms": 320
}
```

You can also run a file directly without the `run` subcommand:

```bash
harn main.harn
```

By default, `harn run` installs a worktree sandbox before executing
the VM. Filesystem and subprocess cwd access are rooted at the nearest
`harn.toml` project root, or at the invocation working directory when
no project manifest is present. Network side effects are denied by the
same default policy. Use `--no-sandbox` only for scripts that need the
old unrestricted process behavior; use `--read-only-root` when a script
needs read access to a sibling or shared directory and should remain in
the sandboxed profile.
The CLI emits a warning when `--no-sandbox` is used, and rejects
`--read-only-root` when `--no-sandbox` is present.

Before starting the VM, `harn run <file>` builds the cross-module
graph for the entry file. When all imports resolve, unknown call
targets produce a static error and the VM is never started — the same
`call target ... is not defined or imported` message you see from
`harn check`.

The inline `-e <code>` form is wrapped in `pipeline main(task) { ... }`
and run as a temp file in the current directory, so:

- Leading `import "..."` (and `pub import { ... } from "..."`) lines
  are hoisted out of the wrapper. They must come first; imports that
  appear after another statement are not lifted.
- Relative imports resolve against the working directory:
  `harn run -e $'import "./lib"\nlog(answer())'` looks for
  `./lib.harn` next to where you invoked `harn`.
- Stdlib imports (`import "std/..."`) work the same as in a file.
- A read-only working directory falls back to the system temp dir;
  pure-expression `-e` still works there but relative imports won't
  resolve.

`--explain-cost` stops after the static pass. It scans direct `llm_call(...)`
and `agent_loop(...)` callsites, estimates statically known prompt/system input
tokens, applies literal `agent_loop` iteration caps, and reports unresolved
dynamic model or prompt expressions without executing pipeline code.

When `--attest` is present, Harn records run start/finish events in an
EventLog, stamps each record with `prev_hash` and `record_hash` provenance
headers, signs the receipt with an Ed25519 key loaded through the configured
secret provider chain, and writes the receipt under `.harn/receipts/` unless
`--receipt-out` is set.

### Running a `.harnpack`

`harn run <bundle.harnpack>` accepts a signed content-addressed bundle
produced by `harn pack`. Detection is by `.harnpack` extension and a
zstd magic-byte fallback so renamed bundles still resolve. The runner:

1. Reads the manifest and embedded contents.
2. Verifies the Ed25519 signature against the bundle hash through the
   OpenTrustGraph trust store.
3. Checks `harn_version` compatibility — patch mismatches warn, minor
   or major mismatches refuse the run.
4. Replays the archive into `$HARN_CACHE_DIR/packs/<bundle_hash>/`
   atomically (the bundle hash is content-addressed, so a second run
   reuses the unpacked tree).
5. Executes the manifest's entrypoint with `Harness::real()`.

The verifier refuses to run an unsigned bundle. Pass `--allow-unsigned`
to opt out (intended for local-dev only). `--dry-run-verify` performs
verification and replay but stops before execution, useful for
deployment gates.

## harn parse

Parse a `.harn` file without type-checking or running it.

```bash
harn parse main.harn
harn parse main.harn --json
harn parse examples/hello.harn --json | jq .data.kind
```

Text mode prints the debug AST. `--json` emits a `JsonEnvelope` with
`schemaVersion: 1`; `data.kind` is `Program`, `data.body[]` contains tagged
AST nodes, and each node carries a byte span.

## harn tokens

Tokenize a `.harn` file.

```bash
harn tokens main.harn
harn tokens main.harn --json
```

Text mode prints one token per line with line/column and byte ranges.
`--json` emits a `JsonEnvelope` with `schemaVersion: 1`; `data[]` entries are
`{ kind, lexeme, start, end, line, column }`. `start` and `end` are byte
offsets into the original source.

## harn verify

Verify a signed Harn provenance receipt.

```bash
harn verify .harn/receipts/<receipt>.json
harn verify receipt.json --json
```

Verification recomputes the EventLog record chain, the receipt event root, the
receipt hash, and the Ed25519 signature. It exits non-zero if any receipt event
or signature material has been changed.

## harn session

Export, validate, import, and check canonical session bundles.

```bash
harn session export .harn-runs/<run>/run.json --out run.bundle.json
harn session export .harn-runs/<run>/run.json --local --out run.local.bundle.json
harn session export .harn-runs/<run>/run.json --replay-only --out run.replay.bundle.json
harn session validate run.bundle.json --json
harn session import run.bundle.json --out imported-run.json
harn session schema --check
```

| Subcommand | Description |
|---|---|
| `export <run-record>` | Writes a `harn_session_bundle` envelope. Default output is sanitized; `--local` preserves local-only content; `--replay-only` withholds prompt/tool payload fields. |
| `validate <bundle>` | Validates required fields, schema version, bundle type, and high-confidence secret markers without writing a run record. |
| `import <bundle>` | Validates the bundle and materializes a local run record from `replay.run_record` or replay metadata. |
| `schema` | Prints, writes, or checks `spec/schemas/session-bundle.v1.schema.json`. |

`export --include-attachments` copies artifact payloads into the top-level
`attachments` array. By default attachment payloads are omitted for share
safety. `import` and `validate` accept
`--allow-unsafe-secret-markers` only for trusted local bundles.

## harn completion

Print shell completion scripts.

```bash
harn completion bash
harn completion zsh
harn completion fish
```

Generated completions include subcommands plus static candidates for known
provider and model values.

## harn config

Inspect, validate, and emit schemas for layered runtime configuration.

```bash
harn config inspect
harn config inspect --explain
harn config inspect --config ./harn.config.toml --managed ./org-policy.toml --explain
harn config validate ./harn.config.toml
harn config schema --output docs/src/schemas/harn-config.schema.json
```

| Command | Description |
|---|---|
| `inspect` | Print the redacted merged runtime config |
| `inspect --explain` | Include per-field provenance, shadowed candidates, and managed policy lock status |
| `validate` | Validate local, project, or managed overlays against the typed config shape |
| `schema` | Print the editor JSON Schema for `harn.config.toml` |

See [Layered runtime configuration](./configuration.md) for precedence, file
locations, environment override names, and managed policy examples.

## harn playground

Run a pipeline against a Harn-native host module for fast local iteration.
`harn try` is an alias for the same command.

```bash
harn playground --host host.harn --script pipeline.harn --task "Explain this repo"
harn playground --watch --task "Refine the prompt"
harn playground --llm ollama:qwen2.5-coder:latest --task "Use a local model"
harn try "Use local Ollama"
```

| Flag | Description |
|---|---|
| `--host <file>` | Host module exporting the functions the script expects (default: `host.harn`) |
| `--script <file>` | Pipeline entrypoint to execute (default: `pipeline.harn`) |
| `--task <text>` | Task string exposed as `HARN_TASK` during the run |
| `--llm <provider:model>` | Override the provider/model selection for this invocation |
| `--llm-mock <path>` | Replay LLM responses from a JSONL fixture file instead of calling the provider |
| `--llm-mock-record <path>` | Record executed LLM responses into a JSONL fixture file |
| `--yes` | Accept first-run provider setup prompts, including local Ollama config seeding |
| `--watch` | Re-run when the host module or script changes |

`harn playground` type-checks the host module, merges its exported function
names into the script's static call-target validation, then executes the script
with an in-process host adapter. Missing host functions fail with a pointed
error naming the function and caller location.

## harn test

Run tests.

```bash
harn test conformance                  # run conformance test suite
harn test conformance tests/language/arithmetic.harn # run one conformance file
harn test conformance tests/stdlib/     # run a conformance subtree
harn test tests/                       # run user tests in directory
harn test tests/ --filter "auth*"      # filter by pattern
harn test tests/ --parallel            # run tests concurrently with bounded workers
harn test tests/ --parallel --timing   # show progress and slowest tests/files
harn test tests/ --parallel -j 4       # pin worker count (also via HARN_TEST_JOBS)
harn test tests/ --watch               # re-run on file changes
harn test conformance --verbose        # show per-test timing
harn test conformance --timing         # show timing summary without verbose failures
harn test tests/ --record              # record LLM fixtures
harn test tests/ --replay              # replay LLM fixtures
harn test tests/ --coverage            # print per-file line coverage
harn test tests/ --coverage-out lcov.info  # also write an LCOV tracefile
harn test agents-conformance --target http://localhost:8080 --api-key "$KEY"
```

Watch mode keeps immutable prepared module artifacts warm between reruns. Each
test still receives a fresh VM, module state, and persistence root.

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests matching pattern |
| `--target <url>` | Harness base URL for `harn test agents-conformance` |
| `--api-key <key>` | Bearer API key for `harn test agents-conformance` |
| `--category <name>` | Agents conformance category to run; repeatable or comma-separated |
| `--json` | Emit conformance results as JSON to stdout, or the agents-conformance leaderboard report |
| `--json-out <path>` | Write user-test results (or the agents-conformance report) to a JSON file; user-test schemaVersion 3 includes typed timeout, phase, aggregate, latency-distribution, and captured-output data |
| `--workspace-id <id>` / `--session-id <id>` | Reuse existing Harness resources for agents conformance setup |
| `--parallel` | Run tests concurrently with a bounded worker pool. Slow tests are front-loaded using historical timings from `.harn/test-timings.json` |
| `--jobs <N>` / `-j <N>` | Maximum concurrent workers (also `HARN_TEST_JOBS`). Defaults to available parallelism, capped at 8 |
| `--watch` | Re-run tests on file changes (mutually exclusive with `--junit` / `--json-out`) |
| `--verbose` / `-v` | Show per-test timing and detailed failures, including a passing test's captured `log`/`print`/`println` output (a failing test's captured output is always shown) |
| `--timing` | Show detailed per-test timing, slowest tests/files, and phase totals. Every user-test run prints the concise p50/p90 latency line |
| `--junit <path>` | Write JUnit XML report for user tests or conformance; missing or unwritable destinations fail loudly. A failing user test's captured output is included as `<system-out>` |
| `--timeout <ms>` | Per-test timeout in milliseconds (default: 30000). For user suites, setup is measured separately and does not consume the pipeline-execution budget; other targets bound their test case or subprocess |
| `--max-test-ms <ms>` | Fail a passing test whose total setup + execution wall time exceeds the budget |
| `--max-execute-ms <ms>` | Fail a passing test whose measured execution phase exceeds the performance budget |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay recorded LLM responses |
| `--coverage` | Print per-file line coverage for executed Harn source (user test suites only) |
| `--coverage-out <path>` | Write an LCOV tracefile (implies `--coverage`); consumable by Codecov, `genhtml`, and Coverage Gutters |

When no path is given, `harn test` auto-discovers a `tests/` directory
in the current folder. Conformance targets must resolve to a file or directory
inside `conformance/`; the CLI now errors instead of silently falling back to
the full suite when a requested target is missing.

## harn repl

Start an interactive REPL with syntax highlighting, multiline editing, live
builtin completion, and persistent history in `~/.harn/repl_history`.

```bash
harn repl
```

The REPL keeps incomplete blocks open until braces, brackets, parentheses, and
quoted strings are balanced, so you can paste or type multi-line pipelines and
control-flow blocks directly.

## harn bench

Benchmark a `.harn` file over repeated runs, or score deterministic replay
fixtures.

```bash
harn bench main.harn
harn bench main.harn --iterations 25
harn bench main.harn --iterations 25 --profile-json bench.json
harn bench replay --json --output replay-benchmark.json
harn bench replay conformance/replay-oracle/fixtures --filter approval --json
```

`harn bench` parses and compiles the file once, executes it with a fresh VM for
each iteration, and reports wall time plus aggregated LLM token, call, and cost
metrics. The wall-time rollup includes min, mean, p50, p95, max, stddev, and
total. `--profile` prints the aggregate categorical timing rollup; `--profile-json`
writes a JSON report with `iterations[]`, `mean_ms`, `p50_ms`, `p95_ms`,
`stddev_ms`, and `rollup`. `HARN_PROFILE=1` and `HARN_PROFILE_JSON=<path>` work
as environment aliases.

`harn bench replay` reads `benchmarks/replay/suite.json` by default. It emits
schema `harn.replay_benchmark.report.v1`, including cloud-platform ingest metadata,
fixture receipts, replay-fidelity scores, permission-preservation scores,
tool-call drift counts, transcript drift counts, observed interaction totals,
and first-divergence triage data. Use `--output <path>` to write the JSON report
and `--json` to print it to stdout. The command exits non-zero when any fixture
fails its expected replay result.

External trace pairs can be adapted with the documented OpenCode-inspired JSONL
adapter:

```bash
harn bench replay \
  --adapter opencode-jsonl \
  --external-first benchmarks/replay/adapters/opencode/first.jsonl \
  --external-second benchmarks/replay/adapters/opencode/second.jsonl \
  --external-name opencode-permission-run \
  --json
```

## harn time

Wrap a subcommand with phase-level wall-clock timing plus per-LLM-call
and per-tool-call latency. Today only `harn time run` is supported.

```bash
harn time run main.harn
harn time run main.harn --json
harn time run main.harn --json --no-cache
harn time run main.harn --no-sandbox
harn time run main.harn --write-root /path/to/output
harn time run -e 'log("hi")' --json
harn time run script.harn -- arg1 arg2
```

The output is keyed by seven fixed rows — `parse`, `typecheck`,
`bytecode_compile`, `run_setup`, `run_main`, `module_compile`, and
`module_load` — even when a bytecode-cache
hit lets us skip parse/typecheck. The `bytecode_compile` row carries a
`cache: "hit" | "miss"` field so cost-and-perf eyeballs and agent
pipelines can dispatch on cache state without a separate flag.
Rows carry `kind: "top_level" | "attribution"`; only top-level rows reconcile
wall time. `module_compile` and `module_load` attribute work that overlaps
`run_setup` and `run_main`; do not add them to the five top-level rows.
Their `events` values count successful compiles and unique fresh-VM loads.

`--json` emits a versioned `JsonEnvelope` (schema registered as
`time run` in `harn --json-schemas`):

```json
{
  "schemaVersion": 2,
  "ok": true,
  "data": {
    "command": "run",
    "target": "main.harn",
    "phases": [
      { "name": "parse", "kind": "top_level", "duration_ms": 12, "input_bytes": 4096 },
      { "name": "typecheck", "kind": "top_level", "duration_ms": 80 },
      { "name": "bytecode_compile", "kind": "top_level", "duration_ms": 35, "cache": "miss" },
      { "name": "run_setup", "kind": "top_level", "duration_ms": 8 },
      { "name": "run_main", "kind": "top_level", "duration_ms": 1200, "events": 14 },
      { "name": "module_compile", "kind": "attribution", "duration_ms": 40, "events": 3 },
      { "name": "module_load", "kind": "attribution", "duration_ms": 75, "events": 8 }
    ],
    "llm_calls": [
      { "model": "claude-sonnet-5", "latency_ms": 850, "tokens": 1500 }
    ],
    "tool_calls": [{ "name": "mcp_call", "latency_ms": 200 }],
    "totals": {
      "wall_ms": 1335,
      "cpu_ms": 320,
      "cache_hits": 0,
      "cache_misses": 1
    },
    "exit_code": 0
  }
}
```

In `--json` mode the wrapper hands stdout to the envelope: any script
output is redirected to stderr so `harn time run … --json | jq …` works
without filtering. Diagnostics and non-zero exit codes from the wrapped
run still propagate through `harn time`. `--no-cache` sets
`HARN_BYTECODE_CACHE=0` for the duration of the invocation only,
forcing a cold parse/typecheck/compile. `--no-sandbox` is forwarded to
the wrapped `harn run` path.

## harn viz

Render a `.harn` file as a Mermaid flowchart.

```bash
harn viz main.harn
harn viz main.harn --output docs/graph.mmd
```

`harn viz` parses the file, walks the AST, and emits a Mermaid `flowchart TD`
graph showing pipelines, functions, branches, loops, and other workflow-shaped
control-flow nodes.

## harn flow

Inspect and operate the Flow shipping substrate.

```bash
harn flow replay-audit --store .harn/flow.sqlite --predicate-root . --touched-dir crates/harn-vm
harn flow replay-audit --since 2026-04-26 --fail-on-drift --json
harn flow ship watch --store .harn/flow.sqlite --predicate-root . \
  --touched-dir crates/harn-vm --mock-pr-out .harn/flow/mock-pr.json --json
harn flow archivist scan . --out .harn/flow/archivist-proposals.json --json
```

`replay-audit` compares predicate hashes pinned in derived slices with the
current `invariants.harn` predicate set. Drift is advisory unless
`--fail-on-drift` is present.

`ship watch` is the Phase 0 Ship Captain shadow-mode surface. It groups stored
atoms into intent summaries, derives a candidate slice, discovers applicable
`invariants.harn` predicates for the touched directories, persists a local
shipping receipt, and can write a mock PR receipt without touching a remote
GitHub repository.

`archivist scan` emits review-ready predicate proposal metadata from the repo's
stack hints and existing Flow predicates. It is propose-only; it does not edit
`invariants.harn`.

## harn persona

List, inspect, and control durable agent persona manifests from `harn.toml`.

```bash
harn persona list
harn persona list --json
harn persona inspect merge_captain
harn persona inspect merge_captain --json
harn persona activate agents/reviewer --autonomy-tier suggest --json
harn persona activations --json
harn persona deactivate agents/reviewer --json
harn persona --manifest examples/personas/harn.toml inspect merge_captain --json
harn persona --manifest examples/personas/harn.toml status merge_captain --json
harn persona --manifest examples/personas/harn.toml tick merge_captain --json
harn persona --manifest examples/personas/harn.toml trigger merge_captain \
  --provider github --kind pull_request \
  --metadata repository=burin-labs/harn --metadata number=462 --json
harn persona --manifest examples/personas/harn.toml pause merge_captain
harn persona --manifest examples/personas/harn.toml resume merge_captain
harn persona --manifest examples/personas/harn.toml disable merge_captain
harn persona --manifest examples/personas/harn.toml supervision tail --json
harn persona --manifest examples/personas/harn.toml supervision tail \
  --persona merge_captain --since-event-id 42 --follow --limit 200 --json
```

| Flag | Description |
|---|---|
| `--manifest <path>` | Use an explicit `harn.toml` path or directory containing one |
| `--state-dir <dir>` | Store persona runtime events under a durable EventLog base directory, default `.harn/personas`; installed-persona activations always remain project-scoped beside the root manifest |
| `--json` | Emit stable JSON for list, inspect, status, controls, trigger, tick, and budget receipts. `supervision tail` always emits NDJSON frames and accepts `--json` for host symmetry |
| `--persona <name>` | Filter `supervision tail` to one persona; omitted streams every persona in the local state directory |
| `--since-event-id <N>` | Replay `supervision tail` frames with `event_id > N` |
| `--follow` | Keep `supervision tail` open and stream new appended frames |
| `--limit <N>` | Cap emitted `supervision tail` frames |

`harn persona` validates the manifest before printing. It rejects missing entry
workflows, unknown capabilities, invalid budget fields, invalid schedules, and
handoffs that point at undeclared personas.

Installed package personas are addressed as `<package-alias>/<persona-name>`.
`activate` writes an atomic project record that pins package and policy digests;
its optional autonomy, tool, and capability flags may only reduce runtime
authority. Budget, model, and receipt policies remain inherited and are pinned
as part of the full exported persona contract; package permissions and host
requirements remain install-time contracts. `activations` lists those records and
`deactivate` removes one even after its package has been removed. Package
installation alone never activates a persona.

Runtime commands append event-sourced lifecycle records to
`persona.runtime.events`. `pause` queues matching trigger events,
`resume` drains queued events once under a lease, and `disable` records later
events as dead-lettered. `tick`, `trigger`, and `spend` enforce per-persona
daily, hourly, run, and token budgets before recording expensive-work receipts.
`supervision tail` projects those records into hosted-compatible
`persona/update` NDJSON frames with `event_id`, `persona_id`, `persona_kind`,
optional `persona_version`, `actor`, `update_kind`, `occurred_at`, and
`payload`.

## harn pg codegen

Generate Harn record types from a directory of `.sql` migrations so
Postgres query results can be type-checked without a live database.

```bash
harn pg codegen --dir migrations
harn pg codegen --dir migrations --out src/db_types.harn
harn pg codegen --dir migrations --out src/db_types.harn --check
harn pg codegen --dir migrations --out src/db_types.harn --suffix Record
```

Replays every forward migration (`.sql`, excluding `.down.sql`) in
lexicographic order — the same discovery rule `pg_migrate` uses —
applying `CREATE TABLE`, `ALTER TABLE … ADD/DROP/ALTER/RENAME COLUMN`,
`RENAME TO`, and `DROP TABLE`, then emits one `type <Table>Row = {…}`
per table mirroring the live schema. Prints to stdout unless `--out` is
given. `--check` (requires `--out`) verifies the file is current without
writing and exits non-zero on drift — wire it into CI. See
[Postgres → Typed rows from migrations](./postgres.md#typed-rows-from-migrations).

## harn fmt

Format `.harn` source files. Accepts files or directories.

```bash
harn fmt main.harn
harn fmt src/
harn fmt --check main.harn            # check mode (no changes, exit 1 if unformatted)
harn fmt --line-width 80 main.harn    # custom line width
harn fmt --separator-width 60 main.harn # fixed section-header separator width
```

| Flag | Description |
|---|---|
| `--check` | Check mode: exit 1 if any file would be reformatted, make no changes |
| `--line-width <N>` | Maximum line width before wrapping (default: 100) |
| `--separator-width <N>` | Fixed total width for normalized `// ----` section-header separators (default: `line-width` minus indent) |

The formatter enforces a **100-character line width** by default (overridable with `--line-width`). When a line exceeds
this limit the formatter wraps it automatically:

`harn fmt` also normalizes optional semicolon-separated statements back to the canonical
newline-separated style. Semicolons are accepted as input syntax in statement-list
contexts, but they are not preserved in formatter output.

- **Comma-separated forms** — function call arguments, function declaration
  parameters, list literals, dict literals, struct construction fields,
  enum constructor payloads, selective import names, interface method
  parameters, and enum variant fields all wrap with one item per line and
  trailing commas.
- **Binary operator chains** — long expressions like `a + b + c + d` break
  before the operator. Operators that the parser cannot resume across a bare
  newline (`-`, `in`, `not in`) get an automatic backslash continuation (`\`);
  other operators (`+`, `*`, `/`, `%`, `||`, `&&`, `|>`, `==`, `!=`, `<`,
  `>`, `<=`, `>=`, `??`) break without one.
- **Operator precedence parentheses** — the formatter inserts parentheses
  to preserve semantics when the AST drops them (e.g. `a * (b + c)` stays
  parenthesised) and for clarity when mixing `&&` / `||`
  (e.g. `a && b || c` becomes `(a && b) || c`) or when `??` is nested inside
  looser binary expressions (e.g. `x == y ?? false` becomes
  `x == (y ?? false)`).

### Trailing comments and line width

A trailing (same-line) comment is treated as an **unbreakable token that does
not count toward the line width**. This matches `rustfmt`, Prettier, and
`gofmt`, and is the least-surprising behaviour:

- The formatter **never relocates** a trailing comment to its own line, and
  **never reflows code to make room** for one. If `let x = compute() // why`
  exceeds the width *because of the comment*, the line is simply left long.
- Code still wraps on its **own** merits. The wrap decision is based on the
  code alone; the comment is appended afterward, riding the **last** physical
  line of a wrapped construct (e.g. the closing `]` of a wrapped list literal).

There is no separate "line too long" lint — width is a formatter concern, so a
trailing comment that overflows is never reported as a diagnostic.

## harn lint

Lint one or more `.harn` files or directories for common issues (unused
variables, unused functions, dead code after terminating statements,
pointless comparisons, redundant clones, empty blocks, missing `/** */`
HarnDoc on public functions, etc.).

`harn lint` also walks `.harn.prompt` (and bare `.prompt`) files in the
same pass and applies template-specific drift-prevention rules
(`template-provider-identity-branch`,
`template-variant-explosion` — see `docs/src/prompt-templating.md`).
Project lint rules are discovered from `[rules]` in `harn.toml`: declarative
`language = "harn"` TOML rules and `*.lint.harn` script rules use `ruleDirs`,
while trusted native dynamic libraries use `nativeRuleDirs`.

```bash
harn lint main.harn
harn lint src/ tests/
harn lint prompts/system.harn.prompt
harn lint --require-public-api-types src/
```

`--require-public-api-types` reports every untyped public function or pipeline
parameter and return as `HARN-LNT-067`. Set
`[lint] require_public_api_types = true` in `harn.toml` to apply the same policy
to CLI and LSP linting across the project. The rule has no autofix because
choosing a public contract is an API-design decision.

Pass `--fix` to automatically apply safe fixes (e.g., `var` → `let` for
never-reassigned bindings, boolean comparison simplification, unused import
removal, string interpolation conversion, `let`-then-`return` simplification,
and removing redundant `clone()`/`to_string(...)`/`to_int(...)`/`to_list(...)`
wrappers or unnecessary parentheses around single values):

```bash
harn lint --fix main.harn
```

The Harn LSP also exposes these autofixes as quick-fix code actions
(Cmd+./Ctrl+. in most editors) and as a bulk `source.fixAll.harn`
action that VS Code can run on save:

```jsonc
"[harn]": {
  "editor.codeActionsOnSave": { "source.fixAll.harn": "always" }
}
```

## harn fix

Emit a repair plan for one `.harn` file or directory, or apply clean
machine-applicable repairs under an explicit safety ceiling. Plan mode runs
the same type-check, lint, and preflight diagnostic passes used by
`harn check`, keeps diagnostics that carry a registered repair classifier, and
reports the concrete `FixEdit` edits when a diagnostic already has a
machine-applicable fix.

```bash
harn fix --plan main.harn
harn fix --plan --json main.harn
harn fix --plan --json --safety behavior-preserving src/
harn fix --apply --safety behavior-preserving main.harn
harn fix --apply --dry-run --json --safety scope-local src/
harn fix --apply --safety surface-changing --harness-threading thread-params src/
```

`--harness-threading` controls how ambient-capability migrations satisfy call
sites that do not already have a local `Harness` parameter. The default,
`local-global`, rewrites ambient builtins to the VM-level `harness` binding and
preserves helper signatures. `thread-params` adds `harness: Harness` parameters
and updates same-file callers; use it when you explicitly want public signature
threading.

`--json` returns a `RepairPlan` with `schemaVersion: 2`, `harnessThreading`,
`diagnostics[]`, `repairs[]`, `skippedFiles[]`, and `safetyLevels[]`. Each
repair includes the diagnostic code, repair `{id, summary, safety}`, impact
metadata, candidate edits, `applies_cleanly`, and `conflicts_with` indexes for
overlapping edit ranges. Ambient Harness repairs classify local rewrites
separately from public signature changes and flag repairs that require
cross-module caller updates. Directory runs continue after read, lex, or parse
failures; those files appear in `skippedFiles[]` with `reason` and diagnostic
details, and the command exits nonzero after processing the remaining parseable
files. Apply mode requires
`--safety <format-only|behavior-preserving|scope-local|surface-changing|capability-changing>`;
`needs-human` repairs are propose-only and are never auto-applied. `--apply
--json` returns `schemaVersion`, `applied[]`, `skipped[]`, `skippedFiles[]`,
and `post_apply_diagnostics_count`. `--dry-run` reports the same apply set
without writing files.

## harn check

Type-check one or more `.harn` files or directories and run preflight
validation without executing them. The preflight pass resolves imports, checks
literal `render(...)` / `render_prompt(...)` targets, detects import symbol collisions across
modules, validates `host_call("capability.operation", ...)` capability
contracts, and flags missing template resources, execution directories, and worker repos that would
otherwise fail only at runtime. Source-aware lint rules run as part of
`check`. (The `missing-harndoc` warning for undocumented `pub fn` APIs is
opt-in via `[lint] require_docstrings = true` in `harn.toml`.)

`check` builds a cross-module graph from each entry file and follows
`import` statements recursively. When every import in a file resolves,
the typechecker knows the exact set of names that module brings into
scope and will emit a hard error for any call target that is neither a
builtin, a local declaration, a struct constructor, a callable
variable, nor an imported symbol:

```text
error: call target `helpr` is not defined or imported
```

This catches typos and stale imports before the VM runs. If any import
in the file is unresolved, the stricter check is turned off for that
file so one broken import does not avalanche into spurious errors — the
unresolved import itself still fails at runtime.

```bash
harn check main.harn
harn check src/ tests/
harn check --host-capabilities host-capabilities.json main.harn
harn check --bundle-root .bundle main.harn
harn check --invariants main.harn
harn check --strict --strict-types src/
harn check --workspace
harn check --preflight warning src/
```

| Flag | Description |
|---|---|
| `--host-capabilities <file>` | Load a host capability manifest for preflight validation. Supports plain `{capability: [ops...]}` objects, nested `{capabilities: ...}` wrappers, and per-op metadata dictionaries. Overrides `[check].host_capabilities_path` in `harn.toml`. |
| `--bundle-root <dir>` | Validate `render(...)`, `render_prompt(...)`, and template paths against an alternate bundled layout root |
| `--invariants` | Evaluate `@invariant(...)` annotations on functions, tools, and pipelines. Violations fail the check and are reported as `invariant[<name>]` diagnostics with concrete source spans. |
| `--workspace` | Walk every path listed in `[workspace].pipelines` of the nearest `harn.toml`. Positional targets remain additive. |
| `--preflight <severity>` | Override preflight diagnostic severity: `error` (default, fails the check), `warning` (reports but does not fail), or `off` (suppresses all preflight diagnostics). Overrides `[check].preflight_severity`. |
| `--strict` | Treat every warning as a failure after rendering all files. Monotonically enables `[check].strict`; it never disables workspace strictness. |
| `--strict-types` | Fail on unvalidated boundary-API values used in field or subscript access. |

Files are checked on a parallel worker pool sized to the machine's available
cores, and resolved-module parsing is memoized for the whole run, so
whole-tree checks scale with the number of distinct modules rather than
`files x import closure`. Per-file output order and content match the serial
driver exactly. Set `HARN_CHECK_JOBS=<n>` to pin the pool size
(`HARN_CHECK_JOBS=1` restores fully serial checking, e.g. when bisecting a
diagnostic that depends on machine load). The module-graph build that
precedes checking is likewise parallel per import wave; pin it independently
with `HARN_MODULE_GRAPH_JOBS=<n>`.

Check results also persist across runs: each file's diagnostics are cached
under the shared Harn cache directory (`HARN_CACHE_DIR`, alongside the
bytecode cache), keyed by the file's content, its transitive import closure,
the effective `[check]` config plus CLI overrides, and the CLI build itself.
Preflight's filesystem lookups — template and prompt-asset paths, directory
targets — are recorded with each cached result and revalidated on every hit,
so creating, editing, or deleting a referenced file invalidates the cache
even though that file is not part of the import graph. A warm re-check of an
unchanged tree replays byte-identical output without re-running the
typechecker or linter. Set `HARN_CHECK_RESULT_CACHE=0` to disable just this
cache; `HARN_BYTECODE_CACHE=0` disables it together with the bytecode cache.

`--invariants` includes the capability-policy lattice:

```harn,ignore
@invariant("capability.policy",
  allow: "fs.write,process.exec,mcp.connector,llm.model",
  workspace: "src/**",
  require_approval: "fs.write",
  require_command_policy: "process.exec",
  require_egress_policy: "mcp.connector",
  require_budget: "llm.model",
)
fn release_worker(client) {
  const _approval = request_approval("edit", {capabilities_requested: ["fs.write"]})
  write_file("src/release.md", "ready")
  with_command_policy({deny: ["rm"]}, { -> exec("git status") })
  egress_policy({default: "deny", allow: ["api.github.com"]})
  mcp_call(client, "github.search", {query: "harn"})
  llm_call("summarize", nil, {budget: {max_output_tokens: 128}})
}
```

Canonical capability names are `fs.write`, `process.exec`,
`network.access`, `mcp.connector`, `llm.model`, `worker.dispatch`,
`human.approval`, and `autonomy.policy`.

### harn.toml — `[check]` and `[workspace]` sections

`harn check` walks upward from the target file (stopping at the first `.git`
directory) to find the nearest `harn.toml`. The following keys are honored:

```toml
[check]
# Load an external capability manifest. Path is resolved relative to
# harn.toml. Accepts JSON or TOML with the namespaced shape
# { workspace = [...], process = [...], project = [...], ... }.
host_capabilities_path = "./schemas/host-capabilities.json"

# Or declare inline:
[check.host_capabilities]
project = ["ensure_enriched", "enrich"]
workspace = ["read_text", "write_text"]

[check]
# Treat warnings from any check phase as failures. The one-shot equivalent is
# `harn check --strict`; the CLI flag can enable but never disable this setting.
strict = true

# Enable boundary-value type checks persistently. Combine the one-shot flags as
# `harn check --strict --strict-types` for a zero-warning type-safety gate.
strict_types = true

# Downgrade preflight errors to warnings (or suppress entirely with "off").
# Keeps type diagnostics visible while an external capability schema is
# still catching up to a host's live surface.
preflight_severity = "warning"

# Suppress preflight diagnostics for specific capabilities/operations.
# Entries match either an exact "capability.operation" pair, a
# "capability.*" wildcard, a bare "capability" name, or a blanket "*".
preflight_allow = ["mystery.*", "runtime.task"]

[workspace]
# Directories or files checked by `harn check --workspace`. Paths are
# resolved relative to harn.toml.
pipelines = ["pipelines", "scripts"]
```

Preflight diagnostics are reported under the `preflight` category so they
can be distinguished from type-checker errors in IDE output streams and
CI log filters.

## harn explain

`harn explain` dispatches on two forms behind one subcommand.

### Form 1 — explain a stable diagnostic code

```bash
harn explain HARN-TYP-014
harn explain HARN-TYP-014 --json
```

Pass any identifier registered in the in-binary diagnostic-code registry
(`HARN-<CAT>-<NNN>` — see [`crates/harn-parser/src/diagnostic_codes.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-parser/src/diagnostic_codes.rs)).
Text mode prints the embedded markdown explanation plus a curated
`See also:` list of related codes. `--json` emits a stable envelope an
agent or editor can ingest without parsing prose:

```json
{
  "schemaVersion": 1,
  "code": "HARN-OWN-001",
  "category": "OWN",
  "summary": "immutable binding is reassigned",
  "body": "# HARN-OWN-001 — ... full markdown ...",
  "repairs": [
    {
      "id": "bindings/make-mutable",
      "safety": "scope-local",
      "summary": "Mark the binding `mut` so it can be reassigned"
    }
  ],
  "related": [],
  "apiStability": "stable"
}
```

`schemaVersion` is the contract LSPs, IDEs, and hosted error-page mirrors
dispatch on; bumps will follow a deprecation cycle. `repairs` carries the
structured repair classifier — a namespaced kebab-case id plus a
six-level safety class (`format-only` → `behavior-preserving` →
`scope-local` → `surface-changing` → `capability-changing` →
`needs-human`) — that agents use to decide whether to auto-apply,
propose, or escalate a fix. Codes without a registered repair shape
return `"repairs": []`. An unknown code exits with status 2.

### Form 2 — explain a control-flow invariant violation

```bash
harn explain --invariant fs.writes write_patch main.harn
harn explain --invariant approval.reachability deploy_agent agent.harn
harn explain --invariant budget.remaining spend_budget budget.harn
```

This is the companion to `harn check --invariants`: `check` answers
whether a handler violates its declared contract, and `--invariant`-mode
`explain` shows the path that makes the violation reachable. It loads
the file, rebuilds the same handler IR used by `check`, and prints:

- the invariant name and handler
- the violation message plus any help text
- a numbered CFG path showing the source locations traversed to reach the
  violating call or assignment

If the handler does not exist or does not declare the requested
`@invariant(...)`, the command exits nonzero with a direct error message.

## harn contracts

Export machine-readable contracts for hosts, release tooling, and embedded
bundles.

```bash
harn contracts builtins
harn contracts host-capabilities --host-capabilities host-capabilities.json
harn contracts bundle main.harn --verify
harn contracts bundle src/ --bundle-root .bundle --host-capabilities host-capabilities.json
```

### harn contracts builtins

Print the parser/runtime builtin registry as JSON, including return-type hints
and alignment status.

### harn contracts host-capabilities

Print the effective host-capability manifest used by preflight validation after
merging the built-in defaults with any external manifest file.

### harn contracts bundle

Print a bundle manifest for one or more `.harn` targets. The manifest includes:

- explicit `entry_modules`, `import_modules`, and `module_dependencies` edges
- explicit `prompt_assets` and `template_assets` slices, plus a full `assets`
  table resolved through the same source-relative rules as `render(...)`
- required host capabilities discovered from literal `host_call(...)` sites
- literal execution directories and worker worktree repos
- a `summary` block with stable counts for packagers and release tooling

Use `--verify` to run normal Harn preflight validation before emitting the
bundle manifest and return a non-zero exit code if the selected targets are not
bundle-safe.

## harn init

Scaffold a new project with `harn.toml` and `main.harn`.

```bash
harn init              # create in current directory
harn init my-project   # create in a new directory
harn init --template eval
```

## harn new

Scaffold a new project from a starter template. Supported templates are
`basic`, `agent`, `chat`, `mcp-server`, `eval`, `pipeline-lab`, `package`, and
`connector`.

```bash
harn new my-agent --template agent
harn new my-chat --template chat
harn new local-mcp --template mcp-server
harn new eval-suite --template eval
harn new package my-lib
```

`harn init` and `harn new` share the same scaffolding engine. Use `init` for
the default quick-start flow and `new` when you want the template choice to be
explicit.

The `agent` template creates an `agent/` app layout with `instructions.md`,
`app.harn`, local skills, and convention folders for tools, subagents,
channels, sandbox policy, and schedules.

## harn tool

Scaffold Harn-native custom tool packages.

```bash
harn tool new acme-echo
harn tool new acme-echo --dir packages/acme-echo
harn tool new acme-echo --description "Echo text for tests."
```

The generated package includes `[[package.tools]]` metadata, a stable
`tools` export, package-local dispatch tests, API docs, and CI commands for
`harn test`, `harn package check`, `harn package docs --check`, and
`harn package pack --dry-run`.

## harn skill

Every skill operation lives under one `harn skill` noun: corpus discovery and
inspection, scaffolding, and provenance (signing, endorsement, verification,
trust).

```bash
# Corpus & discovery
harn skill list                      # canonical embedded corpus
harn skill get harn-language --full  # one skill's frontmatter (+ body)
harn skill dump --all                # write the corpus to disk
harn skill resolved                  # FS-resolved skills (project/user/host layers)
harn skill inspect deploy            # one resolved skill's files + metadata
harn skill match "deploy the app"    # rank skills against a prompt
harn skill install owner/repo        # cache a git/local skill for the resolver
harn skill new review-helper         # scaffold a new SKILL.md bundle

# Provenance
harn skill key generate --out signer.pem
harn skill sign SKILL.md --key signer.pem
harn skill endorse SKILL.md --key signer.pem
harn skill verify SKILL.md
harn skill who-signed SKILL.md
harn skill trust add --from https://example.com/signer.pem
```

## harn demo

Run a bundled, fully-offline scenario that demonstrates Harn end-to-end
without any API keys, project setup, or network access. Designed for
the cold-start "what does Harn actually do?" moment.

```bash
harn demo                         # interactive menu on a TTY; prints the list otherwise
harn demo merge-captain           # persona-supervised PR triage with structured receipts
harn demo review-captain          # HITL clarifying-question loop on a 5-file diff
harn demo provider-race           # latency-aware provider race with cost attribution
harn demo --list                  # one-line summary of every scenario
harn demo merge-captain --json    # machine-readable summary of the run
harn demo merge-captain --live    # re-run against the configured LLM provider
harn demo merge-captain --no-record  # skip writing a `.harn-runs/demo-*` record
```

Each scenario embeds a `.harn` script and a JSONL `--llm-mock` tape into
the `harn` binary, so every demo is reproducible across machines and
finishes in well under a second on the bundled tape. Successful runs
write a `run.json` under `.harn-runs/demo-<scenario>-<timestamp>/`
that the portal can inspect.

When `--live` is set the demo skips the bundled tape and routes through
your configured provider. If the failure looks like a missing/invalid
provider key the demo prints a hint to re-run without `--live` (or to
run `harn quickstart` to wire a provider).

## harn quickstart

Inspect local provider readiness and write starter LLM configuration.

```bash
harn quickstart
harn quickstart --non-interactive
harn quickstart --non-interactive --provider ollama --model llama3.2
```

Quickstart reports existing provider credentials, Ollama reachability, free disk
space, and GPU detection. It creates or updates:

- `~/.config/harn/providers.toml` or `HARN_PROVIDERS_CONFIG` when set
- `harn.toml` in the current directory
- `.env` in the current directory

To pick up the selected provider/model defaults in your shell, run
`source .env` before invoking Harn.

## harn doctor

One-command readiness check for new contributors and host integrators. The
command surfaces actionable, secret-free output for both humans and machine
consumers (IDE-host preflight, cloud-platform onboarding).

```bash
harn doctor                # local checks; skips remote provider probes by default
harn doctor --check-providers  # actively probe configured providers
harn doctor --json         # versioned machine-readable output
```

Each check reports a red/yellow/green status (`fail` / `warn` / `ok`, plus
`skip` when a check is not applicable) along with:

- A single-line `detail`.
- An optional `fix_command` that the user can copy-paste.
- An optional `docs_url` pointing at relevant documentation.
- A `blocks` array naming workflows that fail when this check fails. Stable
  values are `build`, `test`, `release`, `publish`, `portal`, `scripting`,
  and `editor`.

The command exits non-zero when at least one check is `fail`.

### What it checks

- **Toolchain** — `rustc`, `cargo` (FAIL when missing).
- **Optional dev tools** — `cargo-nextest`, `sccache`, `actionlint` (WARN
  when missing; each has a documented fallback).
- **Protocol artifacts** — when run inside the harn repo, compares the
  pinned `HARN_PROTOCOL_ARTIFACT_VERSION` in
  `spec/protocol-artifacts/harn-protocol.ts` against the binary's own
  version and FAILs on drift.
- **Portal frontend** — `node`, `npm`, and the portal's `node_modules`
  directory when run inside the repo.
- **Platform capabilities** — `notify` file-watcher backend availability and
  the system browser opener used by OAuth and `harn portal`.
- **Provider configuration** — `HARN_PROVIDERS_CONFIG`, `HARN_LLM_PROVIDER`,
  the resolved secret-provider chain, and per-provider env vars (printed by
  name only — never by value).
- **Manifest** — nearest `harn.toml`, declared MCP servers, and registered
  triggers.
- **Runtime state** — event log backend, metadata cache, loaded skills,
  Ollama presence and pulled models, hardware snapshot.
- **Provider connectivity** (`--check-providers` to include) — for OpenAI-compatible
  local providers, `/v1/models` probes parse the listing and report missing
  configured models instead of only checking HTTP reachability.

### JSON schema

The JSON document is versioned via the top-level `schema_version` string;
patch releases never break the documented field shapes.

```json
{
  "schema_version": "1",
  "harn_version": "0.8.4",
  "providers_config_path": "...",
  "model_defaults": { ... },
  "checks": [
    {
      "id": "rustc",
      "label": "rustc",
      "status": "ok",
      "detail": "rustc 1.84.0 (...)",
      "fix_command": "https://rustup.rs",
      "docs_url": "https://www.rust-lang.org/tools/install",
      "blocks": ["build", "test", "release", "publish"]
    }
  ],
  "summary": {
    "ok": 18, "warn": 2, "fail": 0, "skip": 1,
    "blocked_flows": []
  },
  "hardware": { "ram_gb": 64, "gpu": "Apple Silicon (MPS available)", "free_disk_gb": 240 },
  "next_step": "..."
}
```

Secrets never appear in any output. Credential checks list the env var
**names** they looked for, not the values.

## harn provider ready

Probe a configured provider's model inventory endpoint and optionally require a
specific model alias or provider-native model id. Harn uses a catalogued
model-list healthcheck such as `/models` or Ollama's `/api/tags`, or derives
the inventory URL from an OpenAI-compatible `/chat/completions` endpoint.

```bash
harn provider ready mlx --model mlx-qwen3.6
harn provider ready mlx --base-url http://127.0.0.1:8002 --json
```

The command exits non-zero for unsupported providers, unreachable servers, bad
HTTP status, unparsable model listings, and missing models. It does not run
local launcher scripts; host applications that auto-start local servers should
report launch failures themselves and then call this probe again.

## harn provider capabilities audit

Check that every priced chat model in the loaded provider catalog has an
explicit capability rule for both `native_tools` and `preferred_tool_format`.
The command exits non-zero and lists suggested defaults when catalog rows need
coverage.

`promote-from-eval` ingests the parity overlay emitted by
`harn eval coding-agent` and updates `crates/harn-vm/src/llm/capabilities.toml`
with exact-model `preferred_tool_format`, `tool_mode_parity`, and
`tool_mode_parity_notes` rows. The edit is idempotent.

```bash
harn provider capabilities audit
harn provider capabilities audit --json
harn provider capabilities promote-from-eval .harn-runs/coding-agent-bench/latest/tool_mode_parity_overlay.toml
```

## harn provider dispatch-explain

Explain the resolved provider dispatch configuration for one provider/model
route without making a network call. This is useful when checking why a route
uses a specific wire format, tool-call format, base URL host, or thinking-mode
setting.

```bash
harn provider dispatch-explain anthropic claude-sonnet-5 --json
harn provider dispatch-explain openrouter anthropic/claude-sonnet-4.5 --thinking --tool-format native
```

## harn provider dispatch-audit

Audit deterministic dispatch resolution across catalog routes and
tool-format/thinking variants without making provider calls. The default matrix
checks `default`, `thinking`, `native`, `text`, and `json` variants for every
catalog route and emits a JSON report suitable for CI or provider catalog
review. JSON output uses `schema_version: 3` and includes the audited catalog's
Blake3 hash plus provider, model, and route counts so downstream live-probe
results can be tied to the exact catalog universe they measured.
It also reports `unrouted_providers`: catalog providers with zero route rows,
including a typed reason, so credentialed or configured providers that cannot
enter route-level live probes are visible as catalog-quality work rather than
silently absent from the empirical matrix.

Pass `--include-tool-probe-plan` to include a post-freeze live-probe execution
manifest for the selected routes. The manifest uses stable IDs tied to the
catalog hash and structured `argv` arrays rather than shell strings, so
automation can run the probes without re-parsing command text. The plan starts
with `readiness_commands`: one `harn provider ready --model ... --json`
preflight per selected route, with its own stable `id`, `argv`, and
`output_path`. Tool-probe commands then carry an explicit `request_profile`, a
runnable `argv`, and an `output_path` under the same catalog-hash-derived
`output_dir` so live receipts can be resumed and tied back to the exact audit
plan. Readiness and tool-probe commands both carry catalogued `secret_envs` key
names when the route accepts credentials through one or more environment
variables; secret values are never included. The filenames include the command id
prefix as well as readable route/case slugs so lossy path sanitization cannot
collide distinct probe commands; override the directory with
`--tool-probe-output-dir` when staging a named run. The manifest also includes a
`matrix` summary; `provider_model_count` and `route_count` are the empirical
provider/model breadth, while bare `model_count` is only the unique model-name
count, and `readiness_command_count` is the expected preflight receipt count.
The manifest also lists `live_request_profiles` and
`excluded_request_profiles`: live `tool-probe` commands intentionally use
`catalog_default`, while
`parameter_edges` remains covered by offline `provider tool-probe-audit` request
validation because non-default request profiles are dry-run only. The
default plan also records `excluded_cases`; it covers the broad live tool-probe
cases except signed-thinking by default. Add
`--tool-probe-case signed_thinking_tool_result_followup` when auditing that
provider-specific dialect explicitly. Explicit signed-thinking cases are still
filtered per route; unsupported routes are reported under
`not_applicable_commands` with the skipped route, case, request profile, and
mode instead of emitted as invalid live commands. A route can therefore have a
readiness command and zero tool-probe commands when the selected live case is
not applicable to that route.

`--route provider:model` is fail-closed: malformed route filters and well-formed
filters that match no catalog route both produce a non-zero audit report, so a
typo cannot silently yield an empty live-probe plan.
`--provider` filters are fail-closed too: unknown provider ids and providers
with zero routing routes produce typed failures instead of a generic empty
report. `--model` and `--capability` are repeatable route filters for focused
provider/model/capability sweeps. They also fail closed when the requested model
or capability matches no selected catalog route.

```bash
harn provider dispatch-audit
harn provider dispatch-audit --provider anthropic --provider gemini
harn provider dispatch-audit --provider anthropic --model claude-sonnet-5 --capability tools
harn provider dispatch-audit --route anthropic:claude-sonnet-5 --variant native
harn provider dispatch-audit --provider anthropic --include-tool-probe-plan --tool-probe-repeat 3
harn provider dispatch-audit --provider openrouter --include-tool-probe-plan --tool-probe-output-dir .harn-runs/provider-live-probes/openrouter-smoke
```

## harn provider probe

Snapshot a provider's readiness and local loaded-model state as JSON. For
Ollama, the output also includes `/api/ps` memory/context details. When a
model is supplied, local runtime profile metadata is included for eval
pipelines.

```bash
harn provider probe ollama --model devstral-small-2
harn provider probe mlx --model mlx-qwen3.6 --base-url http://127.0.0.1:8002
```

## harn provider tool-probe

Run a harmless one-tool conformance probe. The command asks the model to call
`echo_marker({value})`, tests streaming and non-streaming modes by default, and
emits JSON with native/text/disabled fallback classification.

```bash
harn provider tool-probe ollama --model devstral-small-2
harn provider tool-probe llamacpp --model local-qwen3.6 --mode non-streaming
harn provider tool-probe dashscope --model qwen3.6-35b-a3b --mode non-streaming --repeat 5
```

Use `--response-fixture` to classify a saved provider response without making a
network request. Use `--repeat` for live reliability checks; repeated summaries
only pass when every attempted probe for that mode succeeds. `harn local switch`
can consume the JSON with `--probe-result`.

## harn provider tool-scorecard

Aggregate one or more `harn provider tool-probe --json` reports into a stable
route scorecard. Fixture input is offline-only: the command reads saved probe
reports and does not call providers. The JSON report uses `schema_version: 2`
and includes route-level `catalog_claim`, `catalog_mismatches`, and
`suggested_catalog_updates` fields so catalog drift can be reviewed from
evidence instead of incident notes. Suggested updates are advisory and are only
emitted for positive observed tool-call evidence, such as a route that reliably
produces native or text-channel tool calls. Because scorecard aggregation only
observes the text channel, not the exact text grammar, text-channel preferred
format suggestions use Harn's fenced-JSON `json` default unless the catalog
already pins a compatible `text` or `json` format.

```bash
harn provider tool-scorecard --tool-probe-report ./probe.json
harn provider tool-scorecard --tool-probe-report ./probe-a.json --tool-probe-report ./probe-b.json --json=false
harn provider tool-scorecard --tool-probe-report ./probe.json --markdown > scorecard.md
```

Use `--plan-from-catalog` to render the fixed micro-case matrix for catalogued
routes before probing. Plan output remains `schema_version: 1`.

```bash
harn provider tool-scorecard --plan-from-catalog --route anthropic:claude-sonnet-5
harn provider tool-scorecard --plan-from-catalog --route anthropic:claude-sonnet-5 --markdown > scorecard-plan.md
```

## harn local launch

Bring a local model up through Harn's provider catalog:

```bash
harn local launch devstral-small-2:24b --provider ollama --json
harn local launch local-qwen3.6 --provider llamacpp --model-source ~/models/qwen3.6/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf --ctx 8192
harn local launch mlx-qwen3.6 --provider mlx --model-source unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit
harn local launch local-gemma4-e4b --provider vllm \
  --model-source google/gemma-4-e4b-it --lora-adapter tools=org/tools-lora
```

Ollama launch warms the daemon and persists the active selection. llama.cpp
MLX, and vLLM launch the cataloged server command, record a PID/log under
`<state_root>/local/`, wait for `/v1/models`, and then let `harn local stop`
clean up the process. Provider mechanics are data-driven from
`[providers.<id>.local_runtime]` in the generated provider catalog, so local
overlays can adjust command names, ports, arg names, or model-source env vars.
When a model row includes `[models.<id>.local_memory]`, launch preflights the
requested context against current available RAM and returns a `memory_plan` in
JSON mode. Use a smaller `--ctx` or free RAM when the guard trips; pass
`--allow-memory-risk` only to override an intentionally conservative estimate.
For runtimes whose catalog row declares LoRA launch flags, repeat
`--lora-adapter NAME=PATH_OR_REPO` to expose startup adapters; when exactly one
adapter is loaded, Harn records that adapter name as the selected request
model. Provider catalog rows also own the module value shape: the default is the
portable `NAME=PATH_OR_REPO` form, while rows such as vLLM can opt into
lineage-preserving JSON values with the served base model recorded as
`base_model_name`.

## harn local profile

Explain the local runtime risk profile for a model/provider route:

```bash
harn local profile devstral-small-2 --provider ollama
harn local profile ollama-gemma4 --json
```

Statuses are `preferred`, `experimental`, `vision_only_experimental`,
`quarantined`, or `unknown`. `harn local switch` refuses experimental and
quarantined profiles unless the required probes pass or `--force` is supplied.

## harn models list

Query the same provider/model catalog Harn resolves at runtime. The human view
uses a deterministic table; `--json` returns the complete catalog rows rather
than a lossy display projection, including `pricing.cache_read_per_mtok`,
`tool_mode_parity`, and `tool_mode_parity_notes` where known.

```bash
harn models list --where tier=frontier,strengths=coding --sort pricing.input \
  --columns id,provider,pricing.input,pricing.cache_read,context_window,tool_support.parity,tool_support.parity_notes
harn models list --provider anthropic --where open_weight=false --sort context_window
harn models list --where tool_support.parity=native_unreliable --json
```

Repeat `--where KEY=VALUE` for additional all-of filters, or separate entries
with commas. Supported fields are `provider`, `tier`, `strengths` (exact tag
membership), `tool_support.parity`, and `open_weight`. Sort fields are
`pricing.input`, `pricing.output`, `pricing.cache_read`, and `context_window`;
numeric sorts are ascending with missing metadata last. `--columns` selects a
strict allowlist for the human table and cannot be combined with `--json`,
which intentionally returns complete authoritative rows.

## harn models test

Round-trip a small prompt through one resolved model and report model id,
provider, latency, first streamed delta timing, token usage, and estimated
cost.

```bash
harn models test gpt-4o-mini --prompt "Reply with pong."
harn models test qwen3:30b --provider ollama --json
```

`--provider` bypasses provider inference for the model selector. The command
uses the configured provider client path, so it also respects provider
credentials, base URL overrides, and `HARN_LLM_CALLS_DISABLED`.

## harn models info

Print resolved model metadata as JSON. For Ollama models, `--verify` probes
`/api/tags` and checks the selected tag. `--warm` implies `--verify` and sends
an empty `/api/generate` request to preload the matched tag.

```bash
harn models info llama3.2:latest
harn models info --verify llama3.2
harn models info --warm --keep-alive 30m llama3.2
```

Ollama readiness failures use stable `readiness.status` values, including
`daemon_down`, `bad_status`, `invalid_response`, `model_missing`, and
`warmup_failed`. `--verify` and `--warm` exit non-zero when readiness fails.

## harn models batch plan

Plan provider Batch API use for latency-tolerant model workloads:

```bash
harn models batch plan --workload eval
harn models batch plan --provider openai --min-discount-percent 50 --json
harn models batch plan --model gpt-4o-mini --max-turnaround-hours 24
```

The planner reads Harn's provider catalog and lists routes whose provider
capabilities advertise async batch support. Harn owns the reusable batch
manifest, prepare, submit, and receipt workflow so Burin or other hosts can
route lower-priority eval, judge, corpus refresh, and distillation workloads to
discounted batch lanes without leaking provider-specific policy into product
code. Subscription-plan CLI auth is reported as separate from API batch
credentials; providers that expose Batch APIs still require provider API
billing.

Human output marks each route as `live submit` or `dry-run only`. JSON output
includes `batch.harn_live_adapter.{submit,status,cancel,download}` plus Harn's typed
batch lifecycle facts: wire/input mode, published discounts and turnaround,
request/file ceilings, result ordering, per-request failure semantics,
cancellation support, retention, non-secret storage notes, and non-secret
operational notes such as retry and result-rejoin constraints. Provider Batch
API capability stays separate from Harn's current live adapter coverage. Routes
with `batch_api: true` but no Harn live adapter can still be planned,
manifested, and prepared for offline adapter work; `submit`, `status`,
`cancel`, and `download` fail before network calls until that provider adapter
exists. Treat
async batch receipts as offline eval/corpus artifacts, not as live interactive
agent-loop pass@1 evidence for the meter stick.

## harn models batch manifest

Build a durable, provider-neutral manifest for a JSONL request ledger:

```bash
harn models batch manifest --provider openai --model gpt-4o-mini \
  --requests ./requests.jsonl --out ./batch-manifest.json
harn models batch manifest --provider openai --model gpt-4o-mini \
  --requests ./requests.jsonl --out ./batch-manifest.json --tool-format json --json
```

Each input row must be a JSON object. Rows can declare `provider`, `model`,
`tool_format`, `endpoint`, `custom_id`, and `metadata`; omitted provider/model
fields fall back to the CLI flags. Harn resolves every row through the same
catalog metadata as `harn models batch plan`, groups requests by provider,
model, provider batch wire format, endpoint, and tool-call convention, then
writes a manifest with stable request ids and per-row hashes.

`manifest` still does not submit work to providers. It is the durable planning
artifact that lower-priority eval, judge, corpus-refresh, and distillation jobs
can hand to provider-specific upload/poll/download adapters without leaking
provider batch details into Burin or other host products.

## harn models batch prepare

Prepare provider-native request artifacts from a model batch manifest:

```bash
harn models batch prepare --manifest ./batch-manifest.json --out-dir ./.harn/batches/eval-001
harn models batch prepare --manifest ./batch-manifest.json --out-dir ./.harn/batches/eval-001 --json
```

`prepare` reads the provider-neutral manifest and writes one request artifact
per batch group plus a deterministic `receipt.json`. The request artifacts use
the provider's batch envelope (`openai`/`mistral` JSONL, Fireworks
`{custom_id, body}` JSONL, Gemini `{key, request}` JSONL, or Anthropic Message
Batches JSON) while the receipt records the stable manifest hash, request-file
hashes, provider operation, upload/create shape, and result-rejoin ids. It does
not read credentials or call provider APIs;
`harn models batch submit` consumes this receipt as its durable input.
Prepare receipts and job entries also include a provider-neutral `lifecycle`
object with `phase`, normalized `state`, terminal/result/cancel/retry booleans,
and per-state counts so consumers do not need to scrape status strings.

## harn models batch submit

Validate and optionally submit prepared provider-native batch jobs:

```bash
harn models batch submit --receipt ./.harn/batches/eval-001/receipt.json \
  --out ./.harn/batches/eval-001/submission.json --dry-run
harn models batch submit --receipt ./.harn/batches/eval-001/receipt.json \
  --out ./.harn/batches/eval-001/submission.json --json
```

`--dry-run` verifies the prepare receipt, re-hashes every request file, renders
the provider operation with credential names redacted, and writes a durable
`harn.model_batch_submission_receipt` without network calls. Live submit
currently supports OpenAI-compatible batch jobs for OpenAI, Groq, Together, and
Parasail; Mistral file-backed batch jobs; Fireworks dataset-backed batch jobs;
Gemini File API JSONL batches; Anthropic Message Batches; and xAI batches.
Provider API keys must be present
in the provider's normal environment variable (`OPENAI_API_KEY`,
`GROQ_API_KEY`, `TOGETHER_AI_API_KEY` or `TOGETHER_API_KEY`,
`PARASAIL_API_KEY`, `MISTRAL_API_KEY`, `FIREWORKS_API_KEY`, `GEMINI_API_KEY`
or `GOOGLE_API_KEY`, `ANTHROPIC_API_KEY`, or `XAI_API_KEY`); Fireworks live
submit/status/download also needs `HARN_BATCH_FIREWORKS_ACCOUNT_ID` or
`FIREWORKS_ACCOUNT_ID`.
Subscription-plan
auth remains out of scope for provider Batch APIs. The submission receipt
records provider job ids, status, request file hashes, and result handles for
later poll/download/rejoin work. Submission receipts carry the same normalized
`lifecycle` object as prepare receipts; dry-run jobs remain `ready` while the
receipt lifecycle state is `dry_run`.

## harn models batch status

Poll provider state for submitted model batch jobs:

```bash
harn models batch status --submission ./.harn/batches/eval-001/submission.json \
  --out ./.harn/batches/eval-001/status.json --dry-run
harn models batch status --submission ./.harn/batches/eval-001/submission.json \
  --out ./.harn/batches/eval-001/status.json --json
```

`--dry-run` validates the submission receipt and summarizes cached job state
without network calls. Live status currently polls OpenAI/Groq/Together-
compatible `batches/{id}`, Fireworks `batchInferenceJobs/{id}`, Gemini
`batches/{id}`, Anthropic Message Batches, Mistral batch jobs, and xAI batches
from the Harn provider adapter boundary, then writes a
`harn.model_batch_status_receipt` with stable job ids, normalized lifecycle
state, provider status, and result file pointers. The embedded `lifecycle`
object is the stable state-machine contract for eval/corpus runners; top-level
counts remain a human-friendly summary.

## harn models batch cancel

Cancel submitted model batch jobs and write a durable cancellation receipt:

```bash
harn models batch cancel --receipt ./.harn/batches/eval-001/submission.json \
  --out ./.harn/batches/eval-001/cancel.json --dry-run
harn models batch cancel --receipt ./.harn/batches/eval-001/status.json \
  --out ./.harn/batches/eval-001/cancel.json --json
```

`cancel` accepts either a submission receipt or a latest status receipt. In
`--dry-run` mode it validates provider job ids and records redacted cancellation
operations without network calls. Live cancellation is implemented only where
the provider catalog says cancellation is supported and Harn has a known
adapter: OpenAI batch jobs, Anthropic Message Batches, and Gemini Batch API
operations. Jobs that are already terminal, missing provider ids, or backed by
providers with unknown cancellation semantics are skipped with structured
`skip_reason` values rather than treated as hard failures. The command writes a
`harn.model_batch_cancel_receipt` with per-job cancel operations, provider
responses, skipped-job reasons, normalized `canceling`/`canceled` lifecycle
states, and aggregate lifecycle counts.

## harn models batch download

Download result files for completed model batch jobs:

```bash
harn models batch download --status ./.harn/batches/eval-001/status.json \
  --out-dir ./.harn/batches/eval-001/results --dry-run
harn models batch download --status ./.harn/batches/eval-001/status.json \
  --out-dir ./.harn/batches/eval-001/results --json
```

`--dry-run` validates the status receipt, requires completed jobs, and records
the redacted provider download operations without network calls. Live download
currently retrieves OpenAI/Groq/Together-compatible, Gemini, and Mistral file
content plus Fireworks output dataset signed URLs, Anthropic Message Batch
`results_url`, and xAI result pages, writes provider
JSONL files under `--out-dir`, and emits a `harn.model_batch_results_receipt`
containing artifact paths, handles, hashes, source receipt metadata, and
normalized download lifecycle counts. Use
`--max-bytes` to cap each provider file response when working with very large
batches.

## harn models lora export

Export a tool-calling corpus into a trainer-ready LoRA dataset:

```bash
harn models lora export --base local-gemma4-e4b --provider vllm --tool-format auto \
  --corpus ./lora-corpus --out ./train.jsonl --manifest ./train.manifest.json
harn models lora export --base local-gemma4-e4b --provider vllm --tool-format native \
  --corpus ./lora-corpus --modules-to-save embed_tokens,lm_head --check --json
```

The export resolves the same provider capability matrix as model calls and
emits either native `messages`/`tools` rows or Harn text-tool rows. Reports,
row metadata, and manifests include a stable LoRA contract id derived from the
base model, provider, effective Harn tool format, dataset format, chat
template, and tool-catalog policy. Use that id to keep training, adapter
inspection, eval, and serving on the same wire contract even when adapter paths
or request-model names change.
The manifest's `contract.training_contract` block also records the assistant
mask policy, packing policy, parser owner, split policy, and PEFT
`modules_to_save` / embedding-head save policy so trainers can verify the SFT
setup without scraping human-readable notes. The default policy is adapter-only;
declare `--modules-to-save embed_tokens,lm_head` only when tokenizer resize or
output-head training requires it, and keep the weight-tying evidence in target
metadata before merging adapters.
`--tool-catalog-policy full_schema` is the default production contract: prompts
carry the full tool schemas used to validate the dataset. `compressed_names`
and `fixed_catalog_internalized` are explicit fixed-catalog experiment modes;
they require `--tool-catalog-id` or `--tool-catalog-hash` and are hashed into
the LoRA contract so names-only or no-catalog adapters cannot reuse full-schema
manifests.
`--check` validates conversion without writing JSONL rows.

## harn models lora manifest

Write a canonical LoRA training-run manifest without invoking a trainer:

```bash
harn models lora manifest --base local-gemma4-e4b --provider vllm \
  --tool-format json --dataset ./train.jsonl --export-manifest ./train.manifest.json \
  --adapter-name burin-tools --adapter-path ./adapter --out ./adapter.manifest.json \
  --modules-to-save embed_tokens,lm_head
harn models lora manifest --base local-gemma4-e4b --provider vllm \
  --trainer unsloth_sft --method qlora --rank 24 --training-run-id run-123 --json
```

The command records the resolved base route, effective tool-call format, Harn
LoRA contract id, trainer hyperparameters, corpus/export inputs, adapter
artifact paths and hashes when local files exist, serving hints,
`serving.serving_requirements`, and the promotion/eval recipe. It is
intentionally trainer-agnostic: Python, MLX,
Unsloth, TRL, cloud, or future Harn-native trainers can all call this surface
after a run and produce the same manifest shape. The resulting JSON is accepted
by `harn models lora inspect --manifest`, so adapter promotion can compare the
training contract with the served route before any eval or launch.

## harn models lora train

Render or execute a named LoRA trainer backend and write a training receipt:

```bash
harn models lora train --base local-gemma4-e4b --provider vllm \
  --tool-format json --dataset ./train.jsonl --export-manifest ./train.manifest.json \
  --output-dir ./adapter --receipt-out ./adapter/train.receipt.json \
  --adapter-name burin-tools --request-model burin-tools --trainer unsloth_sft \
  --modules-to-save embed_tokens,lm_head --json
harn models lora train --base local-gemma4-e4b --provider vllm \
  --dataset ./train.jsonl --output-dir ./adapter --execute -- \
  uv run python train.py config/e4b.yaml
```

Without `--execute`, the command is a deterministic dry-run: it hashes local
input files when present, records trainer/backend name and version, resolves the
same Harn LoRA contract as plan/export/manifest, and emits the post-training
`harn models lora manifest` plus `harn models lora inspect` commands. The
receipt also includes a structural dataset audit with parse-error,
invalid-tool-block, schema-repair, no-tool, unavailable-tool, parallel-call,
multi-turn, and tool-result counters. With `--execute`, Harn launches only the
caller-supplied backend argv and records the exit status in the same receipt
shape. Python, Unsloth, TRL, MLX, Modal, or cloud trainers remain backend shims;
Harn owns config normalization, route metadata, serving requirements, manifest
shaping, and promotion receipts. Dry-runs with no backend argv set
`backend.argv_required` instead of inventing a trainer command.
`--modules-to-save` is part of the hashed LoRA contract and is forwarded to the
post-training manifest command, so PEFT embedding/head saves cannot silently
drift between export, train, manifest, inspect, and promotion.
`--tool-catalog-policy`, `--tool-catalog-id`, and `--tool-catalog-hash` are also
forwarded into backend recipes and post-training manifests when fixed-catalog
compression experiments intentionally train for names-only or no-catalog
inference.

## harn models lora preflight

Check a tool-calling corpus before spending GPU time on LoRA training:

```bash
harn models lora preflight --base local-gemma4-e4b --provider vllm \
  --corpus ./lora-corpus --config ./config/e4b.yaml --done-marker '##DONE##' --check
harn models lora preflight --base google/gemma-4-e4b-it \
  --corpus ./lora-corpus/burin-tool-calling-corpus.jsonl \
  --source-tool-format json --min-records 190 --json
```

The preflight is CPU-only. It resolves the target model route, reads
`max_seq_length` and `min_fit_ratio` from a lightweight config file when
provided, then checks record count, approximate sequence-budget fit, hard token
outliers, source tool-call body format, malformed tool-call blocks, undeclared
tool names, and an optional caller-supplied done marker. Use `--check` to make
readiness failures exit non-zero. The command intentionally owns reusable LoRA
corpus readiness in Harn; product-specific markers or thresholds should be
passed as flags rather than hard-coded into hosts.

## harn models lora inspect

Inspect a PEFT LoRA adapter directory or repo id against a Harn model route:

```bash
harn models lora inspect --base local-gemma4-e4b --provider vllm ./tools-lora
harn models lora inspect --base local-gemma4-e4b --provider vllm --name tools org/tools-lora --json
```

The report checks local `adapter_config.json` metadata when present, compares
`base_model_name_or_path` with the resolved Harn model id, shows the route's
tool-calling capability metadata, and prints the matching `harn local launch`
command. Inspect JSON also includes `serving.serving_requirements`, so promotion
automation can verify parser ownership, runtime flags, and template metadata for
the served adapter route. When local metadata declares a LoRA rank and the
cataloged runtime has a max-rank flag, the launch command includes
`--max-lora-rank` so runtime defaults cannot silently under-provision the
adapter. Remote adapter ids are treated as runtime-resolved and warned rather
than rejected.

## harn models lora plan

Plan a portable LoRA or QLoRA fine-tune for a Harn model route:

```bash
harn models lora plan --base local-gemma4-e4b --provider vllm --tool-format auto --corpus ./lora-corpus
harn models lora plan --base local-gemma4-e4b --provider vllm --trainer unsloth_sft \
  --teacher dashscope/qwen3-coder-next --corpus ./lora-corpus \
  --modules-to-save embed_tokens,lm_head
harn models lora plan --base google/gemma-4-E4B-it --provider google --tool-format native --json
```

The plan resolves the same provider capability matrix used by agent calls,
records the effective tool-call format, and prints a training/eval/launch
recipe. The report includes an explicit template block so native Gemma 4 or
FunctionGemma training stays on the model tokenizer's function-calling template,
while Harn text/json adapters target the Harn `<tool_call>` parser convention.
Use this before exporting a corpus so train, eval, and runtime serving all agree
on the same tool-call contract. Add `--teacher <model>` to plan a synthetic
distillation or corpus-refresh lane without starting any model calls. With the
default `--corpus-strategy auto`, Harn chooses `refresh` when a corpus is
present, `distill` when only a teacher is present, and `audit-only` otherwise.
The report lists provenance manifest fields, hard-negative slices, and holdout
gates so generated data cannot silently contaminate evaluation fixtures. It
also lists the trainer contract Harn expects for tool-calling SFT, including
assistant-only loss masks, `messages`/`tools` columns, packing boundaries, and
method-specific target modules. Omitting `--trainer` uses the neutral
`external_sft_trainer` contract. Use `--trainer trl_sft_trainer`,
`--trainer unsloth_sft`, or `--trainer external_sft_trainer` to pin a
backend-specific contract explicitly; Harn still owns export, manifests, eval,
and serving promotion, while trainer-specific runtimes only fit the adapter
weights. QLoRA plans use PEFT's `all-linear` target module shorthand; full LoRA
plans keep explicit attention projection modules in `training.target_modules`.
Plans default to no PEFT `modules_to_save`, which keeps tool-calling adapters
small and avoids accidental embedding/LM-head divergence. When embedding or
head modules must be saved, `training.contract.peft_save_policy` records the
declared modules, embedding save policy, and whether a weight-tying check is
required before adapter merge.
The `corpus_refresh.model_aware_selection` block records base-model failure
buckets, parser/schema difficulty signals, sampling policy, refinement loops,
and stop conditions so corpus refresh jobs prioritize failures the target model
almost solves instead of adding near-duplicate syntax examples.
The JSON report exposes the same machine-readable contract under
`training.contract` for automation that prepares TRL/PEFT trainer configs.
The `serving.serving_requirements` array records machine-readable serving
requirements such as parser ownership, vLLM tool-parser flags, chat-template
ids, required manifest metadata, and promotion gates. Plan, manifest, and inspect
automation should satisfy those requirements before serving or promoting an
adapter instead of scraping runtime notes.
The `evaluation.evidence_contract` block adds the promotion receipt contract:
stable promotion id, paired base/adapter routes, required preflight, export,
manifest, inspect, tool-probe, and eval receipts, optional Batch API receipts,
and a `harn models batch manifest` command for latency-tolerant tool-call eval
rows.
The launch block includes the matching `harn models lora export` command so the
trainer dataset, provenance manifest, eval route, and serving route share the
same base model, provider, tool format, and chat-template contract.
Use `--tool-catalog-policy compressed_names` or
`--tool-catalog-policy fixed_catalog_internalized` only for fixed-catalog
experiments, and pair it with `--tool-catalog-id` or `--tool-catalog-hash`.
The policy controls whether inference still includes full tool schemas,
compressed tool names only, or no runtime catalog; it is recorded in
`training.contract.tool_catalog`, included in the LoRA contract id, and
forwarded to export, train, and manifest commands.
Use `--rank`, `--alpha`, and `--dropout` to pin adapter hyperparameters in the
same contract; when the runtime exposes a LoRA rank flag, the local launch hint
uses the planned rank as `--max-lora-rank` so serving buffers match training.
The `serving.lora_module_value_format` field records whether the runtime uses
the portable `NAME=PATH_OR_REPO` adapter value or a cataloged richer value such
as vLLM's base-model-lineage JSON.
For native Gemma 4 routes on vLLM, the plan also warns that the `gemma4`
tool-call parser and chat template are part of the serving contract. Validate
or promote adapters with serialized native-tool traffic unless the pinned vLLM
parser version is already proven concurrency-safe for the route.

## harn models recommend

Recommend a starter model from the local hardware snapshot and configured cloud
credentials. The command prefers an Ollama model when no cloud provider key is
set, and emits the selected model plus the one-line rationale.

```bash
harn models recommend
harn models recommend --json
```

The JSON output includes the hardware snapshot used for the lookup: available
RAM, GPU/MPS acceleration, and free disk space.

## harn connect

Authorize connector providers and store local connector secrets in the
workspace keyring namespace.

```bash
harn connect github \
  --app-slug my-harn-app \
  --app-id 12345 \
  --private-key-file app.pem
harn connect slack \
  --client-id "$SLACK_CLIENT_ID" \
  --client-secret "$SLACK_CLIENT_SECRET" \
  --scope "app_mentions:read chat:write"
harn connect linear \
  --client-id "$LINEAR_CLIENT_ID" \
  --client-secret "$LINEAR_CLIENT_SECRET"
harn connect notion \
  --client-id "$NOTION_CLIENT_ID" \
  --client-secret "$NOTION_CLIENT_SECRET"
harn connect generic acme https://mcp.example.com/mcp
harn connect --generic acme https://mcp.example.com/mcp
harn connect acme
harn connect --list
harn connect --refresh notion
harn connect --revoke slack
```

OAuth provider commands use a plaintext HTTP loopback callback bound to
`127.0.0.1` or `localhost`. The default redirect URI uses port `0`, so Harn
selects a random free localhost port and sends that exact URI in the
authorization request; custom redirect URIs must also include an explicit port.
PKCE S256 is always enabled. Generic OAuth discovers protected-resource and
authorization-server metadata when explicit endpoints are not supplied, attempts
dynamic client registration when available, and sends the `resource` parameter
to both the authorization and token endpoints. `harn mcp login` derives that
resource from the canonical MCP server URI: lowercase scheme and host, no
default port, no query string, no fragment, and no trailing slash.

`harn connect <provider>` is available for providers registered in the nearest
`harn.toml` `[[providers]]` table with `oauth = { ... }` metadata. CLI flags
such as `--client-id`, `--scope`, `--auth-url`, and `--token-url` override the
manifest metadata for that run.

Stored OAuth tokens are written under connector-friendly secret ids:

- `<provider>/access-token`
- `<provider>/refresh-token` when the provider returns one
- `<provider>/oauth-token` for the full local refresh metadata

`harn connect --list` reads a small keyring index and shows token expiration
and last-used metadata when known. `--refresh <provider>` forces a refresh-token
grant. `--revoke <provider>` removes the local OAuth token, access token,
refresh token, and index entry.

Provider-specific OAuth flags:

| Flag | Description |
|---|---|
| `--client-id <id>` | Pre-registered OAuth client id |
| `--client-secret <secret>` | OAuth client secret |
| `--scope <scopes>` | Requested scope string |
| `--resource <resource>` | Override the OAuth resource indicator |
| `--auth-url <url>` | Override the authorization endpoint |
| `--token-url <url>` | Override the token endpoint |
| `--token-auth-method <method>` | `none`, `client_secret_post`, or `client_secret_basic` |
| `--redirect-uri <uri>` | Override the loopback callback URI |
| `--no-open` | Print the authorization URL instead of opening a browser |

The GitHub command captures GitHub App installation metadata. If `--app-id` and
`--private-key-file` are supplied, it stores the private key as
`github/app-<app-id>/private-key`. If `--webhook-secret` or
`--webhook-secret-file` is supplied, it stores `github/webhook-secret`.

## harn connect linear

Register a Linear webhook through the GraphQL `webhookCreate` mutation using
the Linear triggers declared in the nearest `harn.toml`.

The command derives `resourceTypes` from `[[triggers]]` entries with
`provider = "linear"` and requires either `--team-id` or
`--all-public-teams`.

```bash
harn connect linear \
  --url https://example.com/hooks/linear \
  --team-id 72b2a2dc-6f4f-4423-9d34-24b5bd10634a \
  --access-token-secret linear/access-token

harn connect linear \
  --url https://example.com/hooks/linear \
  --all-public-teams \
  --api-key-secret linear/api-key \
  --json
```

Auth options:

- `--access-token` or `--access-token-secret`
- `--api-key` or `--api-key-secret`

Use `--config <path>` to point at an explicit manifest instead of discovering
the nearest one from the current working directory.

## harn watch

Watch a file for changes and re-run it automatically.

```bash
harn watch main.harn
harn watch --deny shell main.harn
```

## harn dev

Watch a whole Harn project and re-type-check only the modules invalidated by
the latest edit. Invalidation is gated by per-module **interface fingerprints**
— a stable BLAKE3 hash of every module's public surface (types, function
signatures, `pub import` re-exports). An edit that only changes a function
body leaves the fingerprint alone, so dependents are not re-checked. An edit
to a public signature, struct shape, or re-export bumps the fingerprint and
every transitive importer is invalidated and re-checked.

```bash
harn dev --watch                 # watch the current directory
harn dev --watch ./src           # watch a specific tree
harn dev --watch --json          # NDJSON event stream for agents/editors
harn dev --watch --with-tests    # also re-run `test_*` pipelines per module
```

| Flag | Description |
|---|---|
| `--watch` | Required. Starts the incremental file-watch loop. |
| `--json` | Emit a newline-delimited JSON event stream on stdout. Every line is a `JsonEnvelope`-wrapped event (`ready`, `fingerprint_changed`, `rerun`, `diagnostics`, `tests`). See `harn --json-schemas --command dev`. |
| `--with-tests` | After each re-check, run every `test_*` or `@test`-attributed pipeline in the invalidated modules. |
| `--test-timeout-ms <ms>` | Per-module test timeout when `--with-tests` is set. Defaults to `10000`. |
| `<root>` | Optional project root. Defaults to the current working directory. |

`harn dev --watch --json` event shapes (one per line):

```jsonc
// First event — the initial fingerprint snapshot.
{"event":"ready","root":"/abs/path","modules":3,"fingerprints":{"src/lib.harn":"<hex>", ...}}
// Public-surface diff for a single module.
{"event":"fingerprint_changed","module":"src/lib.harn","old":"<hex>","new":"<hex>"}
// The set of modules about to be re-checked. Driven by either a body-only
// edit (just the changed file) or a fingerprint flip (the changed file plus
// every transitive importer).
{"event":"rerun","modules":["src/lib.harn","src/user.harn"]}
// One per module re-checked.
{"event":"diagnostics","module":"src/user.harn","count":1,"diagnostics":[{"severity":"warning","code":"HARN-ORC-001","message":"Function 'add' expects 3 arguments, got 2","line":2,"column":17}]}
// Only emitted with `--with-tests`.
{"event":"tests","module":"src/user.harn","passed":3,"failed":0,"failures":[]}
```

The bytecode cache (`harn precompile`) keys on transitive source content, so a
fingerprint-stable edit still busts the entry-chunk cache for the changed
file. `harn dev`'s contribution is the dependent-pruning half: dependents whose
imports' fingerprints didn't move are never re-precompiled.

## harn portal

Launch the local Harn observability portal for persisted runs.

```bash
harn portal
harn portal --dir runs/archive
harn portal --manifest examples/personas/harn.toml --persona-state-dir .harn/personas
harn portal --host 0.0.0.0 --port 4900
harn portal --open false
```

See [Harn Portal](./portal.md) for the full guide.

## harn runs

Inspect persisted workflow runs through stable view schemas.

```bash
harn runs view --json .harn-runs/<run>.json
harn runs view --json --session .harn-runs/
```

## harn replay

Replay a persisted workflow run record from saved output, a replay-oracle
fixture, or an agent session stored in the SQLite EventLog.

```bash
harn replay .harn-runs/<run>.json
harn replay --fixture conformance/replay-oracle/fixtures/simple_trigger_local_handler.valid.json --runs 3 --json
harn replay --session-id <id> --events-db .harn/events.sqlite --runs 3 --json
harn replay --session-id <id> --events-db .harn/events.sqlite --at 7 --counterfactual ./what-if.harn --json
```

`--session-id` reads the `observability.agent_events.<sanitized-id>` topic
from `--events-db` in append order, reconstructs a replayable run record,
and feeds it through the same replay summary path as run-record input.
When `--runs N` is greater than one, JSON output includes per-run reports
plus an allowlist-normalized determinism summary.
`--at <event-id>` rehydrates the session prefix through the inclusive
event id. Repeat `--counterfactual <plan.harn>` to chain returned edit-op
lists into one cumulative dry-run divergence report.

## harn eval

Evaluate a persisted workflow run record as a regression fixture, or
diff a `.harn.prompt` template across a fleet of models with `harn eval
prompt`.

```bash
harn eval .harn-runs/<run>.json
harn eval .harn-runs/<run>.json --compare baseline.json
harn eval .harn-runs/
harn eval evals/regression.json
harn eval harn.eval.toml
harn eval evals/clarifying-question.json
harn eval --llm-mock fixtures.jsonl --structural-experiment doubled_prompt pipeline.harn
harn eval context examples/evals/context-engineering-smoke.json \
    --output target/context-eval --json
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro,qwen3.5,ollama:qwen3.5 \
    --bindings examples/coding-agent-system.bindings.json
```

The legacy `harn eval <path>` entrypoint accepts five inputs:

- a single run record JSON file
- a directory of run record JSON files
- an eval suite manifest JSON file with grouped cases and optional baseline comparisons
- an eval-pack v1 TOML/JSON manifest such as `harn.eval.toml`
- a standalone persona eval ladder manifest

Run eval packs declared by a package manifest with:

```bash
harn test package --evals
```

Package eval discovery uses `[package].evals = ["evals/webhooks.toml"]` when
present, otherwise it falls back to `harn.eval.toml` in the package root. After
`harn install`, discovery also includes eval packs declared by dependency
packages in the leased current generation.

Clarifying-question evals use an explicit fixture with
`"eval_kind": "clarifying_question"`. The fixture checks persisted
`ask_user(...)` prompts captured in the run record and can enforce a
single minimal question via `required_terms`, `forbidden_terms`, and
question-count bounds.

When `path` is a `.harn` pipeline file, `--structural-experiment <spec>` runs
the pipeline twice in isolated temp run directories: once as the baseline and
once with `HARN_STRUCTURAL_EXPERIMENT=<spec>`. The CLI then evaluates both run
sets against their embedded replay fixtures and prints a paired A/B summary.
Use `--llm-mock <fixture.jsonl>` to keep the two runs deterministic.

### harn eval context

Run deterministic context-engineering evals over task fixtures and named
context modes:

```bash
harn eval context examples/evals/context-engineering-smoke.json \
    --output target/context-eval --json
```

The manifest uses `_type = "harn.context_eval.manifest.v1"` and can be JSON
or TOML. It declares tasks, artifacts, transcript snippets, tool disclosures,
expected terms/artifacts/tools, and modes such as HUD packs, projections,
compaction, or limited tool disclosure. The local runner does not call an LLM;
it scores whether each mode exposes enough deterministic context for the task
and writes:

- `summary.json`: a `harn.context_eval.report.v1` aggregate report
- `per_run.jsonl`: one machine-readable record per task/mode run
- `summary.md`: a compact human-readable table

The report schema is checked into
`spec/schemas/context-eval-report.v1.schema.json` so hosted evaluators,
dashboards, and downstream products can ingest the same local artifacts.

### harn eval prompt

Render a single `.harn.prompt` template against every model in a fleet,
optionally execute it against each, and surface the resulting wire
envelopes side-by-side. This is the cross-model diff renderer that
exists so capability-adapted variants do not rot silently as the model
mix evolves (see [Capability-aware prompts](./prompt-templating.md#the-llm-scope)).

```bash
# Render-only: produces side-by-side rendered envelopes per model.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro,qwen3.5,ollama:qwen3.5

# Run mode: also calls each model and collects the response.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5 \
    --mode run

# Judge mode: scores cross-model equivalence with an LLM judge.
harn eval prompt examples/coding-agent-system.harn.prompt \
    --fleet claude-opus-4-7,gpt-5,gemini-2.5-pro \
    --mode judge \
    --judge-model claude-opus-4-7

# Use a named fleet from `[eval.fleets.<name>]` in harn.toml.
harn eval prompt prompts/agent.harn.prompt --fleet-name frontier --output html -o report.html

# Score repo-context artifact selection and rendered section shape.
harn eval prompt examples/evals/repo-context-quality.harn.prompt \
    --fleet claude-3-5-sonnet,gpt-4o,ollama:qwen3.5 \
    --context-fixture examples/evals/context-retrieval-fixture.json \
    --output json -o context-quality.json
```

| Flag | Description |
|---|---|
| `--fleet <list>` | Comma-separated model selectors (alias or `provider:model`). Repeatable. |
| `--fleet-name <name>` | Named fleet from `[eval.fleets.<name>]` in `harn.toml`. |
| `--bindings <path>` | JSON file injected into the template scope. Top-level value must be an object. |
| `--context-fixture <path>` | JSON fixture(s) for context-quality gates. Each case supplies candidate artifacts, assembler options, and expectations for selected/rejected/stale artifact ids, token budget, and logical-section envelopes. Repeatable. |
| `--mode <render\|run\|judge>` | Default `render`. |
| `--output <terminal\|json\|html>` | Default `terminal`. |
| `-o, --out-file <path>` | Write `--output` payload to a file instead of stdout. |
| `--max-concurrent <N>` | Cap concurrent provider invocations in `run` / `judge` modes (default 4). |
| `--judge-template <path>` | Override the built-in judge `.harn.prompt`. |
| `--judge-model <selector>` | Judge model (default `claude-opus-4-7`). |
| `--max-tokens <N>` | Cap on completion tokens for `run` / `judge` calls (default 1024). |
| `--fail-on-unauthorized` | Treat unauthenticated providers as errors instead of skipping. |

Fleets can be declared in `harn.toml` and reused:

```toml
[eval.fleets.frontier]
models = ["claude-opus-4-7", "gpt-5", "gemini-2.5-pro"]

[eval.fleets.local]
models = ["ollama:qwen3.5"]
```

Render mode never calls a model — it only pushes the LLM render context
per fleet member and resolves the template so authors can confirm that
`{{ if llm.capabilities.* }}` branches produce the intended wire
envelope on each profile. Run mode synthesizes a thin Harn driver and
routes through the existing `llm_call` infrastructure, so credentials,
provider catalog, and `HARN_LLM_PROVIDER=mock` work exactly as in
`harn run`. Judge mode is run mode plus a final LLM-as-judge call that
asks for a one-line JSON verdict on cross-model equivalence.

`--context-fixture` adds deterministic repo-context quality gates to
the same fleet render. The CLI first packs each case's candidate
artifacts with `assemble_context`, injects `context`,
`assembled_context`, `candidate_artifacts`, `selected_artifact_ids`,
and `dropped_artifact_ids` into the template bindings, then renders the
case across every fleet member. JSON/HTML/terminal output includes a
`context_eval` report with selected artifact ids, stale/noisy rejection,
budget adherence, per-model logical-section envelopes, and an overall
score suitable for CI or dashboard ingestion.

## harn merge-captain

Run, audit, or exercise Merge Captain fixtures.

```bash
harn merge-captain run --backend replay examples/personas/merge_captain/transcripts/green_pr.jsonl --once
harn merge-captain audit examples/personas/merge_captain/transcripts/green_pr.jsonl --golden examples/personas/merge_captain/goldens/green_pr.json
harn merge-captain ladder personas/merge_captain/harn.eval.toml --format json --report-out .harn-runs/merge-captain-ladder/report.json
harn merge-captain iterate examples/personas/merge_captain/iterations/smoke.toml --format json
harn merge-captain iterate --diff .harn-runs/merge-captain-iterations/baseline .harn-runs/merge-captain-iterations/candidate
```

`harn merge-captain ladder` runs every configured model route and timeout
tier against the same backend fixture. It writes per-tier JSONL transcripts,
receipts, and summaries, and emits an aggregate report with the first correct
tier plus degraded or looping tiers.

`harn merge-captain iterate` runs the next layer up: scenario × variant sweeps
for agent-led prompt/package iteration. Each variant can carry model route,
timeout tier, Harn package revision, and prompt-asset revision metadata. The
iteration directory is self-contained: it stores copied replay fixtures or
materialized mock playgrounds, per-run JSONL transcripts, receipts, summaries,
`summary.json`, and `summary.md`. The aggregate table ranks variants by
transcript-drift score, then cost. `--diff A B` compares two iteration
directories or summary JSON files and marks scenario/variant cells as improved,
regressed, unchanged, or missing.

## harn orchestrator

Long-running manifest-driven orchestrator for trigger ingestion and
connector activation. See [Orchestrator](./orchestrator.md) for full
detail.

```bash
# Start the orchestrator against a manifest. Binds HTTP(S) for
# webhook/a2a-push triggers, activates connectors, writes a state
# snapshot, drains cleanly on SIGTERM/SIGINT, reloads on SIGHUP.
harn orchestrator serve \
  --config harn.toml \
  --state-dir .harn/orchestrator \
  --bind 0.0.0.0:8080 \
  --pump-max-outstanding 64 \
  --log-format json \
  --role single-tenant

# Scrape Prometheus metrics from the live listener.
curl http://127.0.0.1:8080/metrics

# Inspect the running orchestrator state, trigger flow-control state, and recent dispatches.
harn orchestrator inspect --state-dir .harn/orchestrator

# Inject a synthetic TriggerEvent to exercise a specific binding.
harn orchestrator fire <trigger-id>

# Replay a historical event through the dispatcher.
harn orchestrator replay <event-id>

# Run replay determinism oracle fixtures.
harn orchestrator replay-oracle

# Inspect the dead-letter queue.
harn orchestrator dlq list
harn orchestrator dlq --replay <event-id>

# Inspect the pending-queue head.
harn orchestrator queue

# List worker queues + stranded dispatcher envelopes explicitly.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator ls

# Drain one worker queue with a local consumer manifest.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator drain <queue>

# Drop ready jobs from a worker queue.
harn orchestrator queue --config harn.toml --state-dir ./.harn/orchestrator purge <queue> --confirm

# Generate and run a cloud deploy bundle.
harn orchestrator deploy --provider fly --manifest ./harn.toml --build --dry-run
```

`harn orchestrator inspect/fire/replay/dlq/queue` are offline
operations — they read the state snapshot + event log directly. To
operate against a live `harn orchestrator serve`, use the same state
directory. Environment variables `HARN_ORCHESTRATOR_MANIFEST`,
`HARN_ORCHESTRATOR_LISTEN`, `HARN_ORCHESTRATOR_STATE_DIR`,
`HARN_ORCHESTRATOR_API_KEYS`, and `HARN_ORCHESTRATOR_HMAC_SECRET`
configure the serve entry point for container deployments.
`harn orchestrator deploy` accepts `--provider render|fly|railway`,
validates the manifest with the orchestrator runtime, writes provider files
under `deploy/<provider>/`, optionally builds/pushes `--image` with
`--build`, and syncs locally available secrets unless `--no-secret-sync` is
set.

## harn routes

Static trigger-surface inventory for a project manifest.

```bash
# Human-readable table of declared trigger routes.
harn routes .

# Stable JSON envelope for automation and deployment audits.
harn routes . --json
```

`harn routes <root>` loads the nearest `harn.toml`, validates the manifest
trigger declarations, and reads local handler source without executing it. The
report includes trigger kind/provider, HTTP route path when the binding exposes
one, local module and handler, match events, declared budgets, statically
required host capabilities, vendor-lock disclosure from provider-specific
imports, and template framework overhead tokens for non-trivial
`render(...)` / `render_prompt(...)` / `render_string(...)` use. Webhook and
A2A-push triggers default to `/triggers/<id>` when no path is declared.

## harn trigger replay

Replay a persisted `TriggerEvent` from a standalone EventLog snapshot
through the dispatcher (no orchestrator needed).

```bash
# Replay an event, re-dispatch against the live binding.
harn trigger replay <event-id>

# Compare replay result vs. original (structured drift JSON).
harn trigger replay <event-id> --diff

# Replay against a historical binding version by timestamp.
harn trigger replay <event-id> --as-of 2026-04-19T12:00:00Z

# Capture a human replay correction for teaching/policy feedback.
harn trigger replay <event-id> --steer-from outcome --to-decision '{"status":"skipped"}' --reason "human corrected routing" --applied-by alice --scope this_persona

# Preview a filtered bulk replay without dispatching anything.
harn trigger replay --where "event.payload.tenant == 'acme' AND attempt.status == 'failed'" --dry-run

# Replay matching records with progress output and throttling.
harn trigger replay --where "attempt.failed_at > '2026-04-18'" --progress --rate-limit 4
```

Sets `HARN_REPLAY=1` during dispatch so nondeterminism in handlers
can fall back to recorded values when the handler cooperates.
`--steer-from` records the human's injected decision as a typed correction
after a successful single-event replay. The step selector accepts `event`,
`outcome`, or an action-graph node id. `this_persona` and `all` correction
scopes tighten derived capability policy for the affected actor until the
matching correction records no longer apply.

Bulk replay selection uses a Harn expression over event-log records with
top-level `event`, `binding`, `attempt`, `outcome`, and `audit` objects.
The CLI accepts SQL-ish convenience syntax for filters: single-quoted
strings plus `AND`/`OR`/`NOT` normalize into the underlying Harn
expression evaluator before dispatch.

`--dry-run` returns the matching records without replaying them.
`--progress` streams per-item progress to stderr, and `--rate-limit`
caps bulk execution throughput in operations per second.

Every bulk replay appends an audit envelope to
`trigger.operations.audit` describing who ran it, when, the normalized
filter, and the affected records.

## harn flow replay-audit

Audit shipped Flow slices against the current `@retroactive` predicate hashes.
Historical slices remain append-only: drift is advisory unless
`--fail-on-drift` is set.
The `--store` path must already exist; the audit command does not create an
empty Flow store.

```bash
harn flow replay-audit --since 2026-04-26
harn flow replay-audit --since 2026-04-26T12:00:00Z --json
harn flow replay-audit --store .harn/flow.sqlite --predicate-root . --touched-dir crates/harn-vm --since 2026-04-26 --fail-on-drift
```

| Flag | Description |
|---|---|
| `--since <date>` | Include shipped derived slices created at or after an RFC3339 timestamp, unix timestamp, or `YYYY-MM-DD` |
| `--store <path>` | SQLite Flow store to audit (default: `.harn/flow.sqlite`) |
| `--root <path>` | Repository root used for `invariants.harn` discovery |
| `--target-dir <path>` | Directory whose effective current predicate set is audited |
| `--fail-on-drift` | Exit non-zero when advisory drift is found |
| `--json` | Emit the replay-audit report as JSON |

## harn trace import

Convert a third-party eval trace into a standard `--llm-mock` fixture.

```bash
harn trace import \
  --trace-file traces/generic.jsonl \
  --trace-id trace_123 \
  --output fixtures/imported.jsonl
```

The source file is JSONL. Each line should contain at least
`{prompt, response}` and may also include `tool_calls`, `model`,
`provider`, token counts, and `trace_id`. The generated fixture can be
used directly with `harn run --llm-mock ...`, `harn eval
--llm-mock ...`, or `harn test --determinism`.

## harn crystallize

Mine repeated traces into a reviewable deterministic workflow candidate.

```bash
harn crystallize \
  --from fixtures/crystallize/version-bump \
  --shadow-from fixtures/crystallize/version-bump-holdout \
  --out workflows/version_bump.harn \
  --report reports/version_bump.crystallize.json \
  --eval-pack evals/version_bump.toml \
  --min-examples 5 \
  --workflow-name version_bump
```

The input directory may contain crystallization trace JSON files or persisted
Harn workflow run records. The report preserves source trace hashes,
parameters, side effects, approval points, capability and secret requirements,
shadow-mode pass/fail details, promotion metadata, and cost/token savings.
Pass `--shadow-from <TRACE_DIR>` to add future/holdout traces to the shadow
comparison without using them for mining. Candidates with divergent side
effects or replay-oracle receipt drift are rejected instead of promoted.

Pass `--bundle <DIR>` to also emit a portable
`harn.crystallization.candidate.bundle` directory (`candidate.json`,
`workflow.harn`, `report.json`, `harn.eval.toml`, redacted `fixtures/`)
that a cloud platform or any other importer can consume without bespoke glue:

```bash
harn crystallize \
  --from fixtures/crystallize/version-bump \
  --out workflows/version_bump.harn \
  --report reports/version_bump.crystallize.json \
  --eval-pack evals/version_bump.toml \
  --bundle bundles/version-bump \
  --bundle-team platform \
  --bundle-repo burin-labs/harn \
  --bundle-risk-level medium \
  --workflow-name version_bump
```

### harn crystallize validate

Smoke-check a bundle on disk: confirms the schema marker and version, that
every referenced file exists, that fixtures are marked redacted, and that
`required_secrets` only lists logical ids (never raw secret values).

```bash
harn crystallize validate bundles/version-bump
```

### harn crystallize shadow

Re-run the deterministic shadow comparison from a bundle's redacted fixtures
in-process, with no live side effects. Returns non-zero on divergence, so it
fits in CI gates.

```bash
harn crystallize shadow bundles/version-bump
```

See [Workflow crystallization](./workflow-crystallization.md) for the trace
schema, bundle layout, and review loop.

The checked-in release/package-maintenance harness can be run with:

```bash
harn crystallize \
  --from crates/harn-vm/tests/fixtures/crystallize_v2_release/mine \
  --shadow-from crates/harn-vm/tests/fixtures/crystallize_v2_release/holdout-pass \
  --out /tmp/release_package_maintenance.harn \
  --report /tmp/release_package_maintenance.report.json \
  --bundle /tmp/release-package-maintenance \
  --min-examples 3 \
  --workflow-name release_package_maintenance \
  --package-name release-workflows \
  --approver release-lead@example.com
```

## harn trigger cancel

Request cancellation for pending or in-flight non-replay trigger
dispatches recorded in the EventLog snapshot.

```bash
# Cancel a single event/binding lineage if it is still active.
harn trigger cancel <event-id>

# Preview which active runs match a filter.
harn trigger cancel --where "attempt.handler == 'handlers::risky'" --dry-run

# Request cancellation for matching runs with progress output.
harn trigger cancel --where "event.payload.tenant == 'acme'" --progress --rate-limit 4
```

Cancellation writes durable control records to `trigger.cancel.requests`
and the dispatcher polls that topic while dispatches are queued,
sleeping between retries, or running local handlers. Terminal runs are
reported as `not_cancellable` and are left unchanged.

## harn trust query

Query trust-graph records from the workspace event log.

```bash
# List all trust records for one agent.
harn trust query --agent github-triage-bot

# Filter by action, tier, and outcome, then emit JSON.
harn trust query \
  --agent github-triage-bot \
  --action github.issue.opened \
  --tier act-auto \
  --outcome success \
  --json

# Aggregate per-agent stats.
harn trust query --summary
```

Supported filters:

- `--agent`
- `--action`
- `--since`
- `--until`
- `--tier`
- `--outcome`
- `--json`
- `--summary`

`--summary` groups records by agent and reports success rate, mean recorded
cost, tier distribution, and outcome distribution.

## harn trust verify-chain

Verify the workspace trust graph's hash chain.

```bash
harn trust verify-chain
harn trust verify-chain --json
```

The command reads `trust_graph` and falls back to legacy `trust.graph` logs,
recomputes every `entry_hash`, and checks that each `previous_hash` points to
the prior record.

## harn trust promote

Manually promote an agent to a higher autonomy tier. This appends a
`trust.promote` control record to the trust graph.

```bash
harn trust promote github-triage-bot --to act-auto
harn trust promote reviewer-bot --to act-with-approval
```

## harn trust demote

Manually demote an agent and record the reason in trust-graph metadata.

```bash
harn trust demote github-triage-bot --to shadow --reason "unexpected mutation"
harn trust demote deploy-bot --to suggest --reason "needs tighter review gate"
```

## harn serve

Start a workflow server through one of the outbound transport adapters.

```bash
harn serve a2a agent.harn                  # explicit A2A
harn serve agent.harn                      # legacy A2A shorthand
harn serve a2a --port 3000 agent.harn      # A2A with custom port
harn serve a2a --bind 0.0.0.0:3000 --api-key "$HARN_SERVE_API_KEY" agent.harn
harn serve acp agent.harn                  # ACP session server over stdio
harn serve acp --profile-json /tmp/acp.ndjson agent.harn
harn serve api agent.harn                  # local OpenAPI + SSE Agents API
harn serve api --bind 127.0.0.1:8787 agent.harn
harn serve mcp server.harn                 # exported pub fn -> MCP tools over stdio
harn serve mcp --transport http server.harn
harn serve test                            # reusable user-test worker over stdio
```

`harn serve test` lets a caller retain one isolated test-run session instead
of spawning a fresh Harn process for every suite. It accepts JSON-RPC 2.0 as
newline-delimited JSON or `Content-Length` frames. Call `initialize` with
`{"protocol_version":"1"}`; its result includes the negotiated protocol and
the Harn `server_version`, stable `worker_id`, and `process_id`. Then call
`test/run` with a test `path` and any of `filter`, `timeout_ms`, `max_test_ms`,
`max_execute_ms`, `parallel`, `fail_fast`, `jobs`, `shard`, `skill_dirs`, or
`diagnose`. Each response includes the same worker identity, typed test
summary, cumulative `run_count`, and cache counters before and after that run.
The advertised `test_run.schema_version` is 2; summaries include the shared
duration distribution plus per-case and aggregate module attribution.
`shutdown` returns the final run and cache receipt; closing stdin also stops the
worker. Every test still receives fresh VM and module state; only reusable
prepared module artifacts are retained by the worker session.

`harn serve api` starts the local Harn Agents HTTP API. It serves
`/openapi.json`, health/version/runtime/capability metadata, session and task
CRUD, SSE event streams, local tool-registry inspection, workspace file helpers,
and permission request response endpoints. Prompt execution, cancellation, and
approval decisions are routed through the packaged ACP session runtime so API
clients share the same transcript, EventLog, replay, and host-permission paths
as ACP hosts. Use `--api-key <key>` / `HARN_SERVE_API_KEY`,
`--hmac-secret <secret>` / `HARN_SERVE_HMAC_SECRET`, and the shared `--tls`
flags to protect non-discovery routes.

`harn serve mcp` uses the shared `harn-serve` dispatch core and maps each
exported `pub fn` in the target module to one MCP tool. Tool schemas are
derived from Harn type annotations. With `--transport http`, the server also
supports Streamable HTTP on `--path` plus the legacy SSE compatibility
endpoints `--sse-path` and `--messages-path`.

For scripts that author the MCP surface through the registration
builtins (`mcp_tools(registry)`, `mcp_resource(...)`, `mcp_prompt(...)`)
instead of `pub fn` exports, `harn serve mcp` auto-detects that mode,
runs the script once, and exposes the registered tools / resources /
prompts over either stdio or Streamable HTTP. Pass `--card <PATH_OR_JSON>`
to advertise an MCP v2.1 Server Card.

```bash
harn serve mcp agent.harn                  # auto-detect surface
harn serve mcp agent.harn --card ./card.json
```

`harn serve a2a` uses the shared `harn-serve` dispatch core and exposes each
exported `pub fn` in the target module as an A2A skill. The adapter publishes an
AgentCard at `/.well-known/agent-card.json`, keeps the legacy
`/.well-known/a2a-agent`, `/.well-known/agent.json`, and `/agent/card` aliases,
and supports task send, send-and-wait, streaming/resubscribe, push callback
registration, and cancel propagation. The legacy shorthand `harn serve <file>`
is preserved and rewrites internally to `harn serve a2a <file>`. The listener
binds to `127.0.0.1:8080` by default; use `--bind 0.0.0.0:PORT` only with
explicit auth and TLS or a trusted edge proxy.

`harn serve acp` starts the packaged ACP adapter on stdio for editor and IDE
hosts. Pass `--transport websocket` to expose the same single-connection ACP
session server at `--bind` and `--path` instead. It creates ACP sessions,
executes the target pipeline for each `session/prompt`, streams `AgentEvent`
values as `session/update` notifications, and forwards permission prompts
through `session/request_permission`. Use `--api-key <key>` /
`HARN_SERVE_API_KEY` or `--hmac-secret <secret>` /
`HARN_SERVE_HMAC_SECRET` to advertise ACP `authMethods` and require
`authenticate` before protected session methods. WebSocket clients can also
pre-authenticate the upgrade with `Authorization: Bearer <key>` or
`X-API-Key`.
Use `--profile` / `HARN_PROFILE=1` to print one categorical timing rollup per
executed `session/prompt`; use `--profile-json <path>` /
`HARN_PROFILE_JSON=<path>` to append per-turn NDJSON records with
`turn`, `session_id`, and `rollup`. File-backed ACP prompts report `vm_setup`
as a profile bucket and reuse a prepared VM baseline while preserving clean
per-turn execution state. `--trace` / `HARN_TRACE=1` enables the LLM trace
summary printed when the stdio server shuts down.

See [MCP and ACP Integration](./mcp-and-acp.md) and
[Outbound workflow server](./harn-serve.md) for protocol details.

## harn mcp

Manage standalone OAuth state for remote HTTP MCP servers.

```bash
harn mcp redirect-uri
harn mcp login notion
harn mcp login https://mcp.notion.com/mcp
harn mcp login my-server --url https://example.com/mcp --client-id <id> --client-secret <secret>
harn mcp status notion
harn mcp logout notion
```

`harn mcp login` resolves the server from the nearest `harn.toml` when you pass
an MCP server name, or uses the explicit URL when you pass `--url` or a raw
`https://...` target. The CLI:

- discovers OAuth protected resource and authorization server metadata
- prefers pre-registered `client_id` / `client_secret` values when supplied
- falls back to dynamic client registration when supported by the server
- stores tokens in the local OS keychain and refreshes them automatically

`harn mcp status` prints the configured server state. For OAuth-backed HTTP
servers with a vetted identity descriptor and captured token-response metadata,
human output includes the connected account/workspace and `--json` includes
`display_identity`. Harn scripts see the same field through `harn.mcp.status()`,
alongside the server `transport` and `url`.

Relevant flags:

| Flag | Description |
|---|---|
| `--url <url>` | Explicit MCP server URL when logging in/out by a custom name |
| `--client-id <id>` | Use a pre-registered client ID instead of dynamic registration |
| `--client-secret <secret>` | Optional client secret for `client_secret_post` / `client_secret_basic` servers |
| `--scope <scopes>` | Override or provide requested OAuth scopes |
| `--redirect-uri <uri>` | Override the default loopback redirect URI (default shown by `harn mcp redirect-uri`) |

Security guidance:

- prefer the narrowest scopes the remote MCP server supports
- treat configured `client_secret` values as secrets
- review remote MCP capabilities before using them in autonomous workflows

## Maintainer release scripts

Harn's repo-maintainer release scripts are documented in
[Maintainer release workflow](./maintainer-release.md). They are not part of
the end-user `harn` CLI surface.

## harn add

Add a dependency to `harn.toml`.

```bash
harn add github.com/burin-labs/harn-openapi@v1.2.3
harn add @burin/notion-sdk@1.2.3
harn add @burin/notion-sdk@1.2.3 --registry ./harn-package-index.toml
harn add https://github.com/user/my-lib --alias my-lib --tag v1.2.3
harn add https://github.com/user/my-lib --alias my-lib --rev v1.2.3
harn add https://github.com/user/my-lib --alias my-lib --branch main
harn add my-lib --git https://github.com/user/my-lib --tag v1.2.3   # legacy form
```

Git dependencies must specify a stable `tag`/`rev` or an explicit `branch`.
`harn.lock` records the resolved tag when present, commit, and content hash
used for reproducible installs. Registry-name dependencies resolve through the
package index and then write the same git dependency shape as direct GitHub
installs; hand-authored manifests may also use registry semver ranges such as
`my-lib = { version = "^1.2" }`.

## harn install

Install dependencies declared in `harn.toml`, writing or reusing
`harn.lock` and atomically publishing direct plus transitive dependencies as an
immutable package generation.

```bash
harn install
harn install --frozen
harn install --locked --offline
harn install --refetch my-lib
harn install --json
```

`--locked` is a CI-oriented alias for `--frozen`: Harn fails if
`harn.toml` and `harn.lock` disagree. `--offline` also implies locked
behavior and fails instead of fetching when a locked git package is
missing from the shared cache. `--json` emits a structured install
summary suitable for IDE-host and cloud-platform automation.

## harn lock

Resolve dependencies from `harn.toml` and write `harn.lock` without
materializing packages.

```bash
harn lock
```

## harn update

Refresh one dependency (or all of them) and update `harn.lock`.

```bash
harn update my-lib
harn update --all
harn update --all --json
```

`--json` emits a structured update summary, mirroring `harn install`.

## harn remove

Remove one dependency from `harn.toml` and `harn.lock`, then publish a new
generation without that package.

```bash
harn remove my-lib
```

## harn pack

Build a `.harnpack` run bundle from a Harn entrypoint
([#1781][issue-1781]). The bundle is a deterministic tar.zst archive
containing a v2 `WorkflowBundle` manifest (`harnpack.json`), every
transitively-imported `.harn` source and non-Harn import asset under
`sources/`, every module's precompiled `.harnbc`/`.harnmod` artifact
under `bytecode/`, a provider-catalog hash + stdlib version pin, an
archived SPDX-lite 2.3 SBOM (`sbom.spdx.json`), and an optional
Ed25519 signature slot.

```bash
harn pack examples/hello.harn
harn pack examples/hello.harn --out /tmp/hello.harnpack
harn pack examples/hello.harn --sign --key ~/.harn/release-ed25519.pem
harn pack examples/hello.harn --unsigned --json
harn pack workflows/new-entry.harn --upgrade old.harnpack --out new.harnpack
```

Output paths default to `<entrypoint>.harnpack` next to the entrypoint.
`--json` emits a `JsonEnvelope` with `bundle_hash` (BLAKE3 over the
canonical manifest bytes plus sorted content hashes), `output_path`,
`size_bytes`, signature status, SBOM counts, bytecode/debug-symbol
metadata, and the full manifest; see the catalog row at
`harn --json-schemas --command pack` for the schema version and inline
JSON Schema. Repacking the same source produces a byte-identical
archive.

`--upgrade <old.harnpack>` reads a prior bundle (v1 JSON or v2 archive)
and re-emits it under the v2 manifest, preserving the prior bundle's
id, name, version, triggers, workflow graph, and prompt capsules. The
new `<entrypoint>` argument supplies the transitive-module + SBOM
payload that the older schema lacked.

`--sign --key <path>` loads an Ed25519 PKCS#8 PEM private key, signs the
canonical bundle hash, embeds the signature in the manifest, and appends an
OpenTrustGraph `release` record. `--unsigned` skips the manifest signature but
still appends the release record at autonomy tier `suggest`; this is also the
default when neither signing flag is provided.

`--exclude-secrets` refuses secret-looking entrypoints and skips imported
non-Harn assets whose paths match `.env`, `.env.*`, `*.pem`, `*.key`,
`credentials*`, or a `secrets/` directory. Skipped assets are reported as
`JsonEnvelope.warnings` entries and in `manifest.metadata.skipped_assets`.

[issue-1781]: https://github.com/burin-labs/harn/issues/1781

## harn package list

List packages from the current `harn.lock`.

```bash
harn package list
harn package list --json
```

The report includes each locked package's source, materialization status,
integrity status, package version, Harn compatibility range, exported
modules/tools/skills, permissions, and host requirements.

## harn package doctor

Diagnose package install and contract issues.

```bash
harn package doctor
harn package doctor --json
```

Doctor checks for missing or stale lockfiles, missing materialized packages,
content-hash mismatches, declared host capability gaps, and invalid installed
package tool/skill metadata. Publish-readiness checks remain under
`harn package check`, so applications can use doctor without adding package
metadata that only published libraries need.

## harn package scaffold openapi

Create a focused Harn SDK package from a local or remote OpenAPI 3.1 spec.

```bash
harn package scaffold openapi \
  --name acme-sdk-harn \
  --module-name acme_sdk \
  --client-name AcmeClient \
  --spec ./openapi.json \
  --out ./acme-sdk-harn
```

The scaffold writes `harn.toml`, generated `src/lib.harn`, the source spec
under `openapi/`, `scripts/regen.harn`, `tests/smoke.harn`, docs, README,
license, and package CI. Run `harn install` inside the package before checking
or testing so the declared `harn-openapi` regeneration dependency is
materialized.

Useful flags:

| Flag | Description |
|---|---|
| `--default-base-url <url>` | Override the first `servers[].url` from the spec |
| `--harn-openapi-path <dir>` | Use a local `harn-openapi` checkout for generation |
| `--harn-openapi-git <url-or-path>` | Dependency source written to `harn.toml` |
| `--harn-openapi-rev <rev>` | Pin the dependency to a rev or tag |
| `--harn-openapi-branch <branch>` | Track a branch instead of the default pinned rev |
| `--force` | Overwrite generated files in an existing output directory |

## harn package search

Search the configured package registry index.

```bash
harn package search notion
harn package search --registry ./harn-package-index.toml --json
```

The registry source comes from `--registry`, `HARN_PACKAGE_REGISTRY`,
`[registry].url` in `harn.toml`, or Harn's default hosted index.
Private HTTP(S) registries can be accessed with
`HARN_PACKAGE_REGISTRY_TOKEN`; Harn sends it as a bearer token when fetching
the index and any archive URLs listed by that index.

Registry versions may resolve to either a git source or an archive source.
Archive versions use `archive = "<url>"` plus `checksum = "sha256:<tree>"`;
Harn expands `.tar.gz` archives into the package cache and verifies the
expanded tree hash before writing `harn.lock`.

## harn rule publish / search

Publish and discover structural rule packs through the package registry.

```bash
harn rule publish ./acme-rules --registry-name @acme/rules --dry-run
harn rule search typescript
harn rule search --registry ./harn-package-index.toml --json
```

`harn rule publish` is a rule-pack alias over `harn publish`: it requires the
package manifest to declare `[rules] ruleDirs`, validates the rule files, and
adds rule-pack metadata to the package-index row. `harn rule search` filters the
package registry to rule packs and reports the pack description, languages, rule
count, and safety summary.

## harn package info

Show registry metadata for one package, optionally at a specific version.

```bash
harn package info @burin/notion-sdk
harn package info @burin/notion-sdk@1.2.3 --json
```

Metadata includes repository, license, Harn compatibility, exported
modules, connector contract compatibility, docs URL, versions, and any
checksum/provenance fields present in the index.

## harn package cache

Inspect and maintain the shared git package cache.

```bash
harn package cache list
harn package cache verify
harn package cache verify --materialized
harn package cache clean
harn package cache clean --all
```

`verify` recomputes cached package content hashes and compares them with
`harn.lock`; `--materialized` also checks the leased current generation. `clean`
removes cache entries not referenced by the current lockfile, while
`--all` clears all cached git package entries.

## harn package outdated

Compare the resolved entries in `harn.lock` against newer registry
versions or upstream branch HEADs.

```bash
harn package outdated
harn package outdated --remote
harn package outdated --json
```

Registry-attributed dependencies (added via `harn add @scope/name@version`)
are checked against the configured registry index; pass `--registry`
to override or `--refresh` to bypass any local cache. Plain git
dependencies report `skipped` unless `--remote` is set, in which case
Harn shells out to `git ls-remote` to detect branch HEAD drift. Path
dependencies always report `skipped` because they live-link.

## harn package audit

Audit `harn.lock` provenance, package compatibility, and supply-chain
integrity in one pass.

```bash
harn package audit
harn package audit --json
harn package audit --skip-materialized
```

The audit verifies that:

- The lock generator/protocol-artifact versions match the running Harn.
- Each entry carries provenance (`package_version`, `manifest_digest`).
- Resolved packages still satisfy their `harn` compatibility range.
- Cached and materialized git packages still hash to the lockfile entry.
- The materialized package's `harn.toml` digest still matches the lock.
- Registry-attributed entries have not been yanked from the index.
- Publishable manifests are not pinned to path dependencies.

Each finding has a stable `code` (`lockfile-stale`, `harn-compat-violation`,
`yanked-registry-version`, etc.) so downstream automation can route
specific failure modes without parsing the human report.

## harn package artifacts

Inspect or verify the published Harn protocol-artifact contract.

```bash
harn package artifacts manifest
harn package artifacts manifest --output vendor/manifest.json
harn package artifacts check vendor/manifest.json
harn package artifacts check vendor/manifest.json --json
```

`manifest` prints (or writes) the protocol-artifact manifest the running
Harn binary would emit — the same JSON that `make gen-protocol-artifacts`
ships under `spec/protocol-artifacts/manifest.json`. `check` compares a
vendored copy of that manifest against the running Harn and reports any
drift. Hosts that vendor Harn protocol bindings (IDE hosts, cloud platforms,
custom integrators) call `check` from CI to detect when a Harn bump
would require regenerating their bindings.

## harn version

Show version information.

```bash
harn version
```
