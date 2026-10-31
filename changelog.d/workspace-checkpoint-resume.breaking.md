- **The tool-execution boundary now returns a typed admission result.** A
  boolean could not distinguish an untracked call from a call that must be
  refused because durable checkpoint tracking failed.

  Migration: update each `ToolExecutionBoundary` callback to return
  `ToolExecutionAdmission`. Return `Tracked` only after opening the call
  window, `Untracked` when checkpoint tracking does not apply, and
  `Refused(detail)` when the call cannot safely proceed.
