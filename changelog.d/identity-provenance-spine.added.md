- Added `std/identity` for Harn-native ActorChain validation, summaries, compact
  formatting, and structured provenance reports. `std/disclosure` now reuses
  those helpers for traversal and subject parsing.
- Extended `harn.mcp.status()` and `mcp_registry_status()` entries with server
  `transport`/`url` metadata, and added `display_identity` to
  `harn.mcp.status()` for connected OAuth-backed MCP servers with vetted
  identity descriptors.
