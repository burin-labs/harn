- **Internal engine bugs surfaced during tool dispatch now abort the agent
  loop instead of being swallowed as a recoverable tool error.** The loop's
  tool-dispatch catch sites only ever distinguished `cancelled` errors; every
  other failure — including a `VmError::UndefinedBuiltin` (a `#[harn_builtin]`
  def missing from its install array), corrupt bytecode, or another VM
  invariant violation — was folded into a synthetic tool-error observation and
  the run marched on to a `done`/`stuck` status with no log, no non-zero exit,
  and no test failure. That is what let a mis-wired builtin ship silently inert.
  A new `ErrorCategory::Internal` classifies these faults (both the structured
  `VmError::UndefinedBuiltin`/`InvalidInstruction` variants and the stringly
  `"Undefined builtin: …"` message form), the Rust tool-dispatch retry loop no
  longer wastes retries on them, and every agent-loop tool/classifier catch site
  re-raises them through a shared `__agent_error_must_propagate` predicate —
  exactly like `cancelled`. `error_category(err)` now returns `"internal"` for
  these so Harn middleware can react to them too.
