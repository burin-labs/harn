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
| `harn replay --json`           | Per-stage replay summary + embedded fixture verdict      |
| `harn test conformance --json` | Conformance results with xfail accounting                |
| `harn graph --json`            | Static module graph: symbols, imports, capabilities      |
| `harn routes --json`           | Trigger route + budget + capability inventory            |
| `harn dev --watch --json`      | Streaming NDJSON incremental rebuild events              |
| `harn time run --json`         | Per-phase wall-clock + per-LLM/tool-call latency         |
| `harn fix plan --json` / `apply --json` | Repair plan or applied edits at a safety ceiling |
| `harn pack --json`             | `.harnpack` bundle build summary (inline schema)         |
| `harn doctor --json`           | Capability matrix: host, targets, providers, effects     |
| `harn explain <CODE> --json`   | Per-diagnostic-code explanation                          |
| `harn explain --catalog --json` | Full diagnostic-code catalog                            |
| `harn session export --json`   | Portable session bundle export                           |
| `harn provider-catalog --json` | Resolved provider/model catalog snapshot                 |
| `harn connect status --json` / `setup-plan --json` | Connector readiness reports        |
| `harn skills list --json` / `get --json` | Canonical Harn skill corpus frontmatter        |
| `harn version --json`          | CLI build metadata (`name`, `version`, `description`)    |
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
    "description": "CLI for the Harn programming language — run, test, REPL, format, and lint"
  },
  "error": null,
  "warnings": []
}
```

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

### `harn replay --json`

Loads a persisted run record and emits a structured per-stage summary
plus the embedded replay-fixture verdict. `ok: false` with
`error.code: "replay_fixture_failed"` indicates the fixture did not
pass; the same envelope still includes the full `data` payload so
callers can diff.

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
