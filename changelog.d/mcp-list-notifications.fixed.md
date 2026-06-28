MCP list-change notifications now refresh manifest-derived prompt state through
the same in-process path used by the file watcher, making prompt and package
metadata updates deterministic under load.
