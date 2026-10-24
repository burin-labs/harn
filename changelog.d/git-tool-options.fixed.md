Git tool registries check custom names, descriptions, and tool configuration
before execution. They reuse the shared `std/tools.ToolDefinitionConfig` type
and preserve shared and per-tool override order.
`std/git` also exports its catalog option and execution argument types.
