# Merge Captain persona

The Merge Captain persona is a Harn-native runbook for owning
pull-request queues across multiple repositories. It is the
recommended starting point for teams that want a deterministic,
receipt-emitting merge workflow rather than a shell-driven sweep
script.

The full persona package lives at
[`personas/merge_captain/`](https://github.com/burin-labs/harn/tree/main/personas/merge_captain)
and ships with policies for `harn`, `harn-cloud`, and `burin-code`.

## What it owns

- A canonical 12-state machine over each tracked PR (`discovered`,
  `draft`, `waiting_checks`, `behind`, `dirty`, `queued`,
  `merge_group_running`, `failing_ci`, `local_repair`, `blocked`,
  `merged`, `closed`).
- Per-repo policy: merge method, merge-queue toggle, required checks,
  required review count, blocking labels, optional ready label,
  per-repo `local_verification` commands, and downstream bump/release
  ordering.
- Durable per-PR checkpoints written through
  [`std/agent_state`](../agent-state.md) so the sweep loop survives
  cancellation and process restart, and so two sweeps cannot race —
  the second writer hits the
  `conflict_policy: "error"` guard.
- One signed merge receipt per (sweep, repo, PR) capturing the
  classification, the action chosen, the evidence the classifier saw,
  the approval state under the per-repo autopilot policy, the
  commands actually executed, the checks observed, and the final
  outcome. A sweep-level summary aggregates by state, action, and
  repo.
- A typed GitHub adapter that dispatches through
  [`std/connectors/github`](../stdlib/connectors-github.md) in live
  mode and through a deterministic JSON snapshot in fixture mode.
  Tests, evals, and replay all run on fixtures — no `gh` shelling.

## Running the sweep

```bash
# Default fixture sweep — useful for evals and CI.
harn run personas/merge_captain/manifest.harn

# Persona inspection.
harn persona --manifest personas/merge_captain/harn.toml \
  inspect merge_captain --json

# Smoke eval.
harn eval personas/merge_captain/evals/merge_captain_smoke.json

# Unit tests for every layer.
harn test personas/merge_captain/tests/states_test.harn
harn test personas/merge_captain/tests/policy_test.harn
harn test personas/merge_captain/tests/classifier_test.harn
harn test personas/merge_captain/tests/receipt_test.harn
harn test personas/merge_captain/tests/scheduler_test.harn
```

## Live mode input

`manifest.harn` reads its configuration via
[`runtime_pipeline_input()`](../stdlib/runtime.md), so the same entry
pipeline serves both the smoke fixture and a production sweep.

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

The `github` connector must be active for live mode. Local
verification commands run via the host's `process.exec` capability;
no other I/O leaves the typed connector layer.

## Authoring per-repo policy

Policies are JSON files under
[`personas/merge_captain/policies/`](https://github.com/burin-labs/harn/tree/main/personas/merge_captain/policies).
Only `repo` is required; everything else inherits from
`policy.defaults()` in
[`lib/policy.harn`](https://github.com/burin-labs/harn/blob/main/personas/merge_captain/lib/policy.harn).

```json
{
  "repo": "burin-labs/harn",
  "default_branch": "main",
  "merge_method": "squash",
  "merge_queue_enabled": true,
  "required_checks": ["ci / rust", "ci / portal", "ci / conformance"],
  "required_review_count": 1,
  "blocking_labels": ["do-not-merge", "needs-revision", "release-block"],
  "ready_label": null,
  "max_in_flight": 8,
  "local_verification": [
    {"name": "make-test", "command": "make test"},
    {"name": "make-conformance", "command": "make conformance"}
  ],
  "downstream": [{"repo": "burin-labs/burin-code", "action": "fetch_harn_bump"}],
  "bump_after_merge": null,
  "autopilot_states": ["queued", "waiting_checks", "behind"],
  "require_human_for": ["blocked", "local_repair"]
}
```

`autopilot_states` lists states whose mutating actions can fire
without a human approval. `require_human_for` lists states that
always escalate, even if their action is technically non-mutating.
Both lists are evaluated against the per-PR classification, and the
result lands in the receipt's `approval_state` field
(`autopilot`, `dry_run`, `needs_human`, or `no_mutation`).

## Receipt shape

```json
{
  "_type": "merge_receipt",
  "version": 1,
  "persona": "merge_captain",
  "sweep_id": "...",
  "observed_at": 1761729600000,
  "repo": "burin-labs/harn",
  "pr_number": 1010,
  "head_sha": "...",
  "prior_state": "discovered",
  "classification": {
    "state": "queued",
    "action": "enqueue_merge_queue",
    "reason": "all gates passed; enqueueing on the merge queue"
  },
  "evidence": {
    "classifier": {"merge_method": "squash"},
    "snapshot": {"approvals": 1, "labels": [], "...": "..."}
  },
  "approval_state": "autopilot",
  "commands_run": [{"action": "enqueue_merge_queue", "response": {...}}],
  "checks_observed": [{"name": "ci / rust", "status": "completed", "conclusion": "success"}],
  "final_outcome": "enqueued",
  "policy_summary": {"merge_method": "squash", "merge_queue_enabled": true}
}
```

`harn` always prefers the merge queue when `merge_queue_enabled` is
true — admin-merge bypass is never an action the classifier picks.
