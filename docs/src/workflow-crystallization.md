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
