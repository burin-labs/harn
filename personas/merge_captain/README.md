# Merge captain persona

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
| [`lib/repair_bundle.harn`](lib/repair_bundle.harn) | Pure builders + JSON schema for the repair-worker checkpoint contract. |
| [`lib/prompt_pack.harn`](lib/prompt_pack.harn) | Cheap/value-model prompt pack: narrow decision prompts, strict JSON schemas, golden examples, compact context budgets, and revision gates. |
| [`lib/repair_validator.harn`](lib/repair_validator.harn) | Deterministic validators for repair-worker output (missing tests, dirty worktrees, unexpected write scope, malformed output, unpushed commits). |
| [`lib/approval.harn`](lib/approval.harn) | Approval gate for repair-worker action kinds (semantic_repair / force_push / admin_merge / release_tag / branch_delete). |
| [`lib/repair_worker.harn`](lib/repair_worker.harn) | The only `agent_loop` caller. Builds the bundle, runs the gate, dispatches the worker, runs the validators, and returns a `merge_captain.repair_run` record. |
| [`lib/receipt.harn`](lib/receipt.harn) | Per-PR + sweep receipt builders, including the `merge_captain.repair_run_link` summary. |
| [`lib/scheduler.harn`](lib/scheduler.harn) | The actual sweep loop. Routes `local_repair` / `dirty` to the repair worker when policy enables it. |
| [`policies/*.json`](policies) | Per-repo policy fixtures for harn / harn-cloud / burin-code. |
| [`fixtures/github_snapshot.json`](fixtures/github_snapshot.json) | Deterministic GitHub state for evals. |
| [`tests/`](tests) | Pure-Harn unit + scheduler tests. |
| [`evals/merge_captain_smoke.json`](evals/merge_captain_smoke.json) | Smoke eval suite. |
| [`evals/prompt_pack_value_model.json`](evals/prompt_pack_value_model.json) | Deterministic golden fixture for the Gemma-class prompt pack. |
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
harn eval personas/merge_captain/harn.eval.toml
harn merge-captain ladder personas/merge_captain/harn.eval.toml \
  --report-out .harn-runs/merge-captain-ladder/report.json \
  --format json

# Unit tests for every layer.
harn test personas/merge_captain/tests/states_test.harn
harn test personas/merge_captain/tests/policy_test.harn
harn test personas/merge_captain/tests/classifier_test.harn
harn test personas/merge_captain/tests/receipt_test.harn
harn test personas/merge_captain/tests/scheduler_test.harn
harn test personas/merge_captain/tests/prompt_pack_test.harn
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
[`std/connectors/github`](../../crates/harn-stdlib/src/stdlib/stdlib_connectors_github.harn)
connector. You still need a registered `github` connector client — the
adapter never shells out to `gh`. Local verification commands are the
one exception; they run via `process.exec` per the per-repo
`local_verification` list.

## Timeout ladders

`harn.eval.toml` ships a value-model timeout ladder for the green-PR
fixture. It runs the same transcript across increasing timeout/tool-call
tiers for the `gemma-value` route, emits per-tier JSONL transcripts,
receipts, and summaries, and marks the first tier that completed correctly.
Run it through `harn eval`, `harn test package --evals`, or
`harn merge-captain ladder` depending on whether you want eval-pack output
or a standalone machine-readable ladder report.

## Cheap-model prompt pack

`lib/prompt_pack.harn` keeps value-model calls narrow enough for
Gemma-class/local routes. It exposes separate prompts for:

- PR classification
- next deterministic action selection
- CI failure category diagnosis
- repair-result summarization
- approval-request decisions
- release changelog audit/rewrite

Every prompt has a strict JSON schema, one golden example, and an explicit
context budget. Context assembly is deterministic and excludes raw logs,
full diffs, and full threads; CI diagnosis can only see log spans already
selected by a log summarizer. The release changelog audit prompt consumes
deterministic git-log/changelog evidence plus shell/tool observations, so
failed commands stay visible to the model instead of becoming hidden harness
retries.

Prompt revisions are gated by the golden fixture, transcript-oracle diffs,
and timeout-ladder results. The core boundary is intentionally simple:
deterministic Harn code owns side effects, model outputs are recommendations,
and validators decide whether any recommendation can be used.

## Repair-worker checkpoint contract

Most Merge Captain steps are deterministic, but a few require coding
judgment: semantic merge conflicts, CI failure diagnosis, flaky-test
hardening, release-note audits. Those run through a bounded
`agent_loop` worker called at explicit repair checkpoints. The
contract has four pieces:

1. **Typed bundle** (`lib/repair_bundle.harn`). The deterministic
   harness packages a `merge_captain.repair_bundle` v1 record
   covering: repo, PR, base/head SHAs, changed files, conflict paths
   or failing checks, relevant logs, allowed write scope (path
   globs), required verification commands, push target with
   `force_with_lease`, and the action kinds that require approval.
   Bundle kinds are `ci_failure`, `merge_conflict`, `release_audit`,
   `flaky_test`. The release-audit kind carries the typed
   `release_facts` and `release_outputs` shape used by
   [harn#1146](https://github.com/burin-labs/harn/issues/1146).
2. **Approval gate** (`lib/approval.harn`). Per-repo
   `policy.repair_worker.{require_human_for, autopilot_action_kinds,
   pre_approved}` decides whether the worker can proceed without a
   human for each of `semantic_repair`, `force_push`, `admin_merge`,
   `release_tag`, `branch_delete`. The default is "human required for
   everything" — autopilot is opt-in per action kind.
3. **Worker dispatch** (`lib/repair_worker.harn`). The only
   `agent_loop` caller in the persona. It validates the bundle, runs
   the gate, calls `agent_loop` with the prompt + the
   `repair_bundle.output_schema()` JSON schema, runs every
   deterministic validator, and returns a versioned
   `merge_captain.repair_run` record.
4. **Deterministic validators** (`lib/repair_validator.harn`). Every
   worker output is checked against five guards:
   - **missing tests** — `tests_run` is empty or fails to cover every
     `required_verification` entry, or any test reports `status !=
     "ok"`.
   - **unexpected write scope** — any path in `files_changed` falls
     outside the bundle's `allowed_write_scope` glob list.
   - **malformed output** — any required key on the `repair_output`
     schema is missing or has the wrong type.
   - **unpushed commits** — `commits_created` is non-empty but the
     `push_result.pushed` flag is not true, the ref is missing, or
     the ref does not match the bundle's push target.
   - **dirty worktree** — opt-in `harness_validate(...)` runs `git
     status --porcelain` after the worker returns and fails closed if
     the tree is not clean.

The receipt links back to the parent merge-captain PR state through
the `repair_run` field on `merge_receipt`, which carries a compact
`merge_captain.repair_run_link` summary (bundle kind, status,
approval state, output counts, agent_loop telemetry). The full
`repair_run` record is the audit trail.

## Enabling the repair worker

The repair worker is opt-in per repo. Add a `repair_worker` block to
the policy JSON:

```json
{
  "repair_worker": {
    "enabled": true,
    "handles_kinds": ["ci_failure"],
    "require_human_for": [
      "semantic_repair",
      "force_push",
      "admin_merge",
      "release_tag",
      "branch_delete"
    ],
    "autopilot_action_kinds": [],
    "model": "claude-sonnet-4-6",
    "max_iterations": 24,
    "profile": "tool_using"
  }
}
```

When the scheduler classifies a PR as `local_repair` (CI failure with
local verification configured), and the policy enables the worker for
`ci_failure`, the dispatcher builds a CI-failure bundle and routes
through `repair_worker.invoke(...)` instead of running
`run_local_verification` inline. Same for `merge_conflict` when
`handles_kinds` includes it.

For non-Merge-Captain consumers (release-harness in
[harn#1146](https://github.com/burin-labs/harn/issues/1146)), call
`repair_worker.invoke_for_release_audit(...)` directly with the
typed git/changelog facts.

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
- `agent_loop` is invoked **only** at explicit repair checkpoints
  inside `lib/repair_worker.harn`. The classifier and scheduler stay
  pure; no other Merge Captain module calls into the LLM.

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
[harn#1009](https://github.com/burin-labs/harn/issues/1009). The
repair-worker checkpoint contract closes
[harn#1010](https://github.com/burin-labs/harn/issues/1010) and
shares the typed bundle/output shapes with
[harn#1146](https://github.com/burin-labs/harn/issues/1146).
