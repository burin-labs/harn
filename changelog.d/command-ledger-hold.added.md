- The agent loop now OWNS long-running commands instead of asking the model to poll them. A converted
  `run_command` (a `status=="running"` handle) is entered in a session-scoped command ledger; while an `awaited`
  handle is live and a model turn makes no tool calls, the loop parks on the session inbox with zero inference and
  re-enters the model only on its own sparse, delta-gated decision schedule (30s base, doubling, 5-minute cap) with
  ONE coalesced `command_status` digest covering every live handle. Terminal completions wake the hold immediately;
  progress-only re-entries are output-capped while terminal-result re-entries stay uncapped. Per-handle first-stderr
  and byte-stall triggers, a 15-minute awaited-wall ceiling with per-surface auto-resolve (kill in headless/eval,
  release-to-service in interactive), a separate hold re-entry budget, and a `command_hold` checkpoint kind make the
  mechanism bounded and replay-deterministic. Thresholds and wording are normalized once through the `command_wait`
  options contract, so a host passes only overrides.
