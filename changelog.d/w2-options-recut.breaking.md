- **LLM calls now have one canonical, validated option surface.** `llm_call`
  and its safe, structured, streaming, agent-loop, and persona entry points
  reject unknown or removed keys instead of silently dropping them. `output`
  replaces the structured-output aliases; `system` accepts ordered fragments;
  provider-specific settings live under `provider_options`; and the canonical
  transport, speed, effort, routing, tool, store, repair, and model-policy
  spellings replace their former synonyms. The typechecker, runtime, stdlib,
  linter, docs, skills, persona manifests, and migration guide derive from or
  document the same `harn-builtin-meta::llm_options` registry.
