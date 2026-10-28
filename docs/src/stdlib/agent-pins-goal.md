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
import {
  pin, pin_reminder, with_pin_roots, pin_compaction_policy,
} from "std/agent/pins"

const pins = [
  pin("goal", "Ship the auth migration", {dedupe_key: "pin/goal"}),
  pin("artifact_ref", "src/auth/session.rs"),
]

// (1) Survive compaction by construction: inject each pin as a
// preserve_on_compact reminder, or hand
// the summarizer a preserve policy.
const policy = pin_compaction_policy(pins)

// (2) Double as reachability-GC roots: any stale tool result that
// references a
//     pinned path/identifier is kept, not reclaimed.
const projected = transcript_project(
  t, with_pin_roots({policy: "reachability_gc"}, pins),
)
```

`pin(kind, content, opts?)` validates the kind and normalizes the value;
`unpin(pins, id)` removes one; `pin_reachability_roots(pins)` returns the root
strings. Binding pins (`goal`, `constraint`, `no_compact`) render on the system
lane; evidence pins (`artifact_ref`, `decision`) on the developer lane.

### Ingesting the `[no-compact]` marker

Hosts that emit a literal `[no-compact]` heading marker can convert it to a pin:

```harn,ignore
import { recognize_no_compact } from "std/agent/pins"

const maybe_pin = recognize_no_compact(
  "## Session goal [no-compact]\nObjective: X"
)
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

### Canonical session goal state

`harness.agent.goal(id)` reads the session's canonical goal record. It contains
`spec`, `metadata`, `generation`, `created_at`, `updated_at`, and `turn`.
Nonempty timestamps use UTC RFC3339.
`spec` is nullable and uses the same serializable goal shape as managed pins:
`objective`, `success_criteria`, `retired_criteria`, `constraints`, and `budget`.
Criteria contain `id` and `description`; executable checks stay with the caller.

The closed metadata record contains `acceptance_note`, `criterion_sources`,
nullable `verification`, optional `target`, and `source`. Verification contains
the exact `command` and its opaque `source`. When supplied, `target` is the
nonblank identity of the requested edit target. Criterion-source keys must
identify an active or retired criterion. The runtime preserves these values
without interpreting their provenance or inserting metadata into the model's
prompt.

| Method | Result | Meaning |
| --- | --- | --- |
| `set_goal(id, {spec, metadata})` | goal record | Replace the goal and advance its generation |
| `clear_goal(id)` | boolean | Clear the goal and advance its generation |
| `begin_goal_turn(id)` | boolean | Open a turn; report whether an unreturned predecessor's goal was retired |
| `end_goal_turn(id, cancelled)` | nil | Close a turn before its terminal boundary |

Mutations require the calling task's existing canonical run. Use
`harness.agent.with_run` around preparation or a standalone goal command; the
same owner hydrates cold state, acquires the writer lease, flushes the journal,
and handles cancellation. Another task's live writer is refused. A bare goal
mutation never creates a second writer or an unpersisted fallback.

The complete goal record is limited to 64 KiB. Retired criteria retain their
original identities, including IDs reused by later goals. Unknown fields,
duplicate active or blank criterion identities, missing source references, non-JSON runtime
values, nonfinite numbers, and oversized records are refused before mutation.
An unknown session or a sealed terminal run also refuses mutation. Refusal does
not advance the generation.

Beginning an idle turn retains its goal. Beginning after an unreturned open
turn clears the predecessor's goal and advances its generation. Normal or
failed completion closes the marker and retains the goal; cancellation clears
it before the terminal journal seals. Reset clears the goal. Forks copy an
isolated goal snapshot with an idle marker; cold restore preserves the marker
and generation. A VM fork persists its inherited goal through its first
canonical run; it refuses a destination that already has canonical history or
different lineage. These internal operations do not authorize ordinary durable
key-value writes or workspace mutations in a read-only session.

`goal(spec)` normalizes `{objective, success_criteria, constraints, budget}`.
Each success criterion may carry a host-fact `check` callback, which makes it
machine-checkable (a deterministic floor) instead of LLM-judged.

```harn,ignore
import {
  goal, with_goal, goal_judge, goal_check, goal_reloop,
} from "std/agent/goal"

const g = goal({
  objective: "Fix the flaky login test",
  success_criteria: [
    "the suite passes twice in a row",
    {
      id: "green",
      description: "CI is green",
      check: { facts -> return facts?.ci_green == true },
    },
  ],
  constraints: ["do not touch auth.go"],
  budget: {max_cost_usd: 5.0},
})

// Render the goal into every outbound
// request (existing fragment channel):
const goal_opts = with_goal(
  {provider: "anthropic", turn_end_condition: goal_judge(g)}, g,
)
const result = agent_loop(harness, "Proceed.", nil, goal_opts)

// The machine-checkable floor:
// {done: false, unmet: ["green"], ...}
const floor = goal_check(g, {ci_green: false})
```

- `with_goal(opts, goal)` renders the objective, criteria, and constraints into
  the per-turn system prompt through the existing context-profile fragment
  channel.
- `goal_judge(goal, opts?)` returns a `turn_end_condition` config (the semantic ceiling)
  that composes with the existing completion-judge seam; pair it with
  `goal_check` host-fact callbacks for the floor.
- `goal_reloop(goal, opts?)` returns `agent_loop` options that drive the bounded
  "not yet met, re-enter with findings" re-loop through `agent_loop`'s own
  completion loop: each completion attempt is gated by `verify_completion`
  (running `goal_check` against the facts `opts.facts_fn(payload)` extracts), an
  unmet goal vetoes the completion and threads the unmet criteria into the
  transcript as feedback, and the agent re-runs — up to `opts.max_attempts`
  (default 3). Spread it into `agent_loop(harness, task, nil, goal_reloop(g, opts))`
  rather than wrapping `agent_loop` in a hand-written loop.
- `goal_pin(goal)` bridges a goal into a self-replacing `std/agent/pins` pin so
  the objective also survives compaction.
- `goal_under_obligations(goal, obligations?)` returns the goal as accepted
  steering left it. A steer that carried a `goal` (see
  [Retarget](../remote-session-control.md#retarget)) replaces the objective and
  moves every success criterion to `retired_criteria`, which no longer gates
  completion. `with_goal` applies this projection before each model request;
  `goal_judge` and `goal_reloop` apply it at completion. Each projection records
  retired criterion ids and descriptions in a
  `harn.goal_criteria_retired.v1` checkpoint, linked to the accepted control.
  When the retarget lands, typed goal pins replace their objective and retire
  their criteria, preserving constraints and budget across compaction.
  Unstructured goal pins retire; the new objective's standing directive survives
  compaction. `agent_pin` projects a `goal_pin`
  under the current obligations even when you reuse a pin built before steering.
