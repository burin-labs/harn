- **MCP and local runtime edge cases.** MCP connects now reject unsupported
  protocol versions locally and tolerate padded version strings, bytecode cache
  writes avoid same-process temp-file collisions, and `harn local launch`
  deduplicates default flags supplied as `--flag=value`.
