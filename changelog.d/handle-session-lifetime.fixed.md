- **A background command handle now lives as long as its session, not the agent-loop run that
  started it.** A host that runs one loop per turn can poll, wait on, or kill a command an earlier
  turn started; closing the session still cancels its handles. Embedders can register their own
  cleanup with `agent_sessions::register_session_closed_hook`.
