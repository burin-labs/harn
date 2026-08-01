`harness.agent.session_*` (and related live-session journal) capability effect
metadata now matches the `__host_agent_*` runtime_internal builtins (`effects =
[]`), and `push`/`pop_llm_render_context` no longer claim durable
`state.mutate`, so agent-loop session init/render/finalize no longer trip the
active effect ceiling mid-turn.
