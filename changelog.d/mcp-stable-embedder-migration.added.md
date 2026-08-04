- Documented the `harn_vm::mcp_protocol` stable-MCP cutover in
  [Migrating to 0.10](https://docs.harnlang.com/migrations/v0.10.html) for Rust
  embedders: the RC-to-stable rename table, the symbols that went away because
  there is nothing left to negotiate, the `_meta` fields every request must now
  carry, and the two renames that compile cleanly and fail at run time
  (`HeaderName::from_static` panics on `MCP-Protocol-Version`, and
  `server/discover` moved `serverInfo` under `_meta`).
