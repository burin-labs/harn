# Typed task-plan IR (experimental)

> Status: experimental, behind no flag — the module is `std/agent/task_plan`.
> See [burin-labs/harn#2196][issue] for the recommendation that keeps this
> in experimental tier and the eval methodology used to promote it.

The typed task-plan IR is a narrow, JSON-shaped graph that a planner —
human, model, or both — can author without depending on the lower-level
[`workflow_graph`](./workflow-runtime.md) internals. It compiles into a
plain `WorkflowGraph` and runs on the existing workflow runtime; there is
no parallel execution engine.

## Why a separate IR

[`WorkflowGraph`](./workflow-runtime.md) is the executable substrate: it
expresses every concept the runtime supports (capability policies, model
policies, branch semantics, map/reduce, contracts). That power is the
right surface for stage authors but wrong for planners — model-generated
plans repeatedly invent fields, mis-route branches, and bypass execution
guarantees because the surface is too large.

The IR is a tight subset. Planners pick from eleven kinds (`read_fact`,
`search`, `context_pack`, `agent_loop`, `sub_agent`, `workflow_map`,
`verify`, `human_gate`, `deterministic_command`, `join`, `compact`),
declare edges, set top-level budgets and policies, and the compiler does
the rest. Validation rejects bad shape before any execution starts;
lowering produces a `WorkflowGraph` that `workflow_validate`,
`workflow_inspect`, and `workflow_execute` already understand.

## Shape

```json
{
  "schema_version": "1",
  "objective": "Add a token-bucket rate limiter around the auth middleware",
  "entry": "discover",
  "nodes": {
    "discover": {"kind": "read_fact", "prompt": "...", "tools": ["read", "grep"]},
    "implement": {
      "kind": "agent_loop",
      "prompt": "...",
      "tools": ["read", "edit", "run"],
      "effects": ["writes_files"],
      "agent_loop": {"max_iterations": 8, "done_sentinel": "DONE"}
    },
    "verify": {"kind": "verify", "verify": {"command": "cargo test -p auth", "expect_status": 0}}
  },
  "edges": [
    {"from": "discover", "to": "implement"},
    {"from": "implement", "to": "verify"}
  ],
  "capabilities": {"tools": ["read", "edit", "grep", "run"], "side_effect_level": "writes_files"},
  "verification": {"primary": "verify"},
  "unknowns": [{"id": "limiter_crate", "question": "...", "resolved_by_node": "discover"}],
  "compaction_policy": {"threshold_tokens": 8000, "preserve_recent": 8},
  "promotion_policy": {"shadow_runs_required": 3, "human_review_required": true}
}
```

Top-level fields:

| Field | Purpose |
|---|---|
| `schema_version` | Always `"1"` for now. The validator rejects unknown versions. |
| `objective` | Plain-text task description; surfaced in compiled `workflow.metadata.task_plan.objective`. |
| `entry` | Required entry node ID. |
| `nodes`, `edges` | Graph topology. Edge `branch` strings propagate to the lowered graph. |
| `budgets` | Static ceilings (`max_nodes`, `max_depth`, `max_tool_calls`, `max_model_calls`, `max_steps`). Enforced at validation time. |
| `capabilities` | Plan-wide capability ceiling. Lowered to `workflow.capability_policy`. |
| `verification` | Primary verifier node ID and required paths/text the verifier must touch. |
| `unknowns` | Questions the plan acknowledges as open, optionally pointing at the node expected to resolve them. |
| `transcript_policy`, `compaction_policy`, `promotion_policy` | Forward-declared policies retained on `workflow.metadata.task_plan` so receipt consumers and promotion gates can read them later. |

## Node kinds

Lowering rules from IR kind to `WorkflowGraph` node:

| IR kind | `WorkflowGraph.kind` | `mode` | Notes |
|---|---|---|---|
| `read_fact` | `stage` | `llm` | Single LLM turn, read-only — ideal for discovery / mapping. |
| `search` | `stage` | `llm` | Read-only search; behaviorally identical to `read_fact` but surfaces intent. |
| `context_pack` | `stage` | `llm` | Assembles a context pack for downstream stages. |
| `agent_loop` | `stage` | `agent` | Multi-iteration loop with `done_sentinel` from `agent_loop.done_sentinel`. |
| `sub_agent` | `subagent` | `llm` | `metadata.worker_name`, `worker_preset`, `worker_task_label` lifted from `sub_agent.*`. |
| `workflow_map` | `map` | `llm` | `map.items` / `map.item_artifact_kind` / `map.max_concurrency` lifted to `map_policy`. |
| `verify` | `verify` | `llm` | `verify` block (command, expect_status, expect_text, assert_text) passed through. |
| `human_gate` | `stage` | `manual` | `metadata.human_gate=true` + `approval_id` / `approval_prompt` / `approval_skippable`. |
| `deterministic_command` | `stage` | `command` | `metadata.command_tool` / `command_args` carry the deterministic invocation. |
| `join` | `join` | — | Sync barrier. |
| `compact` | `stage` | `compact` | `auto_compact.token_threshold` from `compact.threshold_tokens`; `metadata.compact_preserve_recent` for the count of recent turns to keep. |

When a planner needs to reach a `WorkflowGraph` field that the IR does
not expose, set `node.lowering_overrides` — its keys are merged into the
lowered node verbatim. Use it sparingly; if a field becomes common,
promote it into the IR.

## API

```harn
import { task_plan_compile, task_plan_render_mermaid, task_plan_schema, task_plan_validate } from "std/agent/task_plan"
```

* `task_plan_schema()` — `std/schema` dict; safe to feed to
  `schema_check` / `schema_parse` for fail-fast shape validation.
* `task_plan_validate(plan)` — never throws. Returns
  `{valid, errors, warnings, graph_stats, capability_summary, budget_summary, promotion_summary}`.
* `task_plan_compile(plan)` — runs validation, then lowers. Returns
  `{ok: true, workflow, validation}` on success and
  `{ok: false, validation}` when validation fails. The `workflow` dict is
  ready for `workflow_validate(workflow)`, `workflow_inspect(workflow)`,
  and `workflow_execute(task, workflow, artifacts, options)`.
* `task_plan_render_mermaid(plan)` — Mermaid `flowchart` for IR-level
  human review. Use [`workflow_bundle preview --mermaid`](./workflow-bundles.md)
  for the full execution-time graph.

Validator error codes worth knowing:

* `schema_version_mismatch` — wrong `schema_version`.
* `entry_missing`, `entry_not_found`, `edge_from_unknown`, `edge_to_unknown` — topology problems.
* `unknown_kind`, `agent_loop_missing_prompt`, `sub_agent_missing_worker`,
  `command_missing_tool`, `human_gate_missing_approval`,
  `map_missing_inputs` — kind-specific contract failures.
* `budget_max_nodes` — graph exceeds the declared (or default) node budget.
* `promotion_negative_shadow_runs`, `promotion_invalid_pass_rate` — bad
  promotion policy.
* `writes_without_capability` (warning) — plan declares write-capable
  kinds but `capabilities.tools` does not list `edit`/`run`.
* `node_unreachable` (warning) — node has no path from `entry`.

## Guardrails

The IR is intentionally a *compile-time* surface. It does **not**:

* Execute arbitrary generated Harn. Lowering targets the same
  `WorkflowGraph` runtime that operators already trust.
* Skip capability checks. `capabilities` becomes `workflow.capability_policy`
  and intersects with the host ceiling via `workflow_validate`.
* Replace `workflow_bundle.harn` for distribution. Bundles wrap a
  validated `WorkflowGraph` with signatures, SBOM, and replay metadata.
  An IR plan is the *input* to bundle authoring, not its competitor.

## Execution support today

The existing `workflow_execute` runtime treats `mode: "agent"` as the
trigger for the agent-loop run path; other modes flow through as plain
LLM stages. That covers the executable kinds (`read_fact`, `search`,
`context_pack`, `agent_loop`, `sub_agent`, `workflow_map`, `verify`,
`join`) end-to-end today.

`human_gate`, `deterministic_command`, and `compact` lower correctly and
preserve enough metadata (`metadata.human_gate`, `metadata.command_tool`,
`auto_compact.token_threshold`, etc.) for `workflow_inspect` and the
mermaid renderer to recover authorial intent. Running them with their
intended semantics — block on approval, skip the LLM, force compaction —
requires runtime hooks that are out of scope for the IR landing; until
those land, those three kinds execute as LLM stages with their metadata
intact, and the validator surfaces them on the lowered graph so a
follow-up executor can opt in. Plans that need execution today should
prefer the executable kinds; plans for review-only flows can use the
full vocabulary.

## Evaluation protocol (deferred)

[Issue #2196][issue] proposes a head-to-head comparison of three
strategies across the [five fixtures in `examples/task_plan`](../../examples/task_plan/README.md):

1. **Baseline** — a single tutorial-style `agent_loop` per task.
2. **Burin first-order** — Burin Code's existing
   plan→execute workflow.
3. **Typed task plan** — the same task lowered through this IR.

Metrics: pass/fail verifier, wall-clock latency, input/output tokens,
invalid or repeated tool calls, transcript drift after compaction,
deterministic-vs-model step count, and human-review usefulness of the
generated graph.

The IR side of that comparison is deterministic and shipped today
([`examples/task_plan/eval.harn`](../../examples/task_plan/eval.harn)
emits one JSONL record per plan covering validation, compile status, and
the lowered graph's own validate report). The two LLM-driven strategies
require a model budget that has not been allocated yet; the comparison
runs as a focused follow-up rather than blocking this experimental
landing.

## Promotion

Per [#2196][issue]'s acceptance criteria the IR ships as an experimental
stdlib module. Promotion to a Burin feature flag follows once the
three-strategy comparison shows the IR meaningfully reduces invalid tool
calls and transcript drift on at least three of the five fixtures
without a latency regression. Until then the module's `@api_stability`
tag stays at `experimental`.

[issue]: https://github.com/burin-labs/harn/issues/2196
