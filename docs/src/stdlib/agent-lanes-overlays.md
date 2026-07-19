# Lanes and prompt overlays

Two additive stdlib modules generalize hand-rolled orchestration mechanisms
from burin-code into stdlib: `std/agent/lanes` (tool-surface narrowing keyed
off a data-driven task classification) and `std/agent/overlays` (a data-driven
prompt-nudge overlay). Both are plain data threaded through existing seams —
neither adds a new host surface or a new hook.

## Lanes: classify the task, narrow the tool surface

`lane_policy(rows, task, agent_options?, options?)` classifies `task` into a
named lane and narrows `agent_options.tools`/`agent_options.policy` down to
that lane's allowed tools for the whole `agent_loop` run:

```harn,ignore
import { default_lane_rows, lane_policy } from "std/agent/lanes"

const opts = lane_policy(
  default_lane_rows(),
  task,
  {provider: "anthropic", tools: my_tools},
)
const result = agent_loop(task, nil, opts)
```

`default_lane_rows()` ports burin-code's `agent_lane_for_task` decision table:
a task naming 1-4 explicit file targets (e.g. `src/widget.py`) gets the narrow
`explicit_patch` lane (`look`, `search`, `edit`, `run`, `poll_command`,
`wait_command`, `kill_command`, `read_command_output`); everything else —
including a task naming *more than* 4 targets — stays on the unrestricted
`general` lane.

Lanes are classified **once, from the task**, not reclassified every turn.
This matches burin's own design: narrowing tool *names* mid-transcript risks
the model hallucinating a call to a tool it remembers offering but that has
since been hidden.

### Lane rows are data

```harn,ignore
pub type LaneRow = {
  name: string,
  tool_names?: list<string>,
  tool_kinds?: list<string>,
  match?: dict,
  default?: bool,
}
```

A row's `match` declares which classification strategies apply —
`keywords: [...]` (substring OR-match against the task) and/or
`explicit_target_paths: {min?, max?}` (bounds on the count of detected
file-path tokens). Add a lane by appending a row; there is no `if` branch to
edit. Exactly one row must set `default: true`.

`tool_kinds` resolves against the tool registry's `annotations.kind` /
`annotations.tool_kind` — the same fail-safe annotation lookup
`std/agent/stance` uses — so a lane can be defined by capability class instead
of an explicit name list.

`agent_lane_classify(rows, task, options?)` accepts a custom
`options.classifier` callable to override the heuristic (e.g. an LLM-based
classifier); an unrecognized result falls back to the heuristic instead of
silently accepting an invented lane name.

### Enforcement seam, and why hidden tools are never named

Narrowing reuses the exact tool-surface-narrowing primitives already shared
with `std/agent/stance` (`__tool_surface_filter_registry` /
`__tool_surface_policy_tools`) — no new Rust, no new hook surface. That shared
seam is also why a tool a lane hides is never *named* to the model as an
alternative: narrowing via `policy.tools` means an attempt to call a hidden
tool is rejected by harn-vm's native tool-ceiling/name-resolution denial path,
which reports only the ONE tool the model just attempted and lists what IS
available — it never enumerates the other hidden tools by name.

### Per-turn observability (optional)

`lane_scope_classifier(rows, options?)` lowers the same lane rows onto the
existing `pre_turn_scope_classifier` seam (`std/llm/scope_classifier`,
consulted by `agent_loop` every turn) purely for telemetry: it emits the
standard `scope_classifier_verdict` event every turn, and always reports
`label: "in_scope"` / `skip_main_turn: false` — it never skips a turn and
never narrows the tool surface itself. The classified lane rides the
verdict's free-text `evidence` field (`"lane=<name> reason=<reason>"`)
rather than a dedicated key, because the native event has a fixed Rust-side
shape that silently drops unrecognized fields. Enforcement is
`lane_policy`'s job; this is an audit trail for a long-running lane-scoped
session, spread in alongside it:

```harn,ignore
agent_loop(task, nil, lane_policy(rows, task, opts) + lane_scope_classifier(rows))
```

## Overlays: data-driven prompt nudges

`with_overlay(agent_options, rows, mode, options?)` layers mode-specific
(and optionally lane-specific) guidance lines onto the outbound system prompt,
generalizing burin-code's flag-gated `mode_overlay_lines`. Its `agent_options`-first
arg order matches the rest of the fold family (`with_goal`, `with_governance`);
`overlay_policy(rows, mode, agent_options?, options?)` remains as a deprecated
alias with the old `rows`-first order.

```harn,ignore
import { default_overlay_rows, with_overlay } from "std/agent/overlays"

const opts = with_overlay({provider: "anthropic"}, default_overlay_rows(), "agent")
const result = agent_loop(task, nil, opts)
```

Rows are data (`{mode, lane?, lines, enabled?}`); a row whose `lane` matches
the active lane (see `std/agent/lanes`) wins over a `lane`-less row for the
same `mode`. `enabled: false` disables a row without removing it from the
table.

### Fill nil, never override explicit input

An overlay only ever **adds** a fragment via the existing
`context_profile.prompt_fragments` channel (harn#2631,
`std/agent/preflight::agent_build_turn_system_fragments`) — it never touches
or replaces `agent_options.system` or any fragment the
caller already set. Within the overlay's own content, `options.overrides`
(keyed by `"<mode>"` or `"<mode>:<lane>"`) supplies caller lines that **win**
over that slot's row default — the row only fills the slot when the caller
left it nil:

```harn,ignore
with_overlay(opts, rows, "agent", {overrides: {agent: ["Custom nudge instead of the row default."]}})
```

## Preset packs

`agent_preset`/`agent_preset_register` accept `lane_policy` and
`overlay_policy` pack keys (alongside `budget`, `models`,
`completion_gate`, ...) via the existing fill-nil pack mechanism
(`std/agent/presets`): a pack row fills the option only when the caller left
it nil, and a consumer explicitly lowers it —

```harn,ignore
const opts = agent_preset("repair", {tools: my_tools})
const lane_pack = opts?.lane_policy
if lane_pack != nil {
  opts = lane_policy(lane_pack.rows, task, opts, lane_pack)
}
```

The built-in `repair` preset ships a `lane_policy` pack row (the default lane
table); `review_captain` ships an `overlay_policy` pack row (the base
`agent`-mode economy nudge). Explicit caller input always wins over a preset's
pack default.
