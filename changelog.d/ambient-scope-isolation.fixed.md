- Give each spawned agent/worker task its own ambient execution scope. Capability
  context (execution/approval/command/autonomy policy, dynamic permissions, the
  bridge-trust + command-hook depths, the runtime-context overlay, the LLM
  render context, and the active connector context) lived in thread-local LIFO
  stacks whose guards were held across `.await`. Because workers run interleaved
  on a `spawn_local` LocalSet, a child reading its policy after an await could
  observe a *sibling's* top-of-stack — cross-wiring per-child file scoping, tool
  ceilings, approval, autonomy tier, template render context, and event
  attribution between concurrent fan-out children. Each worker future now swaps
  its own scope in and out around every poll (the `tracing::Instrument`
  technique), so only the currently-polling task's context is ever live on a
  thread. Correct under both cooperative and work-stealing multi-thread runtimes;
  O(1) per poll.
