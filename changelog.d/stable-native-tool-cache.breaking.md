Cache stable native tool definitions separately from session context while
retaining automatic conversation caching on supported routes.

Migration: Rust consumers matching `BreakpointPlacement` must handle
`ToolsAndTopLevel { tool_index }`. Receipt consumers must accept
`style: {"tools_and_top_level":{"tool_index":N}}` alongside the existing
string styles; the zero-based index identifies the final native tool.
