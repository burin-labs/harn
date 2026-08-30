- **ToolRegistry adapter governance.** Tools can declare a closed
  `governance.audiences` list for CLI, MCP, catalog, and dashboard projections.
  Discovery and direct invocation now share the same fail-closed audience
  filter, while omitted governance preserves existing all-adapter behavior.
