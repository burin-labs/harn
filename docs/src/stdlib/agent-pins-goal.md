# Compaction pins and the goal object

Two additive stdlib modules give long-running agents durable intent:
`std/agent/pins` keeps load-bearing context alive across compaction, and
`std/agent/goal` turns a fuzzy objective into a typed, convergence-checkable
value. Both are plain data threaded through existing seams — neither adds a new
host surface.

## Pins: keep context alive across compaction

A pin is a small typed value `{kind, content, ...}`. The taxonomy generalizes
the durable "working set" a coding host maintains: `goal` (the objective),
`constraint` (guardrails / open task-contract clauses), `decision` (a choice the
agent must not relitigate), `artifact_ref` (a live file view or path the work
hinges on), and `no_compact` (a block marked keep-verbatim).

```harn,ignore
import { pin, pin_reminder, with_pin_roots, pin_compaction_policy } from "std/agent/pins"

const pins = [
  pin("goal", "Ship the auth migration", {dedupe_key: "pin/goal"}),
  pin("artifact_ref", "src/auth/session.rs"),
]

// (1) Survive compaction by construction: inject each pin as a
//     preserve_on_compact reminder, or hand the summarizer a preserve policy.
const policy = pin_compaction_policy(pins)

// (2) Double as reachability-GC roots: any stale tool result that references a
//     pinned path/identifier is kept, not reclaimed.
const projected = transcript_project(t, with_pin_roots({policy: "reachability_gc"}, pins))
```

`pin(kind, content, opts?)` validates the kind and normalizes the value;
`unpin(pins, id)` removes one; `pin_reachability_roots(pins)` returns the root
strings. Binding pins (`goal`, `constraint`, `no_compact`) render on the system
lane; evidence pins (`artifact_ref`, `decision`) on the developer lane.

### Ingesting the `[no-compact]` marker

Hosts that emit a literal `[no-compact]` heading marker can convert it to a pin:

```harn,ignore
import { recognize_no_compact } from "std/agent/pins"

const maybe_pin = recognize_no_compact("## Session goal [no-compact]\nObjective: X")
```

`recognize_no_compact` returns a `no_compact` pin with the marker stripped, or
`nil` when the marker is absent. It is an input adapter for an ingestion format,
not a shim for a removed API.

### Preset default pin policies

Agent presets can carry a default `pin_policy` pack row; the long-running
captains (`merge_captain`, `review_captain`, `oncall_captain`,
`release_captain`) ship one that pins goal/constraint/decision context by
default. A pack row fills only a nil/absent `pin_policy` option — explicit caller
input always wins.

## The goal object

`goal(spec)` normalizes `{objective, success_criteria, constraints, budget}`.
Each success criterion may carry a host-fact `check` callback, which makes it
machine-checkable (a deterministic floor) instead of LLM-judged.

```harn,ignore
import { goal, with_goal, goal_judge, goal_check, goal_reloop } from "std/agent/goal"

const g = goal({
  objective: "Fix the flaky login test",
  success_criteria: [
    "the suite passes twice in a row",
    {id: "green", description: "CI is green", check: { facts -> return facts?.ci_green == true }},
  ],
  constraints: ["do not touch auth.go"],
  budget: {max_cost_usd: 5.0},
})

// Render the goal into every outbound request (existing fragment channel):
const result = agent_loop("Proceed.", nil, with_goal({provider: "anthropic", done_judge: goal_judge(g)}, g))

// The machine-checkable floor:
const floor = goal_check(g, {ci_green: false})   // {done: false, unmet: ["green"], ...}
```

- `with_goal(opts, goal)` renders the objective, criteria, and constraints into
  the per-turn system prompt through the existing context-profile fragment
  channel.
- `goal_judge(goal, opts?)` returns a `done_judge` config (the semantic ceiling)
  that composes with the existing completion-judge seam; pair it with
  `goal_check` host-fact callbacks for the floor.
- `goal_reloop(goal, opts?)` returns `agent_loop` options that drive the bounded
  "not yet met, re-enter with findings" re-loop through `agent_loop`'s own
  completion loop: each completion attempt is gated by `verify_completion`
  (running `goal_check` against the facts `opts.facts_fn(payload)` extracts), an
  unmet goal vetoes the completion and threads the unmet criteria into the
  transcript as feedback, and the agent re-runs — up to `opts.max_attempts`
  (default 3). Spread it into `agent_loop(task, nil, goal_reloop(g, opts))`
  rather than wrapping `agent_loop` in a hand-written loop.
- `goal_pin(goal)` bridges a goal into a self-replacing `std/agent/pins` pin so
  the objective also survives compaction.
