MCP servers that announce a changed tool/resource/prompt list
(`notifications/*/list_changed`) now emit an `mcp_catalog_changed` agent event,
so a connected client re-fetches the catalog and surfaces newly added tools
within the same session — no restart.
