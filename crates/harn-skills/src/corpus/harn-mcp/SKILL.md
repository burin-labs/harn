---
name: harn-mcp
short: Connect Harn to MCP servers and expose Harn pipelines as MCP servers.
description: Use for MCP client connections, server definitions, inbound roots/sampling/elicitation, and deterministic MCP fixtures.
when_to_use: Use when calling an MCP server from Harn, exposing tools or resources over MCP, or testing an MCP integration without network access.
---

# Build MCP clients and servers in Harn

Use this skill when a Harn program talks to an MCP server or serves one. Pair
it with [[harn-testing]] for deterministic fixtures, [[harn-agent]] for tool
exposure inside an agent loop, and [[harn-providers]] when sampling routes back
to a model.

Maintainers changing Harn's own MCP implementation should read
`docs/src/dev/mcp-maintenance.md` instead. This skill covers writing MCP
clients and servers in Harn, not editing the runtime that carries them.

## Start here

- `docs/src/mcp-and-acp.md` is the public client and server reference.
- `docs/src/mcp-server.md` covers the orchestrator server, `harn mcp serve`.
- Harn implements stable MCP `2026-07-28` on both transports.
- The official Rust SDK owns protocol mechanics; Harn owns product policy.
- Route every capability through `harness.tools.*`.

## Connect as a client

Connect explicitly when the server is chosen at runtime:

```harn
const client = harness.tools.mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])
const info = harness.tools.mcp_server_info(client)
const tools = harness.tools.mcp_list_tools(client)
const content = harness.tools.mcp_call(client, "read_file", {path: "/tmp/data.txt"})
harness.tools.mcp_disconnect(client)
```

Resources and prompts use `mcp_list_resources`, `mcp_read_resource`,
`mcp_list_resource_templates`, `mcp_list_prompts`, and `mcp_get_prompt`.

- `mcp_call` returns a string for a single text result, a list of content dicts
  for multiple blocks, or nil when empty.
- `mcp_call` throws when the tool reports an error. Catch it where the pipeline
  can recover; do not let a remote failure read as an empty result.
- Disconnect what you connect. Prefer declared servers when the set is fixed.

## Declare servers instead of connecting by hand

Put a fixed server set in `harn.toml` under `[[mcp]]` with `name` plus either
`command` and `args` for stdio or `transport = "http"` and `url`. Declared
servers connect before the pipeline runs and appear in the global `mcp` dict.

- Mark a server `lazy = true` when most runs never call it. It boots on the
  first `mcp_call`, `mcp_ensure_active("name")`, or skill activation that names
  it in `requires_mcp`.
- Declare the server once. Do not also `mcp_connect` the same target.

## Resolve inbound requests

A stable server asks the client for work by returning `input_required` with
embedded requests. Harn resolves them and retries the operation with
`inputResponses`. Harn advertises roots, form and URL elicitation, sampling,
and the stable tasks extension.

- Roots resolve from script and project roots, also readable through
  `harness.tools.mcp_roots()`.
- Sampling dispatches to the host bridge as `capability="mcp"`,
  `operation="sample"`. Approved requests route to `llm_call`. Without
  approval, Harn returns a structured decline; treat that as a normal outcome.
- Elicitation dispatches as `capability="mcp"`, `operation="elicit"` in form and
  URL modes. Harn never prefetches or opens an elicited URL.
- Long tool calls poll through `tasks/get`. Do not add a wait loop of your own.

Install fixtures before the call that triggers the request, not after the run
starts failing. A harness may install them after `mcp_connect` returns; the
client reads fixture state when the inbound request arrives.

## Serve a Harn pipeline over MCP

```harn
pipeline main(harness: Harness) {
  let tools = tool_registry()
  tools = tool_define(tools, "greet", "Greet someone", {
    parameters: {name: "string"},
    handler: { args -> "Hello, ${args.name}!" },
    annotations: {title: "Greeter", readOnlyHint: true},
  })
  harness.tools.mcp_tools(tools)
}
```

Add `harness.tools.mcp_resource(...)`, `harness.tools.mcp_resource_template(...)`,
and `harness.tools.mcp_prompt(...)` for the other surfaces. Run the script with
`harn serve mcp agent.harn`. Use `composition_mcp_tools()` from
`std/composition` for the governed Code Mode profile.

- List endpoints paginate at 100 entries and return `nextCursor`. Override with
  `HARN_MCP_LIST_PAGE_SIZE`.
- `print` and `println` go to stderr when stdio is the transport. Never write
  protocol output to stdout yourself.
- Out-of-scope features fail with `error.data.type = "mcp.unsupportedFeature"`.
- Set `readOnlyHint` and `destructiveHint` honestly. Clients gate approval on
  them.

## Test without a network

Use capability fixtures rather than a live server:

```harn
harness.testing.clear()
harness.testing.respond("mcp", "elicit", {action: "accept", content: {env: "staging"}})
```

- Assert the recorded dispatch through `harness.testing.calls()`, checking
  `capability`, `operation`, and `params`.
- Drive the model side with `with_llm_script` from `std/testing`.
- Mutate the behavior under test and confirm the test fails. A green MCP test
  that never reached the server proves nothing.
- Do not add sleeps or wall-clock polling. See [[harn-testing]].

## Review checklist

- Does every connected client get disconnected or declared?
- Does a tool error reach the caller as an error?
- Are sampling and elicitation declines handled as outcomes?
- Do tool annotations match what the handler actually does?
- Does the change need `docs/src/mcp-and-acp.md` updated in the same patch?
- Is there a fixture-backed test that fails when the behavior breaks?

## Verify

- Script checks: `harn check`, `make fmt-harn`, `make lint-harn`.
- Protocol gates: `make mcp-conformance`, `make protocol-conformance`.
- Docs examples: `make check-docs-snippets`.
- Broader runtime changes: `make test`.
