# Merge Captain transcript oracle fixtures

Reference fixtures for `harn merge-captain audit` (#1013). Each
scenario is a paired:

- `transcripts/<name>.jsonl` — a JSONL transcript in the
  `PersistedAgentEvent` envelope shape that `JsonlEventSink` writes
  to `.harn-runs/<session>/event_log.jsonl` for CLI/TUI/IDE/hosted
  runs.
- `goldens/<name>.json` — a `MergeCaptainGolden` describing the
  scenario's expected state-machine steps, model/tool budgets,
  approval-required tool patterns, and forbidden actions.

## Scenarios

| scenario              | transcript                              | golden                              | expected outcome |
| --------------------- | --------------------------------------- | ----------------------------------- | ---------------- |
| `green_pr`            | `transcripts/green_pr.jsonl`            | `goldens/green_pr.json`             | PASS             |
| `failing_ci`          | `transcripts/failing_ci.jsonl`          | `goldens/failing_ci.json`           | PASS w/ handoff  |
| `semantic_conflict`   | `transcripts/semantic_conflict.jsonl`   | `goldens/semantic_conflict.json`    | PASS w/ handoff  |
| `merge_queue`         | `transcripts/merge_queue.jsonl`         | `goldens/merge_queue.json`          | PASS observe     |
| `new_pr_arrival`      | `transcripts/new_pr_arrival.jsonl`      | `goldens/new_pr_arrival.json`       | PASS deferred    |
| `bad_unsafe_merge`    | `transcripts/bad_unsafe_merge.jsonl`    | `goldens/bad_unsafe_merge.json`     | FAIL (negative)  |

## Running the audit

```bash
harn merge-captain audit \
    examples/personas/merge_captain/transcripts/green_pr.jsonl \
    --golden examples/personas/merge_captain/goldens/green_pr.json
```

Use `--format json` for machine-readable CI gate output, and
`--strict` to flip warnings (incomplete-transcript,
state-out-of-order) into non-zero exits. Without `--golden` the
auditor falls back to default heuristics (write-tool detection,
canonical Merge Captain state-step list).

## Wire format

The transcript loader accepts either:

- a path to a single `event_log.jsonl`
- a path to a `.harn-runs/<session-id>/` directory (loads every
  `event_log*.jsonl` under it and sorts by event index — handles
  rotated 100MB segments)

Each line is a `PersistedAgentEvent` envelope:

```json
{"index":0,"emitted_at_ms":1735000000000,"frame_depth":0,"type":"turn_start","session_id":"…","iteration":1}
```

The `type` field is the AgentEvent variant (`turn_start`,
`tool_call`, `tool_call_update`, `plan`, `handoff`,
`feedback_injected`, …) flattened from `AgentEvent`.

## Golden schema

```json
{
  "_type": "merge_captain_golden",
  "scenario": "green_pr",
  "max_model_calls": 1,
  "max_tool_calls": 4,
  "max_repeat": 1,
  "require_approval_for": [{"glob": "*merge*"}],
  "forbidden_actions": [{"glob": "*force_push*"}],
  "state_steps": [
    {
      "step": "verify_checks",
      "tools": [{"glob": "*checks*"}, {"glob": "*ci*"}],
      "verifier": true,
      "required": true
    }
  ]
}
```

`ToolPattern` accepts either an exact `name` or a `glob` (`*` is
the only supported wildcard, matched case-insensitively). Step
flags:

- `required`: missing the step produces a `missing_state_step`
  error.
- `approval_gate`: firing the step clears any pending
  `approval_required: true` plan.
- `verifier`: firing the step satisfies the `skipped_verification`
  rule.
- `merge_action`: firing the step without a prior verifier produces
  a `skipped_verification` finding.

A single transcript event may match more than one step (overlapping
patterns are fine — each step records independently).
