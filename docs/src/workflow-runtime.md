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
- node kinds such as stage, verify, join, condition, and map
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
- `resume_path`
- `resume_run`

Resuming is practical rather than magical: if a saved run has unfinished
successor stages, Harn can continue from the next node with persisted
artifacts and transcript state.

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

- `workflow_inspect(...)`
- `workflow_clone(...)`
- `workflow_insert_node(...)`
- `workflow_replace_node(...)`
- `workflow_rewire(...)`
- `workflow_set_model_policy(...)`
- `workflow_set_context_policy(...)`
- `workflow_set_transcript_policy(...)`
- `workflow_diff(...)`
- `workflow_validate(...)`
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
- stage transcripts
- visible output
- private reasoning metadata
- verification outcomes
- artifacts
- policy metadata
- execution status

CLI support:

```bash
harn runs inspect .harn-runs/<run>.json
harn replay .harn-runs/<run>.json
harn eval .harn-runs/<run>.json
```

The replay/eval surface is intentionally tied to saved typed run records so
host applications do not need to build their own provenance layer.
