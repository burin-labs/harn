ACP hosts can cancel a tool call's background command by passing its `handleId`
to `session/cancel_tool_call`. Cancellation uses Harn's command registry and
refuses handles owned by another session.
