- **CI now catches `#[harn_builtin]` defs that are declared but never
  installed on a live VM.** A builtin annotated with `#[harn_builtin]` is
  auto-added to the linkme `ALL_BUILTIN_DEFS` slice, but *installing* it onto a
  running VM still runs through hand-maintained `register_*` functions (the
  `LLM_RUNTIME_PRIMITIVE_BUILTINS` array, `register_agent_session_host_primitives`,
  the per-module `register_*_builtins`, …). A def could therefore exist — and
  pass every parser-alignment test — yet never be wired into runtime dispatch,
  so any call threw `Undefined builtin` at runtime and got silently swallowed by
  the agent loop's outer `try` (this is how `__host_agent_undispatched_tool_results`
  shipped inert before #3835). A new alignment test,
  `every_runtime_handler_builtin_is_installed_on_a_full_vm`, walks
  `ALL_BUILTIN_DEFS` against the fully-configured stdlib VM and fails the build
  if any runtime-handler def is missing from every `register_*` path, naming the
  builtin and the array to add it to.
