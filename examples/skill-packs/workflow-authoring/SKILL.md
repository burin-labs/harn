---
name: workflow-authoring
short: Author Harn portable workflow bundles for day-to-day engineering automations.
description: |
  Generate, validate, preview, and run portable Harn workflow bundle JSON for
  monitoring and repairing pull requests, deploys, logs, and other event-driven
  engineering work. Optimized for smaller models (qwen, gemma, llama.cpp) with
  explicit XML sections, strict JSON output, and a validation-and-retry loop.
when-to-use: |
  Use when the user asks for a durable engineering automation expressed as a
  Harn workflow bundle: monitor this PR, wait for deploy then query logs,
  repair failing checks, schedule a recurring health probe, or import a
  workflow into Burin / a host. Pair with the workflow CLI: `harn workflow
  validate|preview|run --bundle <path> --json`.
allowed_tools:
  - "Bash(harn workflow validate:*)"
  - "Bash(harn workflow preview:*)"
  - "Bash(harn workflow run:*)"
  - "Bash(harn workflow patch:*)"
  - "Bash(harn workflow function-tools:*)"
  - "Bash(harn workflow nested-ceiling:*)"
  - "Bash(harn check:*)"
  - "Read"
  - "Write"
  - "Edit"
---

# Workflow authoring (Harn workflow bundles)

You are authoring a **portable workflow bundle** — a JSON file Harn validates,
previews, and runs locally. Hosts (Burin TUI/GUI, Harn Cloud importers) consume
the same bundle without rewriting.

Read **`prompting.md`** in this directory before composing a response. It
defines the response sections, JSON envelope, and small-model rules. Every
recipe under `recipes/` is a worked, validated example you can crib from.

## What good output looks like

1. A single `<bundle>` block containing valid JSON for a `WorkflowBundle`.
2. A short `<rationale>` block (≤ 5 bullets) explaining the trigger, the
   waitpoints, and the connector requirements.
3. A `<verify>` block listing the exact commands the user should run:

   ```bash
   harn workflow validate --bundle out.bundle.json --json
   harn workflow preview  --bundle out.bundle.json --json
   harn workflow run      --bundle out.bundle.json --json
   ```

If validation fails, **read the diagnostic, edit the bundle, and re-validate**.
Do not invent fields or kinds — the validator's enum lists are the source of
truth.

## Bundle skeleton

```json
{
  "schema_version": 2,
  "entrypoint": "workflows/<kebab-case-id>.harn",
  "transitive_modules": [
    {
      "path": "workflows/<kebab-case-id>.harn",
      "source_hash_blake3": "blake3:<64 lowercase hex digits>",
      "harnbc_hash_blake3": "blake3:<64 lowercase hex digits>"
    }
  ],
  "stdlib_version": "<harn stdlib version>",
  "harn_version": "<harn version>",
  "provider_catalog_hash": "blake3:<64 lowercase hex digits>",
  "tool_manifest": [],
  "sbom": {
    "format": "spdx-lite",
    "version": "2.3",
    "packages": [],
    "relationships": []
  },
  "signature": null,
  "parent_trust_record_id": null,
  "id": "<kebab-case-id>",
  "name": "<human label>",
  "version": "1.0.0",
  "triggers": [
    { "id": "<trigger-id>", "kind": "github|cron|delay|webhook|mcp|manual",
      "provider": "github", "events": ["pull_request.opened"],
      "node_id": "<entry-node-id>" }
  ],
  "workflow": {
    "_type": "workflow_graph",
    "id": "<graph-id>",
    "version": 1,
    "entry": "<entry-node-id>",
    "nodes": {
      "<node-id>": { "id": "<node-id>", "kind": "action|waitpoint|notification",
                     "task_label": "<short label>" }
    },
    "edges": [ { "from": "<from-node-id>", "to": "<to-node-id>" } ]
  },
  "prompt_capsules": {
    "<capsule-id>": { "id": "<capsule-id>", "node_id": "<node-id>",
                      "trigger_id": "<trigger-id>",
                      "prompt": "<self-contained continuation prompt>" }
  },
  "policy": {
    "autonomy_tier": "shadow|suggest|act_with_approval|act_auto",
    "retry":   { "max_attempts": 2, "backoff": "exponential" },
    "catchup": { "mode": "none|latest|all", "max_events": 1 }
  },
  "connectors": [
    { "id": "github", "provider_id": "github",
      "scopes": ["pull_requests:read", "checks:read"],
      "setup_required": true, "status_required": true }
  ],
  "environment": {
    "repo_setup_profile": "default",
    "worktree_policy": "reuse_current|new_worktree|host_managed",
    "command_gates": ["make test"]
  }
}
```

## Hard rules (validator enforces)

- `schema_version` must be `2`.
- `entrypoint`, `transitive_modules`, `stdlib_version`, `harn_version`,
  `provider_catalog_hash`, `tool_manifest`, `sbom`, `signature`, and
  `parent_trust_record_id` are part of the canonical `.harnpack` manifest.
- BLAKE3 fields use `blake3:<64 lowercase hex digits>`.
- `id`, `version`, `workflow.id`, `workflow.entry` are non-empty.
- Every `workflow.nodes[k].id` matches its map key `k`.
- Every edge endpoint and `trigger.node_id` references an existing node.
- `policy.autonomy_tier` ∈ `{shadow, suggest, act_with_approval, act_auto}`.
- `policy.catchup.mode` ∈ `{none, latest, all}`.
- `policy.retry.max_attempts >= 1`.
- `environment.worktree_policy` ∈ `{reuse_current, new_worktree, host_managed}`.
- Trigger kind requirements:
  - `github` → `provider: "github"` and at least one `events[]`.
  - `cron` → `schedule`.
  - `delay` → `delay` (ISO-8601 duration, e.g. `PT10M`).
  - `webhook` → `webhook_path`.
  - `mcp` → `mcp_tool`.
  - `manual` → no extra fields.
- Every `prompt_capsules[k].id` equals `k` and points at a real node; at most
  one capsule per node.
- Every connector that a trigger references via `provider` should appear in
  `connectors[]` (warning otherwise).

## Node kinds (host vocabulary)

| Kind | Use it for | Notes |
|---|---|---|
| `action` | Concrete work (normalize event, query API, run a command). | Default. |
| `waitpoint` | Pause until an event/delay/approval resumes the workflow. | Pair with a `delay` or matching trigger. |
| `notification` | Send a user-facing message. | Surface via host UX. |
| `agent` / `subagent` | Agent-loop stages and delegated workers. | Map to `kind: "agent" \| "subagent"`. |
| `approval` | HITL gate. | Validator categorizes `approval`/`hitl`. |
| `connector_call` | Outbound connector method. | Validator categorizes `connector` / `connector_call`. |
| `terminal` | Explicit success/failure sink. | Optional. |

Anything else is treated as a free-form node kind and rendered as-is — prefer
the canonical list above.

## Recipes

| Recipe | What it shows |
|---|---|
| `recipes/pr-monitor/bundle.json` | GitHub PR open/sync trigger → wait for deploy → log query → notify, with retry+catchup policy and `act_with_approval`. |
| `recipes/pr-repair/bundle.json` | Periodic + GitHub-failure dual trigger → worktree setup → repair agent stage → approval gate → notify, with autonomous repair policy. |

Run any recipe end-to-end:

```bash
harn workflow validate --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --json
harn workflow preview  --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --mermaid
harn workflow run      --bundle examples/skill-packs/workflow-authoring/recipes/pr-monitor/bundle.json --json
```

## Eval harness

`eval.harn` is the local-model authoring eval. Point it at any case in
`cases/` and any provider/model:

```bash
harn run examples/skill-packs/workflow-authoring/eval.harn -- \
  --case cases/pr-monitor.case.json \
  --provider ollama --model qwen3:4b
```

The script feeds the case prompts to the model, extracts the `<bundle>` block,
runs the validate → preview → run pipeline, and prints a JSON report with
`{validation_passed, preview_passed, run_passed, structural_assertions, ...}`.

`cases/*.golden_bundle` is the canonical expected bundle. The Rust regression
gate (`crates/harn-cli/tests/workflow_authoring_eval.rs`) loads every case +
recipe, asserts the goldens validate / preview / run, and asserts the
structural assertions hold. Add a case → CI catches drift.

## Workflow patch authoring

When the user wants to **modify** an existing bundle (insert a verifier
between two stages, add an approval gate, narrow a node's tool policy),
emit a **workflow patch** instead of rewriting the bundle. Patches are
auditable JSON: Harn applies them to a copy of the bundle, runs the
validator, computes a structural diff, and rejects anything that widens
the parent capability ceiling.

```bash
harn workflow patch validate \
    --bundle pr-monitor.bundle.json \
    --patch  pr-verifier.patch.json \
    --parent-ceiling /path/to/parent.json --json
harn workflow patch apply --bundle ... --patch ... --out ... --json
harn workflow patch preview --bundle ... --patch ... --mermaid
```

The patch JSON is a flat list of operations:

| `op` | Purpose | Required fields |
|---|---|---|
| `insert_node` | Add a workflow node (agent, verifier, approval, notification). | `node_id`, optional `node` body |
| `add_edge` | Connect two existing nodes. | `from`, `to`, optional `branch`, `label` |
| `upsert_prompt_capsule` | Insert or replace a prompt capsule for a node. | `capsule_id`, `capsule.{node_id, prompt}` |
| `update_node_policy` | Patch a node's `task_label` / `prompt` / `system` / `tools` / `model_policy` / `capability_policy` / `approval_policy`. | `node_id`, `policy.<field>` |
| `update_bundle_policy` | Patch bundle-level `autonomy_tier` / `tool_policy` / `approval_required` / `retry` / `catchup`. | `policy.<field>` |

**Hard rules — the validator enforces them:**

- The patch must declare `schema_version: 1` and a non-empty `id`.
- An empty `operations` list is rejected (patches must do something).
- `insert_node` fails on a duplicate id; `add_edge` fails on a duplicate
  edge or unknown endpoint; `upsert_prompt_capsule` fails when another
  capsule already targets the same node.
- A patched bundle that widens any of `tools`, `capabilities`,
  `side_effect_level`, `workspace_roots`, connector scopes, command
  gates, or `autonomy_tier` against `--parent-ceiling` is rejected with
  per-dimension `widening` violations. **Patches never expand
  permissions.**

Worked example: `recipes/pr-monitor-verifier-patch/patch.json` inserts
a deterministic verifier and a repair branch into the PR-monitor recipe
and validates against the canonical fixture.

## Safe Harn function tools

`harn workflow function-tools --json` lists the allowlisted Harn
functions an agent may call from inside the patch-authoring loop. Every
descriptor is read-only or pure-think with an ACP-aligned
`ToolAnnotations` block: hosts can wire them straight into a model's
tool surface without auditing each one separately. The current
allowlist:

- `workflow_bundle_validate` — validate a bundle JSON file at a path.
- `workflow_bundle_preview` — normalized graph + mermaid + editable fields.
- `workflow_bundle_capability_ceiling` — capability ceiling the bundle would request.
- `workflow_patch_validate` — apply + validate a patch in memory and return the report.

Adding a function to this list is a deliberate, reviewed change in
`crates/harn-vm/src/orchestration/safe_function_tools.rs`. The default
posture is "not exposed."

## Nested invocation ceiling

When a Harn script under an active execution policy launches another
Harn invocation (`harn run`, `harn workflow run`, `harn supervisor
fire/replay`, a Burin harness), the parent must scan the target and
reject anything that asks for more than its own ceiling.
`harn workflow nested-ceiling --bundle <path> --parent <policy>` exposes
the same scanner used internally so hosts can sanity-check before
launching.

## Pointers

- Bundle contract: [`docs/src/workflow-bundles.md`](../../../docs/src/workflow-bundles.md)
- Trigger / connector providers: [`docs/llm/harn-triggers-quickref.md`](../../../docs/llm/harn-triggers-quickref.md)
- Supervisor commands: [`docs/src/workflow-supervisor.md`](../../../docs/src/workflow-supervisor.md)
- Validator source of truth: [`crates/harn-vm/src/orchestration/workflow_bundle.rs`](../../../crates/harn-vm/src/orchestration/workflow_bundle.rs)
- Patch contract source: [`crates/harn-vm/src/orchestration/workflow_patch.rs`](../../../crates/harn-vm/src/orchestration/workflow_patch.rs)
- Safe function-tool allowlist: [`crates/harn-vm/src/orchestration/safe_function_tools.rs`](../../../crates/harn-vm/src/orchestration/safe_function_tools.rs)
- Nested invocation guard: [`crates/harn-vm/src/orchestration/nested_invocation.rs`](../../../crates/harn-vm/src/orchestration/nested_invocation.rs)
