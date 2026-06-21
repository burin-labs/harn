- `agent_session_push_user_message(session_id, options)` (Harn stdlib
  `std/agent/state`): the in-VM, loop-driver equivalent of the ACP
  `session/inject` method. Pushes a user-role message onto the running
  session; `options.mode: "steer"` delivers it at the next tool/iteration
  break-point (after the in-flight tool result, before the next model call),
  `options.mode: "queue"` defers it to loop exit. Lets in-process hosts (e.g.
  the Burin TUI) steer a turn without the ACP wire. Refs: rfd/session-inject.
