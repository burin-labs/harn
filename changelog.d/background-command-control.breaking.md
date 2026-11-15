- ACP hosts can cancel a tool call's background command by passing its `handleId`
  to `session/cancel_tool_call`. Cancellation uses Harn's command registry and
  refuses handles owned by another session. The public Rust request struct
  `AcpSessionCancelToolCallParams` gains an optional `handle_id` field.

  Migration: use `AcpSessionCancelToolCallParams::new(session_id, tool_call_id)`,
  or add `handle_id: None` to existing struct literals. Set it to the background
  command handle to cancel that command after the original tool call returns.
