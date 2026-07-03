- **Mid-conversation MCP mounting for skill-declared servers (default-off).**
  MCP servers were only bootstrapped once, at agent-loop entry, so a skill that
  activated mid-conversation could never surface its MCP tools. A SKILL.md
  frontmatter field `mcp` (alias `mcp-servers`) now carries opaque MCP server
  specs, and — when the new default-off `mid_conversation_mcp_mount` loop opt is
  set — the loop mounts any server an active skill declares that is not already
  active. `agent_mcp_mount_additional` bootstraps only the delta (tracked via
  the running `_mcp_server_info` list), reusing the same catalog merge and
  `__with_mcp_tool_ceiling` admission as the initial bootstrap so the new
  `server__tool` entries become visible AND callable without re-connecting a
  live server or duplicating tools/ceiling entries. `install_session_mcp_clients`
  now merges (rather than replaces) the session MCP client map so an incremental
  bootstrap never drops live handles. With the flag off the one-time bootstrap
  path is byte-identical to before.
