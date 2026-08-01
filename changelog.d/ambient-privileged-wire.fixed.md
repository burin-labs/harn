Ambient legacy calls resolve to in-process runtime handlers: privileged-wire
builtins such as `security_policy` lower to `__security_policy`, and
`__host_*` primitives are projected under their pre-cutover names (for
example `agent_emit_event`) so agent-loop event emission does not fall
through to the embedder bridge under execution policy.
