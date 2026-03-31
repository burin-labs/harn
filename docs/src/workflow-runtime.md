# Workflow Runtime

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

Validation is explicit:

```harn
let graph = workflow_graph({
  name: "repair_loop",
  entry: "act",
  nodes: {
    act: {kind: "stage", mode: "agent", tools: ["read_file", "edit", "run"]},
    verify: {kind: "verify", mode: "agent", tools: ["run"]},
    repair: {kind: "stage", mode: "agent", tools: ["edit", "run"]}
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

Options currently include:

- `max_steps`
- `persist_path`
- `resume_path`
- `resume_run`
- `replay_path`
- `replay_run`
- `replay_mode: "deterministic"`

Resuming is practical rather than magical: if a saved run has unfinished
successor stages, Harn continues from persisted ready-node checkpoints with
saved artifacts, transcript state, and traversed run-graph edges.

Deterministic replay is now a runtime mode rather than a CLI-only inspection
tool: passing a prior run via `replay_run` or `replay_path` replays saved stage
records and artifacts through the workflow engine without calling providers or
tools again.

## Transcript policy

Each node can attach transcript policy:

```harn
{
  mode: "continue",    // or "reset" / "fork"
  visibility: "public",
  compact: true,
  keep_last: 6
}
```

Harn applies transcript policy inside the runtime:

- reset or fork transcript state at stage boundaries
- compact transcripts before or after a stage
- redact public-only transcript views when requested

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
- `workflow_set_transcript_policy(...)`
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
- execution status

CLI support:

```bash
harn runs inspect .harn-runs/<run>.json
harn runs inspect .harn-runs/<run>.json --compare baseline.json
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
harn eval .harn-runs/
harn eval evals/regression.json
```

The replay/eval surface is intentionally tied to saved typed run records so
host applications do not need to build their own provenance layer.

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

These helpers normalize kind names, token estimates, priority defaults,
lineage, and metadata so host products can pass editor/test/diff state into
Harn without recreating artifact taxonomy and provenance logic externally.
