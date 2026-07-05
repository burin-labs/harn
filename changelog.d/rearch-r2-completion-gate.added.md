- **Added `agent_completion_gate` (`std/agent/judge`)** — a configured done-time
  completion gate that composes the existing `verify_completion` and bounded
  `verify_completion_judge` / `done_judge` seams (no new loop seam). It
  generalizes burin-code's completion-verification policy: an ordered veto ladder,
  a source-write **evidence** requirement (only source writes count as progress
  toward "done" — cosmetic/test-scaffold writes do not), a per-session veto budget
  (`max_vetoes`, default 3) with strict post-write classes (a red verifier, or a
  source write whose verifier has not gone green) the budget never releases, and
  AND-of-oracles verify composition (`veto_combine`-configurable). Every domain
  fact stays a host callback (`facts`, `classify_write`, `verify_command`); with
  no callbacks the gate degrades to judge-only mode and surfaces the degraded state
  on the returned bundle (`_completion_gate.facts_available = false`) instead of
  fabricating a pass. The gate never keys on a done-sentinel string. Presets can
  carry a default via the new `completion_gate` pack row.
