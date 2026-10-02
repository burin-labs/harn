- `harn tool run`, `harn serve mcp`, and exported-function dispatch now read the
  `harn.agent_tool_handler_result.v2` envelope with the same parser as agent
  dispatch. An `"ok"` envelope succeeds with its `data`, which is what the
  declared output schema validates; `"error"` and `"rejected"` envelopes are
  declared application failures that carry `data` and `outcome`. A handler
  written for agent dispatch with an output schema no longer fails on the CLI
  and MCP with "output violates its declared schema", and an error outcome is
  no longer reported as success. Non-envelope returns are unchanged.
