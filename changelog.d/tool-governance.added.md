- **ToolRegistry adapter governance.** Tools can declare a closed
  `governance.audiences` list for CLI, MCP, catalog, dashboard, and agent/model
  projections.
  Model prompt, schema, progressive-search, and direct-invocation paths now
  share the same fail-closed audience projection, while omitted governance
  preserves existing all-adapter behavior.
