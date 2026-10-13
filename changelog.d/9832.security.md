- `harn serve mcp` now serves script-published tools, resources, and prompts
  through the same MCP server as exported `pub fn`s. Script-registry tasks over
  HTTP are owned by the authenticated caller that started them; previously any
  caller holding a valid key could read or cancel another caller's task.
  Exported-function servers also gain `harn.hitl.respond`, and both surfaces
  require authorization for every method except `ping` and `server/discover`.
- A script-published MCP tool that allows tasks now runs in the background and
  returns its task id immediately, instead of finishing before `tools/call`
  answered. Calls the registry would refuse are refused before a task exists.
- `harn serve mcp` advertises the tasks extension only when some tool can run
  as a task, and `completion/complete` only when resources or prompts exist,
  for both script and export surfaces.
- An MCP HTTP client whose `Accept` header omits `text/event-stream` receives
  `tools/call` results as a JSON body. Export-surface list pages now follow
  `HARN_MCP_LIST_PAGE_SIZE` (default 100) like every other MCP list.
