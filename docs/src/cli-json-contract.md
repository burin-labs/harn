# `harn --json` contract

Every `harn` subcommand that exposes a machine-readable mode emits a
versioned JSON envelope to **stdout**. Logs, progress, and warnings
continue to go to **stderr** so a `--json` pipeline stays a single
parseable document.

This page is the agent-facing contract. It cross-links the per-command
shapes and explains the envelope discipline so an automated caller can
drive Harn end-to-end without parsing prose.

Tracking epic: [#1753](https://github.com/burin-labs/harn/issues/1753).

## Envelope shape

Every `--json` payload is a [`JsonEnvelope<T>`][envelope-impl] with the
same five fields. `schemaVersion` is the per-command discriminator —
agents dispatch on it to handle multiple Harn releases concurrently.

```jsonc
{
  "schemaVersion": 1,        // per-command, monotonically increasing
  "ok": true,                // false on hard failure; warnings keep ok=true
  "data": { "...": "..." },  // command-specific payload, null on error
  "error": null,             // { "code", "message", "details" } when ok=false
  "warnings": []             // [ { "code", "message" } ]; always [] not absent
}
```

[envelope-impl]: https://github.com/burin-labs/harn/blob/main/crates/harn-cli/src/json_envelope.rs

### Discovery

The full catalog of registered commands and their current schema
version is available at runtime via the top-level `--json-schemas`
flag — itself an envelope:

```bash
harn --json-schemas | jq '.data[] | {command, schemaVersion}'
harn --json-schemas --command lint   # filter to one entry
```

### Error shape

`error.code` is a stable lowercase identifier (e.g. `"lint_failed"`,
`"run_record_load_failed"`). `error.message` is a human-readable
sentence. `error.details` is a free-form JSON object, `null` when the
command has no structured payload to attach.

### Streaming commands

A small number of commands emit **NDJSON** (one envelope-shaped event
per line) rather than a single document. Today this set is `harn run
--json` and `harn dev --watch --json`. Each line still carries
`schemaVersion`; consumers can `jq -c` over the stream.

`harn run --emit-summary-json`, `--emit-phase-json`, and
`--emit-rusage-json` are intentionally separate from this envelope
stream. Each emits one raw NDJSON object to stderr by default or to a
dedicated file/fd sink when supplied, so wrappers can read auxiliary
run metadata without changing the `harn run --json` stdout contract.

## Supported commands

These commands accept `--json` and emit a stable, schema-versioned
envelope. Run `harn --json-schemas` for the live list with current
versions.

| Command                        | Notes                                                    |
| ------------------------------ | -------------------------------------------------------- |
| `harn check --json`            | Per-file static check diagnostics + summary              |
| `harn check provider-matrix --json` | Provider/model capability matrix                    |
| `harn check connector-matrix --json` | Connector package capability matrix                |
| `harn fmt --json`              | Per-file formatting result for write and check modes     |
| `harn lint --json`             | Per-file lint diagnostics + autofix availability         |
| `harn parse --json`            | Tagged Harn AST with byte spans                          |
| `harn tokens --json`           | Lexer token stream with source lexemes                   |
| `harn run --json`              | Streaming NDJSON event log (stdout/stderr/tool/result)   |
| `harn run --emit-summary-json` | One terminal raw NDJSON summary object on stderr/file/fd  |
| `harn run --emit-phase-json`   | One terminal raw NDJSON phase object on stderr/file/fd    |
| `harn run --emit-rusage-json`  | One terminal raw NDJSON CPU sample on stderr/file/fd      |
| `harn replay --json`           | Per-stage replay summary + embedded fixture verdict      |
| `harn test conformance --json` | Conformance results, xfail accounting, and duration distribution |
| `harn graph --json`            | Static module graph: symbols, imports, capabilities      |
| `harn routes --json`           | Trigger route + budget + capability inventory            |
| `harn dev --watch --json`      | Streaming NDJSON incremental rebuild events              |
| `harn time run --json`         | Per-phase wall-clock + per-LLM/tool-call latency         |
| `harn fix plan --json` / `apply --json` | Repair plan or applied edits, plus skipped invalid files |
| `harn pack --json`             | `.harnpack` bundle build summary (inline schema)         |
| `harn doctor --json`           | Capability matrix: host, targets, providers, effects     |
| `harn explain <CODE> --json`   | Per-diagnostic-code explanation                          |
| `harn explain --catalog --json` | Full diagnostic-code catalog                            |
| `harn session export --json`   | Portable session bundle export                           |
| `harn provider catalog show --json` | Resolved provider/model catalog snapshot                 |
| `harn models batch plan --json` | Provider Batch API candidates plus `batch.harn_live_adapter` support |
| `harn models batch manifest --json` | Durable offline batch manifest summary and request groups |
| `harn models batch prepare --json` | Provider-native batch request files, deterministic prepare receipt, and normalized `lifecycle` state |
| `harn models batch submit --json` | Batch submission receipt with provider job ids, dry-run operations, and normalized `lifecycle` state |
| `harn models batch status --json` | Provider batch status receipt with dry-run cached status validation and normalized `lifecycle` counts |
| `harn models batch cancel --json` | Batch cancellation receipt with redacted cancel operations, skipped-job reasons, and normalized `lifecycle` counts |
| `harn models batch download --json` | Provider result-file download receipt with artifact paths, hashes, and normalized `lifecycle` counts |
| `harn models batch execute init\|advance\|inspect\|cancel --json` | Durable execution receipt with stable identities, revision/history, artifact digests, and provider-operation recovery state |
| `harn models batch rejoin --json` | Manifest-ordered normalized result artifact plus exact consumable/quarantine counts, ids, reasons, and source identities |
| `harn models lora plan --json` | LoRA/QLoRA route, trainer, data, eval, promotion evidence, launch contract, `training.contract.tool_catalog`, and `serving.serving_requirements` |
| `harn models lora inspect --json` | PEFT adapter compatibility report, launch metadata, and `serving.serving_requirements` |
| `harn models lora export --json` | Trainer dataset export report with contract id, tool-catalog contract, stats, and promotion evidence contract |
| `harn models lora manifest --json` | LoRA training-run manifest with route, data, artifact, tool-catalog contract, structured serving requirements, and promotion evidence contracts |
| `harn models lora train --json` | LoRA trainer receipt with backend argv/status, `backend.argv_required` when argv is omitted, input hashes, route/tool-catalog metadata, `dataset_audit`, and post-training commands |
| `harn models lora preflight --json` | Corpus readiness report before LoRA training |
| `harn connect status --json` / `setup-plan --json` | Connector readiness reports        |
| `harn skill list --json` / `get --json` | Canonical Harn skill corpus frontmatter        |
| `harn version --json`          | CLI build metadata (`name`, `version`, `description`, optional `source_revision`)    |
| `harn upgrade --json`          | Self-update probe (`--check`) or install summary         |

## Per-command notes

### `harn version --json`

```json
{
  "schemaVersion": 1,
  "ok": true,
  "data": {
    "name": "harn-cli",
    "version": "0.8.27",
    "description": "CLI for the Harn programming language — run, test, REPL, format, and lint",
    "source_revision": "0123456789abcdef0123456789abcdef01234567"
  },
  "error": null,
  "warnings": []
}
```

`source_revision` is an optional additive field under schema version 1. It is
the full immutable object ID supplied when the binary was built, or `null` when
that build carried no revision attestation. The command never guesses it from
the caller's current directory or a runtime environment variable. Consumers
that credit measurements to an exact revision should require a non-null exact
match; version-only consumers may ignore the field.

### `harn upgrade --json`

`upgrade --json --check` is the lowest-risk probe: it resolves the
target release without downloading. Combined with a real install, the
envelope is printed after the install action so callers can read the
final `installed` flag.

```jsonc
{
  "schemaVersion": 1,
  "ok": true,
  "data": {
    "current": "0.8.27",
    "target": "v0.8.27",
    "needs_upgrade": false,
    "mode": "check",
    "installed": false,
    "archive_url": "https://github.com/burin-labs/harn/releases/download/v0.8.27/harn-aarch64-apple-darwin.tar.gz",
    "checksums_url": "https://github.com/burin-labs/harn/releases/download/v0.8.27/SHA256SUMS",
    "target_triple": "aarch64-apple-darwin"
  }
}
```

### `harn lint --json`

Mirrors the per-file diagnostic shape of `harn check --json` so agent
consumers can dispatch on a single `CheckDiagnostic` layout regardless
of whether they invoked `check` or `lint`.

- `data.summary.fixable` counts diagnostics carrying autofix edits;
  `fixed` is the count actually applied (always `0` when `--fix` is
  not set).
- `--json` is intentionally orthogonal to `--fix`: agents plan
  repairs from the report and apply them in a follow-up `harn lint
  --fix` or `harn fix apply`.
- `--changed-from <REV>` adds `data.changed` without changing the ordinary lint
  payload. It records the requested and resolved `from`/`to` commits plus every
  evaluated source path, change status, previous rename/copy path when present,
  and inclusive one-based `added_lines` ranges. `data.files[].diagnostics`
  contains matching warning/error diagnostics and all information diagnostics;
  the file statuses and summary counters are recomputed from that filtered set.

```jsonc
{
  "schemaVersion": 1,
  "ok": false,
  "data": {
    "files": [
      {
        "path": "src/agent.harn",
        "status": "warning",
        "diagnostics": [
          {
            "source": "lint",
            "severity": "warning",
            "code": "HARN-LNT-032",
            "message": "comparison to `false` is redundant",
            "span": { "start": 128, "end": 142 }
          }
        ],
        "fixable": 1,
        "fixed": 0
      }
    ],
    "summary": {
      "ok": 0,
      "warnings": 1,
      "errors": 0,
      "diagnostics": 1,
      "fixable": 1,
      "fixed": 0
    },
    "changed": {
      "from": { "requested": "origin/main", "commit": "<full commit id>" },
      "to": { "requested": "HEAD", "commit": "<full commit id>" },
      "files": [
        {
          "path": "src/agent.harn",
          "status": "modified",
          "added_lines": [{ "start": 9, "end": 9 }]
        }
      ]
    }
  },
  "error": {
    "code": "lint_failed",
    "message": "one or more changed lines failed `harn lint`",
    "details": null
  },
  "warnings": []
}
```

### `harn replay --json`

Loads a persisted run record and emits a structured per-stage summary
plus the embedded replay-fixture verdict. `harn replay --fixture <path>`
accepts either a run record or a `harn.orchestration.replay_trace.v1`
fixture. `harn replay --session-id <id> --events-db <path>` reconstructs
the same replayable run-record shape from the SQLite EventLog
`observability.agent_events.<id>` topic.

The default `--runs 1` response keeps the original single `ReplayReport`
payload. When `--runs N` is greater than one, the response keeps
`schemaVersion`, `ok`, `error`, and `warnings` at the top level and also
emits top-level `reports`, `runs`, and `determinism` fields. `reports`
contains one `ReplayReport` per replay read, `runs` contains the
allowlist-normalized event sequences, and `determinism` reports the
allowlist-stripped replay comparison. `ok: false` with `error.code:
"replay_fixture_failed"` indicates at least one fixture verdict failed;
`error.code: "replay_determinism_failed"` indicates the per-run event
material diverged after applying the replay allowlist.

`harn replay --session-id <id> --counterfactual <plan.harn>` evaluates an
alternate edit plan after the replay source has been rehydrated at the
`--at` cutoff and attaches the divergence to the single `ReplayReport`
under `data.counterfactual`: `{ plan_path, plan_paths, step_count, result,
diverged: [{ path, status, lines_added, lines_removed }], files_touched,
lines_added, lines_removed, ops_applied, ops_rejected }`. Repeat
`--counterfactual` to chain plans; the returned edit-op lists are
concatenated into one cumulative `edit.dry_run`. The field is omitted for
a plain replay. A plan that fails to load or evaluate exits non-zero with
`error.code: "replay_counterfactual_failed"`.

### `harn test --json-out`

User-test reports are standalone JSON documents rather than envelopes. Schema
v2 adds typed timeout and phase records plus suite-level timing and aggregate
work attribution:

```json
{
  "schemaVersion": 2,
  "suite": "user",
  "root": "/workspace/tests",
  "duration_ms": 31,
  "timing": {
    "sample_count": 1,
    "average_ms": 30,
    "p50_ms": 30,
    "p90_ms": 30,
    "p95_ms": 30,
    "p99_ms": 30
  },
  "aggregate": {
    "collection_ms": 1,
    "setup_ms": 0,
    "compile_ms": 0,
    "execute_ms": 30,
    "teardown_ms": 0,
    "modules": {
      "module_compile_ms": 4,
      "module_load_ms": 7,
      "modules_compiled": 1,
      "modules_loaded": 2
    }
  },
  "summary": {
    "total": 1,
    "passed": 0,
    "failed": 0,
    "timed_out": 1,
    "skipped": 0
  },
  "cases": [{
    "name": "test_timeout",
    "file": "test_timeout.harn",
    "classname": "test_timeout.harn",
    "outcome": "timed_out",
    "duration_ms": 30,
    "timeout": { "phase": "execute", "limit_ms": 30 },
    "phases": {
      "setup_ms": 0,
      "compile_ms": 0,
      "execute_ms": 30,
      "teardown_ms": 0,
      "modules": {
        "module_compile_ms": 4,
        "module_load_ms": 7,
        "modules_compiled": 1,
        "modules_loaded": 2
      }
    },
    "message": "execute phase timed out after 30ms"
  }]
}
```

`timeout`, `phases`, and `message` are omitted when unavailable. An empty
distribution has `sample_count: 0` and `null` for every duration statistic.
Discovery and worker-start error rows are not duration samples. Aggregate
phases are cumulative worker-time and can exceed suite wall time under parallel
execution. Nested module values overlap setup/execute and are never additive.

### `harn test conformance --json`

Conformance schema v2 retains the standard envelope and adds the same typed
duration distribution under `data.timing`:

```json
{
  "schemaVersion": 2,
  "ok": true,
  "data": {
    "snapshotKey": "<blake3-hex>",
    "results": [{
      "name": "pass.harn",
      "outcome": "pass",
      "duration_ms": 12,
      "message": null,
      "diagnostic_codes": []
    }],
    "summary": {
      "pass": 1,
      "fail": 0,
      "xfail_expected": 0,
      "xfail_unexpected_pass": 0,
      "skipped": 0
    },
    "timing": {
      "sample_count": 1,
      "average_ms": 12,
      "p50_ms": 12,
      "p90_ms": 12,
      "p95_ms": 12,
      "p99_ms": 12
    }
  },
  "error": null,
  "warnings": []
}
```

### `harn serve test`

The JSON-RPC `initialize` result advertises
`capabilities.test_run.schema_version: 2`. Each `test/run` result uses
snake-case `schema_version: 2` and contains worker identity, run/cache counters,
and `summary`. The summary is the user-runner shape above: `results`, verdict
counts, wall duration, `timing`, and cumulative `aggregate`. Each executed result
carries optional typed `timeout` and measured `phases` with nested module
attribution; discovery and worker-start errors omit unavailable phases.

### `harn run --emit-summary-json`

The post-run summary is a raw NDJSON line, not a `JsonEnvelope`, because
it is an auxiliary sink rather than the command's primary stdout JSON
mode. `--summary-file <path>` overwrites the file with the one-line
summary; `--summary-fd <fd>` writes the same line to an already-open
Unix file descriptor.

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

`profile` is omitted unless `--profile` or `--profile-json` is active.
The LLM counters are enabled by the summary flag itself, so callers do
not need to add `--trace` to receive `llm.call_count`, token totals,
LLM time, and accumulated cost.

### `harn run --emit-phase-json`

The phase sink is also a raw NDJSON line. It preserves the same
seven-row contract as `harn time run --json`, but routes it to a
separate sink so a parent wrapper can spawn `harn run` and recover
parse/typecheck/compile/setup/main timing without parsing stdout.
`--phase-file <path>` overwrites the file with the one-line phase
object; `--phase-fd <fd>` writes the same line to an already-open Unix
file descriptor.

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

The phase order is fixed: `parse`, `typecheck`, `bytecode_compile`,
`run_setup`, `run_main`, `module_compile`, `module_load`. Cache hits keep all
seven rows and switch the
`bytecode_compile` row to `"cache": "hit"` while leaving `parse` and
`typecheck` at `duration_ms: 0`. `kind` is the machine-readable addition rule:
only `top_level` rows reconcile wall time. The final two `attribution` rows
overlap top-level phases; their `events` values count successful compiles and
unique fresh-VM loads.

### `harn run --emit-rusage-json`

The rusage sink is a raw NDJSON line carrying the process CPU sample
needed by subprocess wrappers that cannot call `getrusage` in-process.
`--rusage-file <path>` overwrites the file with the one-line object;
`--rusage-fd <fd>` writes the same line to an already-open Unix file
descriptor.

```json
{
  "schema_version": 1,
  "event": "run_rusage",
  "cpu_ms": 320
}
```

## Compatibility

- `schemaVersion` is bumped when the data shape changes in a way
  agents need to detect. Additive optional fields can land without a
  bump.
- Errors are machine-readable: `error.code` is a stable identifier;
  `error.message` carries the human sentence; `error.details` is
  free-form structured context.
- Streaming commands keep the same envelope shape per line.
- `--json` mode never mixes human chatter into stdout. Anything
  diagnostic — progress bars, warnings about flags, network logs —
  goes to stderr.

## When in doubt

Run `harn <subcommand> --help` to confirm `--json` is supported, and
`harn --json-schemas --command <subcommand>` to see the current schema
version. If a subcommand is missing from the catalog, that's a bug
worth filing.
