`harness.agent.session_*` (and related live-session journal) capability effect
metadata now matches the `__host_agent_*` runtime_internal builtins (`effects =
[]`), so agent-loop session init/record/finalize no longer trip the active
effect ceiling mid-turn.
