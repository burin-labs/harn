The opt-in legacy ambient-capability bridge now resolves pre-cutover ambient
globals whose typed contracts are published as `__cap_<name>` (for example
`runtime_context_set`), and the runtime dispatches those runtime-context
builtins on the parent VM.
