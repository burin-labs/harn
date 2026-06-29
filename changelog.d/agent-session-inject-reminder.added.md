Added `agent_session_inject_reminder(session_id, options)` — inject a single typed
system reminder directly into a live session's transcript event stream,
bridge-free. The in-process sibling of the
`push_bridge_injection`/`drain_bridge_injections` reminder path, for hosts that
drive the agent loop without an ACP `HostBridge`. `options` mirrors
`transcript.inject_reminder` (`body` required; optional `tags`, `dedupe_key`,
`ttl_turns`, `preserve_on_compact`, `propagate`, `role_hint`); the loop's
existing `apply_reminder_post_turn` pass evicts the reminder once its `ttl_turns`
reaches zero.
