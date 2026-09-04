Added an end-to-end reach test for #7884/#7893: a compiled Harn tool handler
that returns `{ok: false, error: "..."}` or the MCP `{isError: true}` shape
without throwing is dispatched through the real `host_agent_dispatch_tool_call`
entry point, and the recorded result must show `ok: false`, `status: "error"`,
and a failure text that reaches the next turn's observation. No runtime
behavior changed; the classifier fix already shipped in #7893.
