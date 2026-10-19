- **Canonical session goal state (#10252).** Session goals retain typed criteria, verifier provenance, generations and
  turn lifecycle in the session journal. Internal goal bookkeeping works under a read-only execution ceiling without
  granting ordinary persistent-state or workspace writes. `SessionState` exposes the canonical `goal: SessionGoalState`
  field; its existing private fields still govern session initialization.

  Migration: code that reads or mirrors session metadata must preserve the goal record, including active and retired
  criteria, verifier provenance and turn state. Use the existing session APIs to create and restore sessions, and query
  goals with `harness.agent.goal` or Rust's `agent_sessions::session_goal`. Harn goal mutations require the session's
  canonical run through `harness.agent.with_run`. `harness.agent.reset` outside a run now persists the cleared goal and
  transcript to the session's canonical store before returning, so cold hydration does not restore them.
