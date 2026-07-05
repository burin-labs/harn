# Workflow runtime

Harn's workflow runtime is the layer above raw `llm_call()` and
`agent_loop()`. It gives host applications a typed, inspectable, replayable
orchestration boundary instead of pushing orchestration logic into app code.

> **Pipeline vs. workflow.** Two different things that are deliberately not
> renamed — learn the distinction once:
>
> - **`pipeline`** is the *language keyword*: a named, callable, function-like
>   composition (`pipeline name(args) { ... }`) that serves as a program
>   entrypoint and container. Not itself agentic. See
>   [Pipeline lifecycle](./pipeline-lifecycle.md).
> - **Workflow** is the *stage-graph runtime*: the typed, replayable graph of
>   stages (`stage`, `verify`, `join`, `condition`, `fork`, `map`, `reduce`,
>   `subagent`, `escalation`) executed by `workflow_execute` — this page.
>
> On the abstraction ladder: `llm_call` = one request < `agent_loop` = one
> goal < workflow = multiple goals, attempts, or models. (`agent_preset` is
> how you build `agent_loop` options, not a tier of its own.) See
> [Choosing an agent abstraction](./concepts/abstraction-ladder.md) and the
> [glossary](./concepts/glossary.md).

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
import { StageSpec } from "std/workflow/options"

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

// Build each node through the typed `StageSpec` alias (or the
// `workflow_stage_spec(...)` constructor) from `std/workflow/options`
// so stage-spec typos fail at check time.
let act: StageSpec = {kind: "stage", mode: "agent", tools: review_tools()}
let verify: StageSpec = {kind: "verify", mode: "agent", tools: tool_select(review_tools(), ["run"])}
let repair: StageSpec = {kind: "stage", mode: "agent", tools: tool_select(review_tools(), ["edit", "run"])}

let graph = workflow_graph({
  name: "repair_loop",
  entry: "act",
  nodes: {act: act, verify: verify, repair: repair},
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

log(run.status)
log(len(run.batches))
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

Build run options through the typed `WorkflowExecuteOptions` alias from
`std/workflow/options` (inline dict literals in the options slot are
flagged by the `unnormalized-options` lint):

```harn
import { WorkflowExecuteOptions } from "std/workflow/options"

let run_options: WorkflowExecuteOptions = {max_steps: 8}
let run = workflow_execute(
  "Fix the diagnostic regression and verify the tests.",
  graph,
  [selection, plan],
  run_options,
)

log(run.status)
log(run.path)
```

Use `harn runs view --json <path>` on `run.path` for the stable
`harn.run_view.v1` projection, including stage summaries.

Agent-backed stages pass `model_policy.iteration_budget` directly into the
per-stage `agent_loop`. Treat that structured budget as the source of truth for
adaptive or fixed loop limits; scripts no longer need to copy that cap into
`max_iterations`. `max_iterations` remains accepted as a scalar fixed cap, and
when both fields are present `iteration_budget.max` is the cap used by the agent
loop. Invalid budget fields fail at loop startup instead of being ignored.

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

A stage's `verify` may also be a **function** (fn-verify mode) when the check
is easier to express as Harn logic than as a command or an assertion dict:

```harn,ignore
goal: {
  kind: "subagent",
  retry_policy: {max_attempts: 3, feedback: true},
  verify: { result ->
    let text = to_string(result?.artifacts[0]?.text)
    return {ok: contains(text, "SUMMARY:"), findings: ["output is missing a SUMMARY: section"]}
  },
}
```

The verifier receives the settled attempt result and returns either a bool or a
verdict dict `{ok, findings?}` (`findings` may be a `list<string>` or a single
string). A failing fn-verify forces the retry-eligible `failed` branch and its
findings thread into the next attempt's repair prompt exactly like the
structured-check findings above. It applies on the same VM-executed stage paths
that honor `max_attempts` (subagent stages and deterministic execute stages) —
so a `subagent` stage can self-verify its own output and repair with feedback,
without a separate `verify` node. Because the verifier is a closure it runs in
Harn (`workflow_evaluate_verification`, `std/workflow/stage.harn`) and never
crosses into the host.

`node.retry_policy.max_attempts` uses total-attempt semantics for VM-executed
stage paths: command/compact/manual stages, subagent, fork/join, condition,
reduce, escalation, map branches, and deterministic command `verify` nodes.
Attempts stop on the first success and every attempt is recorded under the
stage's `attempts` array. Agent-backed stages still rely on their
`agent_loop`/LLM retry and iteration policies. Backoff fields are accepted in
the normalized policy shape but deterministic workflow execution does not sleep
between attempts yet; use host/orchestrator retry policy for scheduled delivery
retries and provider failover rather than relying on workflow backoff fields.

### Retry with feedback

A stage's `retry_policy` is the typed `WorkflowRetryPolicy`
(`std/workflow/options`): `{max_attempts?, feedback?, repair_prompt_builder?,
verify?, repair?, backoff_ms?, backoff_multiplier?}`. Build it through the
`StageSpec` alias so a typo fails at check time.

By default a retry re-issues the *unmodified* task on every attempt (a blind
retry — replayed runs are byte-identical). Two `retry_policy` keys turn the
retry into a repair loop that threads the prior attempt's verification findings
into the next attempt's task:

- `feedback: true` appends a bounded default template to the retry task —
  `Previous attempt N failed: <findings>`, where the findings are the failed
  verification checks (or the prior attempt's error/output when there are no
  structured checks). `feedback: {max_chars: N}` bounds the injected findings
  (default ~2000 characters).
- `repair_prompt_builder` is a closure that receives the full retry context and
  returns the complete replacement task. Its return value becomes the next
  attempt's task verbatim (it takes precedence over `feedback`). The context
  dict has exactly these keys:

  ```harn,ignore
  {
    task,          // the original (base) task string
    attempt,       // the just-failed attempt number (its return runs as attempt N+1)
    findings,      // list<string> of failed verification checks
    verification,  // the prior attempt's verification dict
    error,         // the prior attempt's error message, if any
    prior_text,    // the prior attempt's visible text
    stage,         // the stage node
  }
  ```

  ```harn,ignore
  verify_stage: {
    kind: "subagent",
    retry_policy: {
      max_attempts: 3,
      repair_prompt_builder: { ctx ->
        return ctx.task + "\n\nFix these findings from attempt " +
          to_string(ctx.attempt) + ":\n" + join(ctx.findings, "\n")
      },
    },
  }
  ```

Retry-with-feedback applies to the VM-executed stage paths that consume the
task (subagent stages, and deterministic execute stages) — the same paths that
honor `max_attempts`. The mechanism lives in the embedded stage loop
(`std/workflow/stage.harn`), so the closure runs in Harn and never crosses into
the host. `workflow_repair_stage_graph` (`std/workflow/patterns`) is the
one-stage sugar over this policy: a single delegated goal stage that retries
with feedback until it settles.

`workflow_run_repair` (`std/workflow/repair`) goes one step further and *runs*
that pattern for you — the run→validate→repair loop as a first-class helper:

```harn,ignore
let out = workflow_run_repair({
  task: "Write the release notes for v1.2.",
  model_policy: {provider: "anthropic", model: "claude-sonnet"},
  verify: {command: "scripts/lint_release_notes.sh", expect_status: 0},
  max_attempts: 3,
})
// out = {ok, status, text, findings, verification, attempts, result, run}
```

It runs one agent stage, validates its output with the supplied verifier
(a callable, a `{command, expect_status?}` check, or a
`{assert_text?, expect_status?}` assertion — command/assertion verifiers are
wrapped into fn-verify closures so they gate + retry), and re-prompts with the
findings up to `max_attempts` times. It owns no loop of its own; the retry and
findings-threading run in the same attempt machinery described above.

### The stage executor

By default an agent stage runs its attempt by delegating to a spawned worker.
Set `executor` on the stage node to run the attempt as an in-process Harn
closure instead. The closure *is* the attempt: it receives the attempt context
and returns the attempt result. Reach for it when the work is easier to express
as Harn code than as a delegated agent (a deterministic transform, a call into
your own module, a hand-built repair step), while still getting the stage's
retry, feedback threading, and fn-verify gate for free.

```harn,ignore
{
  id: "act",
  kind: "stage",
  retry_policy: {max_attempts: 3, feedback: true},
  executor: { ctx ->
    let patched = my_patch_step(ctx.task, ctx.prior_findings)
    return {text: patched.summary, artifacts: patched.artifacts}
  },
}
```

**The context it receives.** The closure is called with one dict of exactly
these keys:

```harn,ignore
{
  task,                // the (possibly repaired) task string for this attempt
  attempt,             // 1-based attempt number
  prior_findings,      // list<string> of findings from the previous attempt, [] on the first
  prior_verification,  // the previous attempt's verification dict, nil on the first
  prior_text,          // the previous attempt's visible text, "" on the first
  artifacts,           // the artifacts selected for this stage
}
```

The `feedback` / `repair_prompt_builder` policy has already been applied to
`task` before the closure sees it, so `ctx.task` on attempt 2+ already carries
the prior findings when feedback is on. The `prior_*` keys are there when you
want the raw signal rather than the templated task. (Note the distinction from
the `repair_prompt_builder` context, which keys findings as `findings` and adds
`error` and `stage`; the executor context uses `prior_findings` and adds
`artifacts`.)

**What it returns.** Return a dict shaped as:

```harn,ignore
{
  result?,        // the full stage result dict; or use `text` for the common case
  text?,          // convenience: wrapped into {status: "completed", visible_text: text}
  artifacts?,     // produced artifacts, defaults to []
  transcript?,    // falls back to the stage's input transcript
  verification?,  // a verdict dict {ok, findings?} to self-gate this attempt
}
```

Supply `result` for a full stage result, or `text` for the common "here's my
output" case. A returned `verification` runs through the same fn-verify gate as
any other stage, so an executor can grade its own attempt and drive the retry.

**A throw is a failed attempt.** If the closure throws, the stage contains it as
`{ok: false, error}` and the attempt fails like any other; it does not abort the
run. The next attempt fires with the incremented `attempt` and the failure
threaded into `prior_findings` (and into `task` when feedback is on). This
mirrors the delegated worker's success/failure contract exactly, so a stage
behaves identically whether it delegates or runs your closure.

**Where it composes.** `executor` is a field on any agent stage node, so it works
directly in `workflow_stages` (set `executor` on a stage row) and in
`workflow_run_repair` (pass `executor` in the config to replace the delegated
goal stage with your closure). `workflow_repair_stage_graph`
(`std/workflow/patterns`) wires `executor` onto its single goal stage when you
supply one.

### Building linear stage graphs

`workflow_stages` (`std/workflow/patterns`) is ergonomic sugar for the common
case of a linear stage pipeline. It expands a concise `WorkflowStagesSpec`
(a `list<StageSpec>`, or `{stages, name?, entry?, edges?}`) into the
`{entry, nodes, edges}` graph `workflow_execute` consumes — each stage's `id`
becomes the nodes-map key, stages are wired head-to-tail, and `entry` defaults
to the first stage. It is pure sugar over `workflow_graph`: the result is
byte-identical to the hand-authored equivalent, so there is no new node shape
or runtime concept to learn.

```harn,ignore
let graph = workflow_stages({
  name: "implement",
  stages: [
    {id: "act", kind: "stage", mode: "agent", model_policy: {provider: "mock"}},
    {id: "check", kind: "verify", mode: "command", verify: {expect_status: 0}},
  ],
})
```

### Stage option flattening and the capability ceiling

Before a stage runs its agent loop, its policy structs — model policy,
auto-compaction, tool spec, capability + approval policy, the workflow skill
registry, and nested-execution attribution — are *flattened* into the single
options dict the loop consumes. That flattening lives in Harn
(`workflow_flatten_agent_loop_options` in `std/workflow/stage.harn`): Harn
decides *what options the loop gets*.

The host keeps exactly one thing here — enforcement. Rust re-derives the
stage's capability ceiling (the intersection of the tool spec's implied policy
with the stage `capability_policy`) and, when the flattened dict crosses back
into the host, checks that its `policy` never *widens* that ceiling. A
flattener may narrow a capability, budget (`recursion_limit`), root allowlist,
side-effect level, or sandbox profile, but any attempt to add a tool or
capability, raise a budget, add a root, or loosen the sandbox is rejected with
a `tool_rejected` error naming the widened dimension. The ceiling is authority;
Harn is trusted only for shape, so the host re-checks rather than assuming the
flattener narrowed correctly.

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

Options currently include (typed as `WorkflowExecuteOptions` in
`std/workflow/options`):

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
tools again. For delegated stages, replay also preserves the recorded worker
envelope from stage metadata so replayed parent runs keep the same child
run/snapshot pointers for inspection and evals.

Delegated runs surface child worker lineage in each delegated stage's metadata.
This makes replay/eval and host timelines able to distinguish parent execution
from child execution without reconstructing that structure from plain text.
Persisted runs also retain explicit `parent_run_id`, `root_run_id`, and
`child_runs` lineage, and `load_run_tree(path)` materializes that hierarchy
recursively for inspection or host-side task views. When a process exits after a
stage record is written but before the parent `child_runs` list is refreshed,
subsequent save/load/normalize passes recover the child entry from the stage's
worker metadata before exposing or replaying the run.

Map nodes can now execute branch work in parallel. `node.join_policy.strategy`
accepts:

- `"all"` to wait for every branch result
- `"first"` to return after the first completed branch
- `"quorum"` to return after `join_policy.min_completed` branches finish

`node.map_policy.max_concurrent` limits branch fan-out, and partial failures are
retained alongside successful branch artifacts instead of aborting the whole map
stage on the first error.

Workflow state channels are a design-stage extension for workflows whose
fan-out branches should merge structured state by name instead of only
producing artifacts. The v0 proposal keeps artifacts and transcripts as the
default runtime model, then adds explicit `state_channels`, node `reads` /
`writes`, and deterministic reducers for cases that need LangGraph-style typed
state. See [Workflow state channels v0](./spec/workflow-channels/v0.md).

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

## Durable workflow messages

Workflows can also expose a durable mailbox/query surface that lives alongside
run records under `.harn/workflows/<workflow_id>/state.json`. This is the
shared substrate for external workflow control over Harn builtins, ACP, and
A2A without requiring a live in-memory handle.

The mailbox builtins are:

- `workflow.signal(target, name, payload?)`
- `workflow.query(target, name)`
- `workflow.publish_query(target, name, value?)`
- `workflow.update(target, name, payload?, options?)`
- `workflow.receive(target)`
- `workflow.respond_update(target, request_id, value, name?)`
- `workflow.pause(target)`
- `workflow.resume(target)`
- `workflow.status(target)`
- `workflow.continue_as_new(target)`
- `continue_as_new(target)`

`target` may be a workflow-id string or a dict with `workflow_id` /
`workflow`. When you already have a saved run, passing
`{workflow_id, persisted_path}` lets Harn derive the correct workspace root
without an extra lookup.

Use signals for one-way notifications, queries for last-known published state,
and updates when the caller needs a response:

```harn
let workflow_id = "customer-journey-42"

workflow.signal(workflow_id, "customer_joined", {customer_id: 7})
workflow.publish_query(workflow_id, "progress_pct", 25)

let next = workflow.receive(workflow_id)
log(next?.kind == "signal")
log(workflow.query(workflow_id, "progress_pct"))
```

`workflow.update(...)` enqueues a request and waits until
`workflow.respond_update(...)` publishes a response for the generated
`request_id`:

```harn
let response = workflow.update(
  "review-42",
  "approve_budget",
  {max_usd: 10},
  {timeout_ms: 5000}
)
log(response?.approved)
```

Pause and resume are durable state transitions, not ephemeral process-local
flags. They set `paused` in workflow state and enqueue a control message so the
workflow can observe that transition through `workflow.receive(...)`.

`workflow.continue_as_new(...)` increments the workflow generation counter and
clears pending update responses. The `std/agents` helper
`continue_as_new(prev, options?)` pairs that state transition with a transcript
reset so long-running workflows can roll forward without losing their durable
workflow identity.

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

For common graph shapes, prefer `std/workflow/patterns` over ad hoc graph
assembly:

- `workflow_self_verifying_graph(config?)` builds `act -> verify`.
- `workflow_command_verify_graph(config?)` builds
  `implement -> verify -> repair -> verify`.
- `workflow_verification_only_graph(config?)` runs only a deterministic
  verifier.
- `workflow_failover(config)` runs a typed failover loop over opaque route
  handles while the caller/host owns provider HTTP, credentials, endpoint
  policy, and billing.

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
harn runs view --json .harn-runs/<run>.json
harn runs view --json --session .harn-runs/
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
