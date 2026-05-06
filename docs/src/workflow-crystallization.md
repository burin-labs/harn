# Workflow Crystallization

Workflow crystallization is the review loop for turning repeated agent traces
into deterministic Harn code:

1. Capture ordered traces from runs, host shims, or imported records.
2. Mine a conservative workflow candidate from repeated action sequences.
3. Generate readable Harn plus a machine-readable report.
4. Shadow-check the candidate against the source fixtures without mutating
   external systems.
5. Review promotion metadata, capability boundaries, secrets, rollback target,
   and eval pack link.
6. Package or promote the approved workflow so later runs use CPU/interpreter
   steps for the stable portion and reserve model calls for ambiguity.

The first Harn-side substrate intentionally avoids broad unsupervised discovery.
It looks for repeated contiguous action sequences, extracts scalar parameters
from fields that vary across examples, rejects candidates with divergent side
effects, and marks any model-dependent step as a fuzzy segment.

## Trace Input

`harn crystallize` accepts a directory of JSON files. Each file can be either:

- a crystallization trace with `version`, `id`, and ordered `actions`
- a persisted Harn workflow run record, which is normalized into the same trace
  shape

The crystallization trace format preserves ordered actions, tool calls, model
calls, human approvals, file mutations, external API calls, observed outputs,
costs, timestamps, source hashes, and optional Flow provenance references:

```json
{
  "version": 1,
  "id": "trace_release_001",
  "source_hash": "sha256:...",
  "flow": {
    "trace_id": "trace_01J...",
    "agent_run_id": "run_01J...",
    "transcript_ref": "runs/release-001.json",
    "atom_ids": [],
    "slice_ids": []
  },
  "actions": [
    {
      "id": "checkout",
      "kind": "tool_call",
      "name": "git.checkout_branch",
      "parameters": {
        "repo_path": "/work/harn",
        "branch_name": "release-0.7.41"
      },
      "capabilities": ["git.write"],
      "side_effects": [
        {"kind": "git_ref", "target": "release-branch", "capability": "git.write"}
      ],
      "duration_ms": 30
    },
    {
      "id": "manifest",
      "kind": "file_mutation",
      "name": "update_manifest_version",
      "parameters": {"version": "0.7.41"},
      "inputs": {"path": "harn.toml", "version": "0.7.41"},
      "capabilities": ["fs.write"],
      "side_effects": [
        {"kind": "file_write", "target": "harn.toml", "capability": "fs.write"}
      ]
    }
  ]
}
```

Secrets are references such as `CRATES_IO_TOKEN`, not raw token values.

## CLI

Run the miner against at least five traces of the same repeated workflow:

```bash
harn crystallize \
  --from fixtures/crystallize/version-bump \
  --out workflows/version_bump.harn \
  --report reports/version_bump.crystallize.json \
  --eval-pack evals/version_bump.toml \
  --min-examples 5 \
  --workflow-name version_bump \
  --package-name release-workflows
```

The generated workflow is a reviewable skeleton. It contains explicit
parameters, capability comments, side-effect comments, approval boundaries, and
TODO comments for fuzzy segments that still require a model or reviewer.

```harn
pipeline version_bump(repo_path, version, branch_name, release_target) {
  let review_warnings = []
  // Step 1: tool_call git.checkout_branch
  // side_effect: git_ref release-branch
  log("crystallized step 1: git.checkout_branch")
  return {status: "shadow_ready", review_warnings: review_warnings}
}
```

## Report

The report includes:

- normalized workflow-candidate IR with parameters, constants,
  preconditions, side effects, capabilities, required secrets, approval points,
  expected outputs, deterministic segments, and fuzzy segments
- source trace hashes and example action ids for provenance
- confidence and rejection reasons
- shadow-mode pass/fail details for every source trace
- model calls avoided, token savings, estimated cost savings, wall-clock
  savings, CPU/runtime cost, and remaining model-call requirements
- promotion metadata: source trace hashes, author, approver, created_at,
  version, package name, capability set, required secrets, rollback target, and
  eval pack link

Candidates with divergent side effects stay in `rejected_candidates` and do not
produce a selected candidate.

## Shadow Mode

Shadow comparison does not call tools or mutate external systems. It compares
the selected sequence against each source trace:

- action signature and ordering
- deterministic output when a stable expected output exists
- requested side effects
- approval boundaries

This gives Harn Cloud and local reviewers a deterministic pass/fail surface
before promotion.

## Eval Pack

When `--eval-pack` is supplied, the CLI writes a minimal eval-pack v1 manifest
with a `crystallization-shadow` assertion. Hosted runners can attach the trace
fixtures and richer rubrics later; the local artifact records the candidate id,
source trace ids, and blocking shadow expectation.

## Portable Bundle

Pass `--bundle <DIR>` to also emit a portable crystallization-candidate
**bundle** that Harn Cloud (and any other downstream importer) can consume
without bespoke glue:

```text
bundle/
├── candidate.json        # versioned manifest (see below)
├── workflow.harn         # generated/reviewable workflow
├── report.json           # full mining/shadow/eval report
├── harn.eval.toml        # generated eval pack (when --eval-pack is set)
└── fixtures/             # redacted replay fixtures referenced by the report
    ├── trace_release_001.json
    └── ...
```

`candidate.json` carries the stable schema markers and metadata Harn Cloud
needs to import a candidate directly:

```json
{
  "schema": "harn.crystallization.candidate.bundle",
  "schema_version": 1,
  "generated_at": "2026-04-26T12:34:56Z",
  "generator": {"tool": "harn", "version": "0.7.43"},
  "kind": "candidate",
  "candidate_id": "candidate_4f5e...",
  "external_key": "version-bump",
  "title": "version_bump (3 steps)",
  "team": "platform",
  "repo": "burin-labs/harn",
  "risk_level": "medium",
  "workflow": {
    "path": "workflow.harn",
    "name": "version_bump",
    "package_name": "release-workflows",
    "package_version": "0.1.0"
  },
  "source_trace_hashes": ["sha256:..."],
  "source_traces": [
    {
      "trace_id": "trace_release_001",
      "source_hash": "sha256:...",
      "source_url": "/work/harn/runs/release-001.json",
      "source_receipt_id": null,
      "fixture_path": "fixtures/trace_release_001.json"
    }
  ],
  "deterministic_steps": [...],
  "fuzzy_steps": [...],
  "side_effects": [...],
  "capabilities": ["fs.write", "git.write"],
  "required_secrets": ["CRATES_IO_TOKEN"],
  "savings": {...},
  "shadow": {...},
  "eval_pack": {"path": "harn.eval.toml", "link": null},
  "fixtures": [
    {
      "path": "fixtures/trace_release_001.json",
      "trace_id": "trace_release_001",
      "source_hash": "sha256:...",
      "redacted": true
    }
  ],
  "promotion": {
    "owner": null,
    "approver": "lead@example.com",
    "author": "ops@example.com",
    "rollout_policy": "shadow_then_canary",
    "rollback_target": "keep source traces and previous package version",
    "created_at": "2026-04-26T12:34:56Z",
    "workflow_version": "0.1.0",
    "package_name": "release-workflows"
  },
  "redaction": {
    "applied": true,
    "rules": ["sensitive_keys", "secret_value_heuristic"],
    "summary": "fixture payloads scrubbed of secret-like values and sensitive keys before write",
    "fixture_count": 5
  },
  "confidence": 0.94,
  "rejection_reasons": [],
  "warnings": []
}
```

Importers MUST refuse bundles whose `schema` is not exactly
`harn.crystallization.candidate.bundle` or whose `schema_version` is greater
than the highest version they understand. Only the documented additive fields
may be added without bumping `schema_version`.

`kind` is one of:

- `candidate` — a normal candidate that passed shadow comparison.
- `plan_only` — every side effect stays inside Harn's own data plane (receipt
  writes, in-memory event-log appends, plan stashes). Cloud can promote these
  without explicit external-side-effect approval.
- `rejected` — no safe candidate was selected; the bundle still records what
  was attempted and why so reviewers can debug or feed it back into mining.

### Redaction

Bundles never ship raw private trace payloads. Before fixtures are copied into
`fixtures/`, the writer:

- replaces values for sensitive keys (anything containing `token`, `secret`,
  `password`, `api_key`, `apikey`, plus `authorization` and `cookie`) with
  `"[redacted]"`,
- redacts string values that look like raw API tokens
  (`sk-…`, `ghp_…`, `ghs_…`, `xoxb-…`, `xoxp-…`, `AKIA…`, or a long
  alphanumeric run that fits the credential heuristic).

`required_secrets` always lists logical ids (e.g. `CRATES_IO_TOKEN`), never
secret values.

### Validating a bundle

`harn crystallize validate <BUNDLE_DIR>` is a CLI smoke check that reads the
manifest, verifies the schema marker and version, confirms each referenced
file is present, and refuses bundles that include unredacted fixtures or
secret-shaped logical ids:

```bash
harn crystallize validate bundles/version-bump
# Bundle: bundles/version-bump (schema=harn.crystallization.candidate.bundle ...)
# Checks: manifest=ok workflow=ok report=ok eval_pack=ok fixtures=ok redaction=ok
# OK
```

### Shadow replay from a bundle

`harn crystallize shadow <BUNDLE_DIR>` re-runs the deterministic shadow
comparison in-process against the bundle's redacted fixtures, with no live
side effects. The exit code is non-zero if the replay diverges from the
recorded shadow report — useful in CI to prove the bundle stays
self-consistent across Harn upgrades.

```bash
harn crystallize shadow bundles/version-bump
# Shadow replay: bundle=bundles/version-bump candidate_id=candidate_... compared=5 pass=true
```

## Release-harness steel thread

The `crystallize ingest` subcommand is the consumer half of the
`release_harn.harn` ↔ Harn steel thread tracked in
[harn-bump-fleet#2](https://github.com/burin-labs/harn-bump-fleet/issues/2)
(producer) and [harn#1146](https://github.com/burin-labs/harn/issues/1146)
(this consumer). It turns a single
`release_harn.crystallization_input.v1` fixture bundle into a reviewed
crystallization candidate without going through repeated-sequence
mining: the trace IS the workflow.

The fixture layout the importer consumes is exactly what
`release_harn.harn` writes at
`${RUN_ROOT}/<run-id>/crystallization-input/`:

```text
crystallization-input/
  manifest.json                # release identity + file map
  release-run.json             # full release-harness payload
  deterministic-events.jsonl   # release facts, findings, step records
  agent-events.jsonl           # model audit + recovery advice
  tool-observations.jsonl      # shell/read observations
  README.md                    # human-readable description
```

A small checked-in sample lives at
[`crates/harn-vm/tests/fixtures/release_harn_sample/`](https://github.com/burin-labs/harn/tree/main/crates/harn-vm/tests/fixtures/release_harn_sample)
so the importer can be exercised without a live release run.

```bash
harn crystallize ingest \
  --from crates/harn-vm/tests/fixtures/release_harn_sample \
  --bundle bundles/release-harn-sample \
  --shadow
# Ingest: from=… run_id=… version=0.7.52->0.7.53 candidate=candidate_…
# Bundle: bundles/release-harn-sample (kind=Candidate schema_version=1 fixtures=1)
# Segments: deterministic=7 agentic=4 (review-required: 4)
# Recovery: shell_failures=2 recovery_runs=1 fed_into_agent=true
# Shadow replay: candidate_id=candidate_… compared=1 pass=true
```

The emitted bundle uses the same `harn.crystallization.candidate.bundle`
schema as `harn crystallize`, so `harn crystallize validate <BUNDLE_DIR>`
and `harn crystallize shadow <BUNDLE_DIR>` work unchanged.

### Deterministic vs. agentic split

`report.json` for an ingested release-fixture bundle includes a
`segment_summary` block that describes the deterministic/agentic split
in plain English. It groups events into:

- **safe to automate** — deterministic harness events (release analysis,
  changelog inputs, successful release steps).
- **requires human review** — agent-authored review attempts, agent
  recovery advice, deterministic findings, and any failed deterministic
  steps that need recovery before re-run.

Every agent step is materialized as a candidate step with an explicit
approval boundary so hosts cannot promote the candidate to fully
autonomous execution without resolving the review-required entries
first.

### Recovery feedback summary

`report.json` also includes a `recovery_summary` block that records:

- how many shell/tool failures were observed in the source trace,
- how many `agent_loop` recovery-advice runs were invoked,
- whether the failure context was fed back into a model loop, and
- which deterministic step names failed.

This makes it obvious at a glance whether recovery was advisory only
(human-resolved) or whether the workflow attempted automated repair.
The Harn implementation always treats recovery advice as advisory: the
candidate steps generated for `agent_recovery_advice` events carry an
`agent_recovery_advice` review note and a `recovery_review` approval
boundary so hosts must not re-run a failing step without a human
acknowledging the advice.

## Persona-aware crystallization

Generic crystallization mines repeated traces. Persona-aware crystallization is
the narrower loop for durable personas that repeatedly call a repair-worker for
the same shape of problem. The stdlib helper
`persona_crystallization_bundle(history, options?)` lives in
`std/personas/prelude` and keeps recurrence selection in Harn orchestration:
Rust provides receipt/run history, bundle validation, diff, and replay
primitives, while Harn persona code decides whether a recurring worker result
should become a deterministic `@step`.

The helper is deliberately conservative. It proposes only exact recurrence:

- same persona and repair worker
- same repair-worker input shape
- same repair-worker output shape
- same downstream action
- at least `min_examples` successful records
- at least `min_hosted_history_days` of hosted history, defaulting to 90 days

When the gate passes, the returned proposal uses the existing
`harn.crystallization.candidate.bundle` shape. It includes source trace
references, a deterministic shadow comparison over the matched records, savings
from avoided repair-worker model calls, and a literal Harn `@step` patch. The
patch is review material, not an automatic mutation: promotion still runs
historical shadow fixtures, eval-pack delta, human approval, a package version
bump, and a normal PR against the persona package.

```harn
import { persona_crystallization_bundle } from "std/personas/prelude"

pipeline propose_persona_step(repair_runs) {
  return persona_crystallization_bundle(
    repair_runs,
    {
      persona: "merge_captain",
      hosted_history_days: 91,
      min_examples: 3,
      package_name: "merge-captain-workflows",
      approver: "release-lead@example.com",
    },
  )
}
```

### Out of scope here

This subcommand is intentionally a one-shot ingest path. It does not:

- emit additional release-specific telemetry into `release_harn.harn`
  (that lives in
  [harn-bump-fleet#2](https://github.com/burin-labs/harn-bump-fleet/issues/2)),
- introduce a new workflow loader or Burin UI mechanism (Burin local
  loading lives in
  [burin-code#516](https://github.com/burin-labs/burin-code/issues/516)),
- or host a tenant candidate inbox (that lives in
  [harn-cloud#145](https://github.com/burin-labs/harn-cloud/issues/145)).
