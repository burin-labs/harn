# Merge Captain persona

Merge Captain is a Harn-native runbook for owning pull-request queues
across `harn`, `harn-cloud`, and `burin-code` (or any other repo whose
policy you check in here). It replaces the shell-driven sweep MVP with
a deterministic state machine, durable per-PR checkpoints, and a typed
GitHub adapter that rides the connector contract — no raw
`process.exec` against `gh` shell strings.

## What it does

- Discovers open PRs in every configured repo on every sweep.
- Classifies each PR into one of twelve canonical states (see
  [`lib/states.harn`](lib/states.harn)) using a pure observation
  classifier ([`lib/classifier.harn`](lib/classifier.harn)).
- Persists each PR's state to a session-scoped agent-state checkpoint
  ([`lib/checkpoint_store.harn`](lib/checkpoint_store.harn)) so the
  sweep loop survives cancellation and process restart.
- Runs the action the classification calls for: comment, request
  review, update branch, run local verification, enqueue on the merge
  queue, escalate to a human, etc.
- Emits one merge receipt per PR per sweep
  ([`lib/receipt.harn`](lib/receipt.harn)) capturing classification,
  action, evidence, approval state, commands run, observed checks, and
  final outcome — plus a sweep-level summary that aggregates by state,
  action, and repo.
- Treats the GitHub merge queue as the first-class merge path; admin
  bypass is never the action it picks.
- Picks up brand new PRs on the very next sweep, and reconciles PRs
  that disappeared from the open list with a synthetic closing
  receipt.

## Layout

| Path | Role |
|---|---|
| [`harn.toml`](harn.toml) | Persona manifest entry. |
| [`manifest.harn`](manifest.harn) | Entry pipeline. One run = one sweep. |
| [`lib/states.harn`](lib/states.harn) | Canonical states, transitions, actions. |
| [`lib/policy.harn`](lib/policy.harn) | Per-repo policy schema, defaults, validation. |
| [`lib/observation.harn`](lib/observation.harn) | Normalized PR observation shape. |
| [`lib/classifier.harn`](lib/classifier.harn) | Pure (observation, policy) -> (state, action). |
| [`lib/checkpoint_store.harn`](lib/checkpoint_store.harn) | `std/agent_state`-backed per-PR persistence. |
| [`lib/github_adapter.harn`](lib/github_adapter.harn) | Live (connector) and fixture-mode GitHub I/O. |
| [`lib/local_verify.harn`](lib/local_verify.harn) | Runs per-repo local verification commands. |
| [`lib/receipt.harn`](lib/receipt.harn) | Per-PR + sweep receipt builders. |
| [`lib/scheduler.harn`](lib/scheduler.harn) | The actual sweep loop. |
| [`policies/*.json`](policies) | Per-repo policy fixtures for harn / harn-cloud / burin-code. |
| [`fixtures/github_snapshot.json`](fixtures/github_snapshot.json) | Deterministic GitHub state for evals. |
| [`tests/`](tests) | Pure-Harn unit + scheduler tests. |
| [`evals/merge_captain_smoke.json`](evals/merge_captain_smoke.json) | Smoke eval suite. |
| [`runs/merge_captain_smoke.run.json`](runs/merge_captain_smoke.run.json) | Recorded run for the smoke eval. |

## States

`discovered`, `draft`, `waiting_checks`, `behind`, `dirty`, `queued`,
`merge_group_running`, `failing_ci`, `local_repair`, `blocked`,
`merged`, `closed`. The legal-edges table lives in
[`lib/states.harn`](lib/states.harn) and is enforced by
`classifier.verify(...)` after every classification — illegal jumps
trigger an automatic `escalate_human` rather than a silent state
update.

## Running locally

```bash
# Dry-run sweep over the checked-in fixtures (default behaviour).
harn run personas/merge_captain/manifest.harn

# Persona inspection + smoke eval.
harn persona --manifest personas/merge_captain/harn.toml inspect merge_captain --json
harn eval personas/merge_captain/evals/merge_captain_smoke.json

# Unit tests for every layer.
harn test personas/merge_captain/tests/states_test.harn
harn test personas/merge_captain/tests/policy_test.harn
harn test personas/merge_captain/tests/classifier_test.harn
harn test personas/merge_captain/tests/receipt_test.harn
harn test personas/merge_captain/tests/scheduler_test.harn
```

## Running against live GitHub

`manifest.harn` accepts a structured input via
`runtime_pipeline_input()`:

```json
{
  "mode": "live",
  "policy_paths": [
    "personas/merge_captain/policies/harn.json",
    "personas/merge_captain/policies/harn-cloud.json",
    "personas/merge_captain/policies/burin-code.json"
  ],
  "state_root": "/var/lib/harn/merge-captain/state",
  "session_id": "production",
  "writer_id": "merge_captain@host-1",
  "dry_run": false,
  "sweep_id": "live-2026-04-29T17:30Z"
}
```

In live mode the adapter dispatches through the
[`std/connectors/github`](../../crates/harn-modules/src/stdlib/stdlib_connectors_github.harn)
connector. You still need a registered `github` connector client — the
adapter never shells out to `gh`. Local verification commands are the
one exception; they run via `process.exec` per the per-repo
`local_verification` list.

## Invariants

- One writer per session. The checkpoint store opens with
  `conflict_policy: "error"`, so two concurrent sweeps will error
  rather than race.
- Classifier is pure and deterministic. Tests cover every transition
  branch.
- Every transition is checked against the legal-edges table; failures
  rewrite the action to `escalate_human`.
- Mutating actions are gated by either `dry_run`, autopilot allow-list,
  or human approval — the receipt's `approval_state` is the audit
  trail.

## Customizing for a new repo

1. Copy one of the JSON files in [`policies/`](policies) and edit the
   fields. `repo` is required; everything else has a sane default in
   [`lib/policy.harn`](lib/policy.harn).
2. Add the path to `policy_paths` in your runtime input.
3. (Optional) Add a per-repo entry in
   [`fixtures/github_snapshot.json`](fixtures/github_snapshot.json) to
   exercise the new policy under `harn test`.

## Provenance

This implementation closes
[harn#1009](https://github.com/burin-labs/harn/issues/1009).
