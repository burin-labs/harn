- **`std/agent/lanes` and `std/agent/overlays`: tool-surface lanes and
  prompt-nudge overlays.** `lane_policy(rows, task, opts?)` classifies a task
  once into a named lane from a data table (`default_lane_rows()` ports
  burin-code's `agent_lane_for_task` decision ladder: 1-4 explicit file
  targets narrows to a `look`/`search`/`edit`/`run`/... `explicit_patch` lane,
  otherwise the unrestricted `general` lane) and narrows `opts.tools`/
  `opts.policy` down to that lane's allowed tools for the run, reusing the
  same tool-surface-narrowing primitives already shared with
  `std/agent/stance` — no new Rust, no new hook surface. Narrowing via
  `policy.tools` means a hidden tool is never *named* to the model as an
  alternative when the model attempts it: harn-vm's native tool-ceiling denial
  path reports only the tool actually attempted. `lane_scope_classifier(rows)`
  optionally lowers the same rows onto the existing `pre_turn_scope_classifier`
  seam for per-turn lane telemetry (never narrows or skips a turn itself).
  `overlay_policy(rows, mode, opts?)` layers data-driven, mode/lane-specific
  prompt-nudge lines onto the outbound system prompt through the existing
  `context_profile.prompt_fragments` channel (#2631) — it only ever adds a
  fragment alongside the caller's explicit `system`/fragments, never replaces
  them, and an `options.overrides` entry fills (and wins over) a row's slot.
  `agent_preset`/`agent_preset_register` gain `lane_policy`/`overlay_policy`
  pack keys (fill-nil, explicit caller input always wins); the built-in
  `repair` preset ships a default lane table and `review_captain` ships a
  default overlay row.
