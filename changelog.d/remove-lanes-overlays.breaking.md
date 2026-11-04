- **Removed `std/agent/lanes` and `std/agent/overlays`.** The modules classified
  the first user message into a fixed tool lane and appended mode-specific
  prompt lines. No in-repo caller used them, and a lane pinned the tool
  surface to the first message, so later user direction could not widen it. The
  `lane_policy` and `overlay_policy` preset-pack keys and agent option fields
  are gone with them, along with `lane_scope_classifier` and the
  `stage_gate_*` text-marker helpers.

  Migration: delete the imports. Narrow tools through the tool list passed to
  `agent_loop`, and put mode-specific guidance in the system prompt or a
  skill. A preset registered with `lane_policy` or `overlay_policy` now fails
  as an unknown pack key.
