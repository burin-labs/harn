# The expressiveness spectrum

One task. Five levels of control. The point of this page is a promise: the easy
case stays a few lines, and when you need more control you reach for it without
throwing away what you already wrote. Every level is the same primitives
composed one step further. You never pay for machinery you don't use.

The task, held fixed the whole way down: **take a bug report and produce a
fix.** At the top that means "guess the likely cause in a sentence." At the
bottom it means "edit the code, run the test, and don't stop until an
independent check says the test is green, and let the host (which can see the
test runner) tell the agent when it's stalling." Same job, more control.

Read top to bottom once to see the shape. Then use it as a menu: pick the
highest line on the page that still says "yes, that's all I need."

## Level 1: one call

You have a bug report and you want a fast read on it. Nothing to edit yet, no
tools, no loop. One request:

```harn
let hint = llm_call(
  "Bug: add(2,2) returned 5. One sentence: what's the likely cause?",
  "You are a careful Rust engineer.",
  {provider: "mock"},
)
log(hint.text)
```

Three lines. [`llm_call`](../llm/llm_call.md) sends one prompt and hands back a
result dict. This is the floor, and most classification, summarization, and
extraction never needs to leave it. If your whole job is "read this, tell me
that," stop here.

## Level 2: make the answer a shape you can trust

`hint.text` is prose. The moment you want to branch on the answer in code
(route by severity, file under a component), free text is a liability. Ask for a
schema instead and get back typed data, validated, with a retry on the model's
first malformed attempt:

```harn
let triage = llm_call_structured(
  "Triage this bug: add(2,2) returned 5 in src/math.rs.",
  {
    type: "object",
    required: ["severity", "component"],
    properties: {
      severity: {type: "string"},
      component: {type: "string"},
    },
  },
  {provider: "mock"},
)
log(triage.severity)
log(triage.component)
```

Same call underneath. [`llm_call_structured`](../llm/llm_call.md#llm_call_structured)
just pre-applies the schema-validated-JSON defaults and re-asks once if the model
returns something off-shape, so `triage.severity` is a string you can switch on
instead of a paragraph you have to parse. You climbed one rung and wrote two
extra lines.

## Level 3: let it actually do the work

Reading and classifying is one model call. *Fixing* the bug is not. The agent
has to read the file, edit it, run the test, look at what the test said, and
decide what to do next. That is a loop, and you should not hand-write it.

[`agent_loop`](../llm/agent_loop.md) is the loop. But you don't have to
configure it by hand. [`agent_preset(kind)`](../llm/agent_loop.md#presets-how-you-build-agent_loop-options)
hands you a tuned options dict for a common shape. Here that's `"repair"`: a
tool-using agent with a sane iteration budget, stall detection, and bounded
transport retry already set.

```harn
import { agent_preset } from "std/agent/presets"

let opts = agent_preset("repair", {provider: "mock"})
let run = agent_loop(
  "Fix the failing test test_add in src/math.rs, then run it to confirm.",
  opts?.system,
  opts,
)
log(run.status)
log(run.visible_text)
```

`agent_preset` is not a new tier. It returns a plain `agent_loop` options dict
and your explicit keys always win, so it's a starting point you customize, never
a wall. The built-in kinds are `audit`, `repair`, `summary`, `verify`, and the
four captains (`merge_captain`, `review_captain`, `oncall_captain`,
`release_captain`). Register your own with `agent_preset_register` when a shape
recurs in your codebase.

Most agents want exactly this: a preset, your tools, go. If the defaults fit,
you're done at Level 3.

## Level 4: tune the loop by composing building blocks

Sometimes the defaults don't fit and you need to shape *how* the loop behaves:
cap its spend, hide tools it shouldn't touch, add a real gate on "done," nudge
its prompt. The mistake here is to write that logic inline in a loop body you
hand-rolled. Don't. Each concern has one home module, and each returns a value
you fold into the same `agent_loop` options dict. You compose them; the loop
stays the loop.

```harn,ignore
import { agent_preset } from "std/agent/presets"
import { with_governance } from "std/agent/governors"
import { agent_completion_gate } from "std/agent/judge"
import { lane_policy } from "std/agent/lanes"
import { with_overlay } from "std/agent/overlays"

// Start from the preset, then layer control onto it.
var opts = agent_preset("repair", {provider: "ollama", model: "qwen3-coder", tools: repair_tools})

// Bound cost and cadence; add a stall detector that fires when the agent
// keeps editing but nothing verifies as progress.
opts = with_governance(opts, {
  governor: {budget: 40.0, signal: "iterations"},
  detectors: {no_progress: {messages: 3}, stuck: {same_diagnostic: 3}},
})

// An independent "are we actually done?" gate, not the model's own say-so.
opts = {...opts, ...agent_completion_gate({
  require: { result -> contains(to_string(result?.text), "test result: ok") },
})}

// Hide tools this task can't need, and fill in a prompt nudge — both are
// fill-nil, so they never override anything you set explicitly.
opts = lane_policy(lane_rows, task, opts)
opts = with_overlay(opts, overlay_rows, "repair")

let run = agent_loop(task, opts?.system, opts)
```

Every one of those lines is opt-in. Governors bound spend
([`std/agent/governors`](../stdlib/governors.md)). Detectors catch loops and
stalls. The completion gate ([`std/agent/judge`](../stdlib/agent-judge.md))
is a deterministic veto plus an optional bounded judge, so "done" is a fact you
control, not a mood the model is in. Lanes narrow the tool surface; overlays add
data-driven prompt nudges ([`std/agent/lanes` and `std/agent/overlays`](../stdlib/agent-lanes-overlays.md)).
Leave any of them out and the loop still runs. You add guardrails one at a
time, each from its own module, none of them a new hook on the loop. The
[placement contract](./abstraction-ladder.md#the-placement-contract-where-cross-cutting-mechanisms-live)
is the full map of which concern lives where.

## Level 5: full control, and let the host feed in what it can see

The ceiling. You want more than one attempt, an independent verify stage between
attempts, and (the part only a workflow reaches) the ability to run each
attempt as your own code and to let the host tell the loop things it cannot see
on its own. This is the level Burin builds on.

Two seams open up here.

**The executor closure.** A stage's `executor` runs the attempt as a plain Harn
closure instead of delegating to a spawned worker. It receives the attempt's
context and returns a result; a throw counts as a failed attempt and feeds the
retry. You own what happens inside a try:

```harn,ignore
{
  id: "act",
  kind: "stage",
  retry_policy: {max_attempts: 3, feedback: true},
  executor: { ctx ->
    // ctx = {task, attempt, prior_findings, prior_verification, prior_text, artifacts}
    let patched = my_patch_step(ctx.task, ctx.prior_findings)
    return {text: patched.summary, artifacts: patched.artifacts}
  },
}
```

**Host-fed facts.** The stall detector owns the *mechanism*: when to warn, when
to cut a run for spinning in place. But only the host can see whether the test
runner actually moved. So the host feeds that fact in through a callback. The
loop supplies the payload; you return the one number that matters:

```harn,ignore
// "Writes are not progress." Return the turn's best verify-state — say, the
// count of passing tests. nil means "no verification evidence this turn."
let opts = {...base, stall_diagnostics: {
  progress_signal: { payload -> host_passing_test_count() },
}}
```

Now "the agent edited a file" and "the build got closer to green" are two
different facts, and the loop stops mistaking motion for progress. Those
callbacks (progress, delivered-fix-not-landing, pace decisions) are covered in
[Host-supplied facts](../stdlib/fact-intake-seams.md). Wrap the whole thing in
[`workflow_stages`](../workflow-runtime.md#building-linear-stage-graphs) for the
verify stage, retry threading, replay, and audit, and you have the full machine.

```harn,ignore
import { workflow_stages } from "std/workflow/patterns"

let graph = workflow_stages({
  name: "fix-the-test",
  stages: [
    {id: "act", kind: "stage", mode: "agent",
      retry_policy: {max_attempts: 3, feedback: true},
      executor: my_executor},
    {id: "check", kind: "verify", mode: "command",
      verify: {command: "cargo test test_add --quiet", expect_status: 0}},
  ],
})
let run = workflow_execute("Fix the failing test.", graph)
```

## The through-line

Five levels, one task, and each level is the level above it with one more thing
composed on:

| Level | Reach for | You get | You write |
|---|---|---|---|
| 1 | `llm_call` | one answer | 3 lines |
| 2 | `llm_call_structured` | typed, validated output | +2 lines |
| 3 | `agent_preset(kind)` + `agent_loop` | a tuned tool-using agent | +1 import |
| 4 | `agent_loop` + governors / judge / lanes / overlays | bounded, gated, narrowed control | one fold per concern |
| 5 | `workflow_stages` + `executor` + host-fed facts | attempts, verify gates, replay, your code in the loop | a closure and a callback |

You are never forced up the table, and moving up never means a rewrite. The
call you wrote at Level 1 is still the call running underneath at Level 5. That
is the whole design: the easy case is easy, and the hard case is the easy case
with more composed onto it.

## See also

- [Build your first workflow](../tutorials/build-your-first-workflow.md) — the
  guided walkthrough of levels 1, 3, and 5, runnable end to end.
- [Choosing an agent abstraction](./abstraction-ladder.md) — the ladder and the
  placement contract in reference form.
- [Workflow runtime](../workflow-runtime.md) — the executor closure and retry
  policy in full.
- [Host-supplied facts](../stdlib/fact-intake-seams.md) — the callbacks that let
  your host feed the loop what it can see.
