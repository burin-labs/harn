# Verification

`std/verification` is the Harn-owned home for deterministic verification
facts. It keeps reusable verification substrate in Harn rather than in a
particular host product: hosts supply file bytes and code-index state, while
Harn policies consume explicit facts.

## File-Hash Snapshots

`verification_file_hash_snapshot(paths)` captures current on-disk hashes for a
batch of workspace paths under one code-index sequence binding:

```harn
import { verification_file_hash_snapshot } from "std/verification"

pipeline default() {
  let _ = hostlib_code_index_rebuild({root: "."})
  let snap = verification_file_hash_snapshot(["src/main.zig", "build.zig"])
  let verdict = verification_diagnostic_classify(
    {rung: "R2", rowId: "zig/file", at: snap.captured_at_ms, snapshot: snap.snapshot},
    snap.snapshot,
  )
  return verdict.status
}
```

The returned shape includes:

- `seq`: the current code-index version-log sequence at capture time.
- `snapshot`: a direct path-to-hash map accepted by
  `verification_diagnostic_classify`.
- `missing`: paths whose current bytes could not be read.
- `files`: per-path metadata including readability, index membership,
  current hash, hash source (`indexed`, `disk`, or `missing`), indexed hash,
  mtime, size, and last edit sequence.

The hash algorithm is the same decimal FNV-1a 64-bit value used by
`hostlib_code_index_file_hash` and `hostlib_code_index_changes_since`.
Unknown-but-readable files are still included in `snapshot` with `known =
false`, which lets a diagnostic bind to a newly created file before the next
full code-index rebuild.

## Affected-Target Facts

`verification_affected_targets(changed_paths, adapters, options)` resolves the
build/test targets affected by file changes through data-declared adapter rows.
Adapters provide a command `spec` and a `parser_id`; Harn owns the parser
contract, while project stacks add rows without changing Rust or host glue:

```harn
import { verification_affected_targets } from "std/verification"

pipeline default() {
  let affected = verification_affected_targets(
    ["crates/app/src/lib.rs", "apps/web/src/index.ts"],
    [
      {id: "cargo", parser_id: "cargo.metadata.v1", spec: {mode: "shell", command: "cargo metadata --format-version=1 --no-deps"}},
      {id: "web", parser_id: "js.workspace_graph.v1", spec: {mode: "shell", command: "pnpm nx graph --file=/dev/stdout"}},
    ],
  )
  return affected.targets
}
```

Parser ids currently shipped by `std/verification`:

- `harn.targets_json.v1`: stdout JSON is a target list or `{targets: [...]}`
  for custom stacks and synthetic adapters.
- `harn.targets_lines.v1`: stdout is one target id per line.
- `cargo.metadata.v1`: Cargo metadata JSON maps changed files to package
  roots and package targets.
- `js.workspace_graph.v1`: JS workspace/project graph JSON maps changed files
  to project roots and declared build/test targets.

If adapters produce no targets, the helper falls back to the existing
code-index graph by returning the changed file plus reverse importers as
conservative file-level targets. Disable that with `{fallback:false}`.
Command strings may use the standard template engine with
`changed_paths_json`, `changed_paths_space`, and `root` bindings; the space
binding is shell-quoted by the helper.

## Check Timing Capture

`verification_observation_from_command_result(result, options)` converts a
normalized command/check result into the canonical profile-store observation
shape:

```harn
import { verification_record_check_result } from "std/verification"

pipeline default() {
  let result = {success: false, exit_code: 1, duration_ms: 420, stderr: "failed"}
  let recorded = verification_record_check_result(
    "cargo/test",
    result,
    {warm: false, snapshot: {"src/lib.rs": "hash1"}, failure_signature_from: "stderr"},
  )
  return recorded.row.timings.coldMs.p95
}
```

The observation uses the existing verification profile store fields:

- `durationMs`: wall-clock duration, folded into warm/cold p50/p95/p99 timing
  windows.
- `warm`: warm/cold classifier supplied by the scheduler or harness.
- `exit`: process/check exit code, with timed-out checks normalized to `124`
  when no exit code is available.
- `failureSignature`: bounded stderr/stdout/combined output for failed checks.
- `snapshot`: optional file-hash binding consumed by stale-diagnostic
  classification.

`verification_record_check_result(row_id, result, options)` applies one such
observation to a profile row and returns `{result, observation, row, snapshot}`.
Unknown row ids return `row = nil`, matching
`verification_profile_record_run`.

`verification_run_check(row_id, spec, options)` is the one-call path for harness
authors: capture optional launch-time `snapshot_paths`, run the command through
`std/command::command_run`, record exactly one profile observation, and return
the same receipt shape. It rejects `command_options.background = true`; use the
background pair below when the check must outlive the current turn.

`verification_start_check(row_id, spec, options)` starts a check in the
background after capturing optional launch-time hashes. It returns a receipt
with the command handle, row id, observation options, and snapshot fact.
`verification_finish_check(receipt, options)` waits for that handle, builds the
same canonical observation, and records the completed result. This keeps
background execution snapshot-bound without blocking the agent turn loop that
launched the check. Finish waits up to 60 seconds by default; pass
`wait_options` to use a shorter poll, a longer wait, or host-specific wait
parameters.

Scheduler policy remains outside these helpers: callers choose the row, rung,
command, warm/cold classification, retry strategy, and when to join background
work.

## Toolchain Identity Facts

`verification_toolchain_facts(rows, options)` executes config-declared
toolchain probes and returns data facts for verification profiles. Harn does
not hardcode a language or toolchain list; each row supplies the command and
version extraction pattern:

```harn
import { verification_toolchain_facts } from "std/verification"

pipeline default() {
  let facts = verification_toolchain_facts([
    {
      id: "go/default",
      name: "go",
      versionProbe: {
        spec: {mode: "shell", command: "go version"},
        versionPattern: "go([0-9]+\\.[0-9]+\\.[0-9]+)",
      },
      cacheIdentity: {GOFLAGS: "-buildvcs=false"},
    },
  ])
  return facts[0]
}
```

Each fact includes:

- `id` and `name`: stable row identity.
- `available`: whether the probe command completed successfully.
- `version`: the first capture from `versionPattern`, when available.
- `raw_version`: stdout, or stderr when stdout is blank.
- `cache_identity`: caller-declared cache/build-server/env facts such as
  `GOCACHE`, `GOFLAGS`, `ZIG_LOCAL_CACHE_DIR`, `SBT_OPTS`, or equivalent
  project-specific fields.
- `probe`: status, exit, duration, stdout, and stderr from the command result.

Missing or failing probes return `available = false` facts instead of throwing.
This lets verification schedulers bind false-fail risk to concrete toolchain
and cache identity without embedding toolchain-specific heuristics in Burin or
other host products.
