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
