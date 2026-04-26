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
