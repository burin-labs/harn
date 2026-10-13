- **Tool-result adjacency capability removed.** `harn_vm::llm::capabilities::Capabilities`
  and `ProviderRule` no longer have a `requires_tool_result_adjacency` field, and
  `provider_capabilities()` no longer reports it. Harn now keeps every tool
  result next to its call on every route, and a live probe showed OpenAI itself
  enforces that order, so the field no longer distinguished any route.

  Migration: delete reads of the field. Capability overlays that still set
  `requires_tool_result_adjacency` load unchanged; the key is ignored.
