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
  const _ = hostlib_code_index_rebuild({root: "."})
  const snap = verification_file_hash_snapshot(["src/main.zig", "build.zig"])
  const verdict = verification_diagnostic_classify(
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
  const affected = verification_affected_targets(
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
  const result = {success: false, exit_code: 1, duration_ms: 420, stderr: "failed"}
  const recorded = verification_record_check_result(
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
  const facts = verification_toolchain_facts([
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

## Warm-State Facts

`verification_warm_state_facts(rows, options)` executes config-declared
readiness probes for slow verifier state such as build servers, watchers, and
toolchain caches. Rows are data, not code: a profile row or fact row declares a
`warmMode` / `warm_state` object with a `readyProbe`, optional cache identity,
and expected warm-vs-cold delta. Harn does not hardcode sbt, Gradle, Cargo,
Zig, or any other stack.

```harn
import { verification_ladder_plan, verification_warm_state_facts } from "std/verification"

pipeline default() {
  const warm = verification_warm_state_facts([
    {
      id: "scala/full",
      name: "sbt",
      warmMode: {
        mode: "build-server",
        readyProbe: {spec: {mode: "shell", command: "test -S .bloop/socket"}},
        expectedWarmDeltaMs: 8000,
      },
      cacheIdentity: {BLOOP_HOME: ".bloop"},
    },
  ])
  const plan = verification_ladder_plan(
    {path: "modules/cart/src/Main.scala", language: "scala", task: "post_edit"},
    {timing_kind: "auto", warm_state_facts: warm},
  )
  return plan.selected
}
```

Each fact includes:

- `id` and `name`: stable row identity, usually matching a verification profile
  row id.
- `configured`: whether a warm mode or readiness probe was declared.
- `available` / `ready` / `warm`: the probe result normalized into scheduler
  booleans. A missing or failing probe makes the row cold, not an error.
- `timing_kind`: `warm` or `cold`, ready to feed ladder planning.
- `expected_warm_delta_ms`, `warm_ms`, and `cold_ms`: caller-declared or
  profile-derived timing hints.
- `cache_identity`: cache/build-server identity fields that make warm-state
  facts auditable.
- `probe`: status, exit, duration, stdout, and stderr from the readiness probe.

Pass the returned list to `verification_ladder_plan` with
`{timing_kind:"auto", warm_state_facts:facts}` to choose warm or cold p95 timing
per row. This keeps slow-toolchain scheduling in Harn profile data instead of
embedding stack-specific string heuristics in a host product.

## Warm-State Lifecycle

`verification_start_warm_state(row, options)` and
`verification_teardown_warm_state(receipt, options)` manage reusable verifier
processes without inventing a second process manager. The row stays data:
`warmMode.startCommand` / `start_command` / `spawnCommand` is a normal
`std/command` spec, `readyProbe` remains the readiness fact, and optional
`teardownCommand` / `stopCommand` handles stack-specific cleanup. Harn owns the
receipt shape and scheduler policy; hostlib owns the background process handle,
process-group cancellation, and session cleanup.

```harn
import {
  verification_start_warm_state,
  verification_teardown_warm_state,
} from "std/verification"

pipeline default() {
  const row = {
    id: "scala/build-server",
    warmMode: {
      mode: "build-server",
      startCommand: {mode: "shell", command: "sbt --client"},
      readyProbe: {spec: {mode: "shell", command: "test -S .bloop/socket"}},
    },
  }
  const receipt = verification_start_warm_state(row)
  // Later, or during test teardown:
  return verification_teardown_warm_state(receipt).cancelled
}
```

If the readiness probe is already warm, `verification_start_warm_state`
returns `status: "ready"` and does not spawn. Pass a prior receipt as
`existing` or `receipt` to reuse an already-started handle while the process is
still warming. Teardown cancels the stored background handle through
`std/command::command_cancel` and then runs any declared stop command.

## Ladder Planning

`verification_profile_matches(query, dir?)` returns every profile row that
matches `{repo?, path?, language?, task?}`, ordered by the same
most-specific-wins selector semantics as `verification_profile_resolve`.
Each result is `{row, specificity, index}` so scheduler code can rank rows
without reimplementing profile matching.

`verification_ladder_plan(query, options)` builds an auditable plan from those
matched rows:

```harn
import { verification_ladder_plan } from "std/verification"

pipeline default() {
  const plan = verification_ladder_plan(
    {path: "src/main.zig", language: "zig", task: "post_edit"},
    {resource_classes: ["cheap", "moderate"], max_rung: "R3"},
  )
  return plan.selected
}
```

The planner is data-only. It never hardcodes languages, toolchains, commands,
or build systems. It ranks matching rows by declared rung (`R0` before `R1`
through `R5`), resource class, observed p95 timing, selector specificity, and
row order. `selected` contains runnable checks only; matched rows without a
command or spec are returned in `skipped` with `reason = "missing_command"`.
Each selected entry includes the original row, row id, rung, command, runnable
flag, granularity, trust, resource class, p95 timing, timing kind, optional
warm-state fact, and sort key. Execution stays with `verification_run_check` or
`verification_start_check` /
`verification_finish_check`, so background checks remain snapshot-bound.

Useful options:

- `dir`: profile-store directory to read from.
- `min_rung` / `max_rung`: inclusive rung bounds, default `R0` to `R5`;
  values may be `R2`, `"2"`, or `2`.
- `resource_classes`: allow-list such as `["cheap", "moderate"]`.
- `timing_kind`: `warm` (default), `cold`, `any`, or `auto`; `auto` uses
  `warm_state_facts` per row and falls back to warm timing when no fact exists.
- `warm_state_facts`: facts from `verification_warm_state_facts`, either a
  list keyed by `id` / `row_id` or a dict keyed by profile row id.
- `unknown_p95_ms`: fallback timing for rows without observations.
- `allow_stale_prone`: set `false` to skip stale-prone rows.
- `limit`: maximum selected checks.

When no profile rows match or every row is filtered out, the returned plan sets
`fallback = true` and leaves current caller behavior unchanged unless the
caller explicitly opts into consuming the plan.

## Verification HUD Model

`verification_hud_model(state, options)` builds a structured, host-agnostic
verification status model from explicit host facts and Harn profile rows:

```harn
import { verification_hud_model, verification_hud_text } from "std/verification"

pipeline default() {
  const model = verification_hud_model(
    {
      started_ms: 1000,
      last_edit_ms: 61000,
      path: "src/a.tx",
      language: "toolchainx",
      task: "post_edit",
      current_hashes: {"src/a.tx": "h2"},
      last_check: {
        rung: "R2",
        verdict: "failed",
        at_ms: 110000,
        bound_snapshot: {"src/a.tx": "h1"},
      },
    },
    {now_ms: 121000},
  )
  return verification_hud_text(model)
}
```

The returned `harn.verification.hud_model.v1` shape includes elapsed timing,
time since last edit, last-check freshness, the next runnable ladder row, the
full-verify row, and the underlying ladder plans. Hosts should render the
structured fields directly where possible; `verification_hud_text(model)`
exists for prompt and transcript surfaces that still need compact text.

`verification_snapshot_staleness(bound_snapshot, current_hashes)` is the pure
freshness helper used by the HUD model. A missing current hash is treated as
stale/unverifiable, matching the stricter VM stale-diagnostic contract: a red
cannot feed user-visible confidence unless every file in its bound snapshot
still has the same current hash. Gate consumers should continue to use
`verification_gate_input`, which also enforces rung/timestamp/snapshot binding.

## Diagnostic Delta

`verification_diagnostic_delta(previous, current, options)` compares previous
and current diagnostics as row-normalized signature sets. It is the reusable
Harn-side primitive for answering whether a verification failure is genuinely
new, unchanged, advanced, cleared, or advisory without embedding that logic in
a host product or in agent-loop string heuristics:

```harn
import { verification_diagnostic_delta } from "std/verification"

pipeline default() {
  const delta = verification_diagnostic_delta(
    {diagnostics: ["src/a.zig:10:2: error: missing writeEscaped"]},
    {diagnostics: ["src/a.zig:44:9: error: missing writeEscaped"]},
    {row: {diagnosticDelta: {strip_locations: true}}},
  )
  return delta.status
}
```

The result shape is auditable:

- `status`: `initialized`, `unchanged`, `advanced`, `cleared`, `regressed`, or
  `advisory`.
- `progress_credit`: true only when a gate-bearing stuck diagnostic was removed,
  changed, reduced, or cleared.
- `feedsGates` / `feeds_gates`: copied from the current diagnostic contract.
  Current advisory, stale, or unbound diagnostics always return
  `status = "advisory"` and never receive progress credit.
- `previous` / `current`: normalized `{count, signatures, feedsGates}` facts.
- `removed`, `added`, and `persisted`: set differences over the normalized
  signatures.
- `normalization`: the merged normalization config used for the comparison.

Inputs can be a string signature, a single diagnostic dict, a check/result dict,
or a `{diagnostics:[...]}` / `{errors:[...]}` envelope. Previous diagnostics
with `feedsGates = false` are not used as a baseline; a fresh current failure
after such a baseline returns `initialized`, not progress.

Normalization is data-driven. `diagnosticDelta`, `diagnostic_delta`,
`signatureNormalization`, or `signature_normalization` may be declared on a
profile row or its `check`, then overridden by direct options. Supported knobs
include `lowercase`, `strip_ansi`, `strip_locations`, `strip_temp_paths`,
`collapse_whitespace`, `include_path`, `include_code`, `include_severity`,
`max_chars`, `replacements`, `strip_patterns`, and `ignore_patterns`.

## Gate Input

`verification_gate_input(previous, current, current_hashes, options)` is the
canonical Harn-side reducer for no-progress, escalation, and completion policy.
It classifies both diagnostics with `verification_diagnostic_classify`, demotes
stale or unbound diagnostics to advisory, and then runs
`verification_diagnostic_delta` over only gate-bearing signatures:

```harn
import { verification_gate_input } from "std/verification"

pipeline default() {
  const current_hashes = {"src/writer.zig": "new"}
  const previous = {
    rung: "R2",
    at: "2026-07-03T00:00:00Z",
    snapshot: {"src/writer.zig": "old"},
    diagnostics: ["src/writer.zig:10:2: error: missing writeEscaped"],
  }
  const current = {
    rung: "R2",
    at: "2026-07-03T00:01:00Z",
    snapshot: current_hashes,
    diagnostics: ["src/writer.zig:99:7: error: missing writeEscaped"],
  }
  return verification_gate_input(
    previous,
    current,
    current_hashes,
    {row: {diagnosticDelta: {strip_locations: true}}},
  )
}
```

The returned shape is the object loop policy should consume instead of
reassembling freshness and signature state from strings:

- `status`: `advisory`, `initialized`, `unchanged`, `advanced`, `cleared`, or
  `regressed`.
- `feedsGates` / `feeds_gates`: true only when the current diagnostic is
  snapshot-bound and fresh.
- `progress_credit`: true only when a fresh, gate-bearing diagnostic advanced
  or cleared.
- `no_progress`: true for the same fresh normalized signature set.
- `gate_bearing_failure` / `blocks_completion`: true when the current fresh
  diagnostic still contains failure signatures.
- `stale` and `stale_files`: copied from the current diagnostic classification.
- `previous` / `current`: the classified values passed into the delta reducer.
- `delta`: the full `verification_diagnostic_delta` receipt.

When `current_hashes` is `nil`, the reducer preserves the diagnostic's existing
gate status instead of calling the freshness classifier with an empty hash map.
Callers that have a current file-hash snapshot should pass it; callers that
already classified a diagnostic can embed `classification` on the diagnostic.
`std/agent/stall` uses this reducer for its current-failure model, so stale or
unbound diagnostics remain advisory and do not increment no-progress streaks.

This is the DRY seam for stale-diagnostic enforcement: a red diagnostic bound
to an older file hash is advisory and cannot feed no-progress detectors,
escalation streaks, verifier signatures, or completion gates. A newer fresh
green result at the same or higher rung clears the gate without requiring
callers to compare transcript order or line-number-shifted strings.
