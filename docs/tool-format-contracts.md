# Tool format contracts

An agent transcript is bound to one tool-calling contract for its lifetime.

## Invariant

- The first `agent_loop` that runs against a named session claims the session's
  `tool_format`.
- Re-entering that same session with a different `tool_format` is a runtime
  error.
- `agent_session_reset` clears the transcript and the claimed tool format.
- `agent_session_fork` and `agent_session_fork_at` copy the claimed tool format,
  because a fork reuses prompt/history produced under that contract.
- Child and sub-agent sessions start as new transcripts. They record lineage
  events on the parent, but they do not inherit the parent's messages or tool
  format and may choose a different model/tool contract.

## Model switching

Changing models inside an existing transcript is valid only when the effective
tool format stays the same. If a requested model change would cross from `text`
to `native`, or from `native` to `text`, callers must create a new transcript or
explicitly reset the current session before running the next model call. If
history must be preserved across a cross-format model switch, summarize or
compact the prior transcript into a fresh session and then claim the new
session's target `tool_format`. This avoids replaying text-protocol
instructions, text-mode tool schemas, or native-only tool messages under the
wrong harness contract.

Native text-tool fallback is opt-in compatibility behavior. The default policy
is `reject`, so a native session no longer accepts `<tool_call>` text as a
successful tool invocation unless the caller explicitly sets
`native_tool_fallback: "allow"` or `"allow_once"`.

## Builtins

- `agent_session_tool_format(session_id)` returns the claimed tool format or
  `nil`.
- `agent_session_claim_tool_format(session_id, tool_format)` claims the contract
  or errors if the session already has a different one.
