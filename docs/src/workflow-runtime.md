# Workflow runtime

Harn's workflow runtime is the layer above raw `llm_call()` and
`agent_loop()`. It gives host applications a typed, inspectable, replayable
orchestration boundary instead of pushing orchestration logic into app code.

## Core concepts

### Workflow graphs

Use `workflow_graph(...)` to normalize a workflow definition into a typed
graph with:

- named nodes
- explicit edges
- node kinds such as stage, verify, join, condition, fork, map, reduce, subagent, and escalation
- typed stage input/output contracts
- explicit branch semantics and typed run transitions
- per-node model, transcript, context, retry, and capability policies
- workflow-level capability ceiling
- mutation audit log entries

`subagent` nodes are now a real delegated execution boundary. They run through
the worker lifecycle, attach worker metadata to their stage records, and tag
their produced artifacts with delegated provenance so parent workflows can
inspect and reduce child results explicitly.

Start with a helper that registers the tools the workflow will expose to
each node. Each tool carries its own capability policy so validation can
enforce them automatically:

```harn
fn review_tools() {
  var tools = tool_registry()
  tools = tool_define(tools, "read", "Read a file", {
    parameters: {path: {type: "string"}},
    returns: {type: "string"},
    handler: nil,
    policy: {
      capabilities: {workspace: ["read_text"]},
      side_effect_level: "read_only",
      path_params: ["path"],
      mutation_classification: "read_only"
    }
  })
  tools = tool_define(tools, "edit", "Edit a file", {
    parameters: {path: {type: "string"}},
    returns: {type: "string"},
    handler: nil,
    policy: {
      capabilities: {workspace: ["write_text"]},
      side_effect_level: "workspace_write",
      path_params: ["path"],
      mutation_classification: "apply_workspace"
    }
  })
  tools = tool_define(tools, "run", "Run a command", {
    parameters: {command: {type: "string"}},
    returns: {type: "string"},
    handler: nil,
    policy: {
      capabilities: {process: ["exec"]},
      side_effect_level: "process_exec",
      mutation_classification: "ambient_side_effect"
    }
  })
  return tools
}

let graph = workflow_graph({
  name: "repair_loop",
  entry: "act",
  nodes: {
    act: {kind: "stage", mode: "agent", tools: review_tools()},
    verify: {kind: "verify", mode: "agent", tools: tool_select(review_tools(), ["run"])},
    repair: {kind: "stage", mode: "agent", tools: tool_select(review_tools(), ["edit", "run"])}
  },
  edges: [
    {from: "act", to: "verify"},
    {from: "verify", to: "repair", branch: "failed"},
    {from: "repair", to: "verify", branch: "retry"}
  ]
})

let report = workflow_validate(graph)
assert(report.valid)
```

When tool entries include `policy`, Harn folds that metadata into workflow
validation and execution automatically. That keeps the registry itself as the
source of truth for capability requirements instead of forcing products to
repeat the same information in both tool definitions and node policy blocks.

### Action graphs

`std/agents` now exposes an action-graph layer above raw workflow graphs for
planner-driven orchestration:

- `action_graph(raw, options?)` canonicalizes planner output variants into a
  stable `{_type: "action_graph", actions: [...]}` envelope.
- `action_graph_batches(graph, completed?)` repairs missing cross-phase
  dependencies and groups ready work by phase plus tool class.
- `action_graph_flow(graph, config?)` turns that plan envelope into a typed
  workflow graph with one scheduled batch stage per ready batch.
- `action_graph_run(task, graph, config?, overrides?)` attaches a durable
  `plan` artifact and executes the generated workflow via `workflow_execute`.

This is the intended shared substrate for "research -> plan -> execute ->
verify" style pipelines when the planner output is unstable but the executor
should still see a canonical schedule.

```harn
import "std/agents"

let raw_plan = {
  steps: [
    {id: "inspect", kind: "research", title: "Inspect parser", tools: ["read", "search"]},
    {id: "patch", title: "Patch diagnostics", tools: ["edit"]},
    {id: "docs", title: "Update release notes", tools: ["edit"]}
  ]
}

let plan = action_graph(raw_plan, {task: "Fix parser diagnostics"})
let run = action_graph_run("Fix parser diagnostics", plan, {
  research: {mode: "llm", model_policy: {provider: "mock"}},
  execute: {mode: "llm", model_policy: {provider: "mock"}},
  verify: {command: "cargo test --workspace --quiet", expect_status: 0}
})

println(run.status)
println(len(run.batches))
```

### Artifacts and resources

Artifacts are the real context boundary. Instead of building context mostly
by concatenating strings, Harn selects typed artifacts under policy and
budget.

Core artifact kinds that ship in the runtime include:

- `artifact`
- `resource`
- `summary`
- `analysis_note`
- `diff`
- `test_result`
- `verification_result`
- `plan`

Artifacts carry provenance fields such as:

- `source`
- `created_at`
- `freshness`
- `lineage`
- `relevance`
- `estimated_tokens`
- `metadata`

Example:

```harn
let selection = artifact({
  kind: "resource",
  title: "Selected code",
  text: read_file("src/parser.rs"),
  source: "workspace",
  relevance: 0.95
})

let plan = artifact_derive(selection, "plan", {
  text: "Update the parser diagnostic wording and preserve spans."
})

let context = artifact_context([selection, plan], {
  include_kinds: ["resource", "plan"],
  max_tokens: 1200
})
```

## Executing workflows

`workflow_execute(task, graph, artifacts?, options?)` executes a typed
workflow and persists a structured run record.

```harn
let run = workflow_execute(
  "Fix the diagnostic regression and verify the tests.",
  graph,
  [selection, plan],
  {max_steps: 8}
)

println(run.status)
println(run.path)
println(run.run.stages)
```

`verify` nodes can also run deterministic checks without an LLM loop:

```harn,ignore
verify: {
  kind: "verify",
  verify: {
    command: "cargo test --workspace --quiet",
    expect_status: 0,
    assert_text: "test result: ok"
  }
}
```

Command-based verification records `stdout`, `stderr`, `exit_status`, and a
derived success flag on the stage result while still flowing through the same
workflow branch/outcome machinery as LLM-backed verification.

Verifier requirements can also be published as structured contract inputs for
earlier planning and execution stages. Harn injects these contracts into the
stage prompt automatically so the model sees exact verifier-owned identifiers,
paths, and wiring text before it starts editing:

```harn,ignore
verify: {
  kind: "verify",
  verify: {
    command: "python scripts/verify_rate_limit.py",
    expect_status: 0,
    required_identifiers: ["rateLimit"],
    required_paths: ["src/middleware/rateLimit.ts"],
    required_text: ["app.use(rateLimit)"],
    notes: ["Use the verifier-exact symbol names. Do not rename them."]
  }
}
```

When the verifier contract lives outside the workflow file, point `contract_path`
at a JSON file relative to the workflow execution context:

```harn,ignore
verify: {
  kind: "verify",
  verify: {
    command: "python scripts/verify_rate_limit.py",
    contract_path: "scripts/verify_rate_limit.contract.json",
    expect_status: 0
  }
}
```

Options currently include:

- `max_steps`
- `persist_path`
- `resume_path`
- `resume_run`
- `replay_path`
- `replay_run`
- `replay_mode: "deterministic"`
- `audit`
- `mutation_scope`
- `approval_policy`

Resuming is practical rather than magical: if a saved run has unfinished
successor stages, Harn continues from persisted ready-node checkpoints with
saved artifacts, transcript state, and traversed run-graph edges.

Deterministic replay is now a runtime mode rather than a CLI-only inspection
tool: passing a prior run via `replay_run` or `replay_path` replays saved stage
records and artifacts through the workflow engine without calling providers or
tools again.

Delegated runs surface child worker lineage in each delegated stage's metadata.
This makes replay/eval and host timelines able to distinguish parent execution
from child execution without reconstructing that structure from plain text.
Persisted runs also retain explicit `parent_run_id`, `root_run_id`, and
`child_runs` lineage, and `load_run_tree(path)` materializes that hierarchy
recursively for inspection or host-side task views.

Map nodes can now execute branch work in parallel. `node.join_policy.strategy`
accepts:

- `"all"` to wait for every branch result
- `"first"` to return after the first completed branch
- `"quorum"` to return after `join_policy.min_completed` branches finish

`node.map_policy.max_concurrent` limits branch fan-out, and partial failures are
retained alongside successful branch artifacts instead of aborting the whole map
stage on the first error.

Runs may also include `metadata.mutation_session`, a normalized audit record
used to tie tool gates, workers, and artifacts back to one mutation boundary:

- `session_id`
- `parent_session_id`
- `run_id`
- `worker_id`
- `execution_kind`
- `mutation_scope`
- `approval_policy`

This is not an editor undo stack. It is the runtime-side provenance contract
that hosts can map onto their own approval and undo/redo UX.

## Transcripts and sessions

Stage transcripts are owned by the [session store](./sessions.md), not by
a per-node `transcript_policy` dict. Each node picks up a session id from
`model_policy.session_id`; two nodes that share an id share their
conversation automatically. Unset ids get a stable stage-scoped default.

To shape transcript behavior on a node, use the dedicated workflow
setters plus the lifecycle builtins:

- `workflow_set_auto_compact(graph, node_id, policy)` — sets
  `auto_compact`, `compact_threshold`, `tool_output_max_chars`,
  `compact_strategy`, `hard_limit_tokens`, `hard_limit_strategy`.
- `workflow_set_output_visibility(graph, node_id, visibility)` —
  `"public" | "private" | nil`.
- `agent_session_reset(id)`, `agent_session_fork(src, dst?)`,
  `agent_session_fork_at(src, keep_first, dst?)`,
  `agent_session_trim(id, keep_last)`, `agent_session_compact(id, opts)`
  — call these in the pipeline before `workflow_execute` to branch,
  reset, or compact a stage's conversation explicitly.

The old `transcript_policy` dict (with `mode: "continue" | "reset" |
"fork"`) was removed in 0.7.0; see [Sessions](./sessions.md) for
migration.

## Meta-orchestration builtins

Harn exposes typed workflow editing builtins so orchestration changes can be
audited and validated against the workflow IR:

- `workflow_inspect(..., ceiling?)`
- `workflow_clone(...)`
- `workflow_insert_node(...)`
- `workflow_replace_node(...)`
- `workflow_rewire(...)`
- `workflow_set_model_policy(...)`
- `workflow_set_context_policy(...)`
- `workflow_set_auto_compact(...)`
- `workflow_set_output_visibility(...)`
- `workflow_diff(...)`
- `workflow_validate(..., ceiling?)`
- `workflow_policy_report(..., ceiling?)`
- `workflow_commit(...)`

These mutate structured workflow graphs, not free-form prompt text.

## Capability ceilings

Workflows and sub-orchestration may narrow capabilities, but they must not
exceed the host/runtime ceiling.

This is enforced explicitly by capability-policy intersection during
validation and execution setup. If a node requests tools or host operations
outside the ceiling, validation fails.

## Run records, replay, and evals

Workflow execution produces a persisted run record containing:

- workflow identity
- task
- stage records
- stage attempts, outcomes, and branch decisions
- traversed graph transitions
- ready-node checkpoints for resume
- stage transcripts
- visible output
- private reasoning metadata
- tool intent and tool execution events
- provider payload metadata kept separate from visible text
- verification outcomes
- artifacts
- policy metadata
- parent/root run lineage and delegated child runs
- a derived observability block summarizing planner rounds, research facts,
  action-graph nodes/edges, verification outcomes, and transcript pointers
- execution status

CLI support:

```bash
harn portal
harn runs inspect .harn-runs/<run>.json
harn runs inspect .harn-runs/<run>.json --compare baseline.json
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
harn eval .harn-runs/
harn eval evals/regression.json
```

The replay/eval surface is intentionally tied to saved typed run records so
host applications do not need to build their own provenance layer.

For a local visual view over the same persisted data, `harn portal` reads the
run directory directly and renders stages, the derived action graph, trace
spans, transcript sections, and delegated child runs without introducing a
second storage format.

For host/runtime consumers that want the same logic inside Harn code, the VM
also exposes:

- `run_record_fixture(...)`
- `run_record_eval(...)`
- `run_record_eval_suite(...)`
- `run_record_diff(...)`
- `eval_suite_manifest(...)`
- `eval_suite_run(...)`

Eval manifests group persisted runs, optional explicit replay fixtures, and
optional baseline run comparisons under a single typed document. This lets
hosts treat replay/eval suites as data rather than external scripts.

## Host artifact handoff

Hosts and editor bridges should hand Harn typed artifacts instead of embedding
their own orchestration rules in ad hoc prompt strings. The VM now exposes
helpers for the most common host surfaces:

- `artifact_workspace_file(...)`
- `artifact_workspace_snapshot(...)`
- `artifact_editor_selection(...)`
- `artifact_verification_result(...)`
- `artifact_test_result(...)`
- `artifact_command_result(...)`
- `artifact_diff(...)`
- `artifact_git_diff(...)`
- `artifact_diff_review(...)`
- `artifact_review_decision(...)`
- `artifact_patch_proposal(...)`
- `artifact_verification_bundle(...)`
- `artifact_apply_intent(...)`

These helpers normalize kind names, token estimates, priority defaults,
lineage, and metadata so host products can pass editor/test/diff state into
Harn without recreating artifact taxonomy and provenance logic externally.
