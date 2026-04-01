# MCP and ACP Integration

Harn has built-in support for the Model Context Protocol (MCP), Agent
Client Protocol (ACP), and Agent-to-Agent (A2A) protocol. This guide
covers how to use each from both client and server perspectives.

## MCP Client (Connecting to MCP Servers)

Connect to any MCP-compatible tool server, list its capabilities, and
call tools from within a Harn program. Harn supports both stdio MCP
servers and remote HTTP MCP servers.

### Connecting manually

Use `mcp_connect` to spawn an MCP server process and perform the
initialize handshake:

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

let info = mcp_server_info(client)
println("Connected to: ${info.name}")
```

### Listing and calling tools

```harn
let tools = mcp_list_tools(client)
for tool in tools {
  println("${tool.name}: ${tool.description}")
}

let content = mcp_call(client, "read_file", {path: "/tmp/data.txt"})
println(content)
```

`mcp_call` returns a string for single-text results, a list of content
dicts for multi-block results, or nil when empty. If the tool reports an
error, `mcp_call` throws.

### Resources and prompts

```harn
let resources = mcp_list_resources(client)
let data = mcp_read_resource(client, "file:///tmp/config.json")

let prompts = mcp_list_prompts(client)
let prompt = mcp_get_prompt(client, "review", {code: "fn main() {}"})
```

### Disconnecting

```harn
mcp_disconnect(client)
```

### Auto-connection via harn.toml

Instead of calling `mcp_connect` manually, declare servers in `harn.toml`.
They connect automatically before the pipeline executes and are available
through the global `mcp` dict:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[[mcp]]
name = "notion"
transport = "http"
url = "https://mcp.notion.com/mcp"
scopes = "read write"
```

Use them in your pipeline:

```harn
pipeline default(task) {
  let tools = mcp_list_tools(mcp.filesystem)
  let content = mcp_call(mcp.filesystem, "read_file", {path: "/tmp/data.txt"})
  println(content)
}
```

If a server fails to connect, a warning is printed to stderr and that
server is omitted from the `mcp` dict. Other servers still connect
normally.

For HTTP MCP servers, Harn can reuse OAuth tokens stored with the CLI:

```bash
harn mcp redirect-uri
harn mcp login notion
```

If the server uses a pre-registered OAuth client, you can provide those
values in `harn.toml` or on the CLI:

```toml
[[mcp]]
name = "internal"
transport = "http"
url = "https://mcp.example.com"
client_id = "https://client.example.com/metadata.json"
client_secret = "super-secret"
scopes = "read:docs write:docs"
```

When no `client_id` is provided, Harn will attempt dynamic client
registration if the authorization server advertises it.

### Example: filesystem MCP server

A complete example connecting to the filesystem MCP server, writing a
file, and reading it back:

```harn
let client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

mcp_call(client, "write_file", {path: "/tmp/hello.txt", content: "Hello from Harn!"})
let content = mcp_call(client, "read_file", {path: "/tmp/hello.txt"})
println(content)

let entries = mcp_call(client, "list_directory", {path: "/tmp"})
println(entries)

mcp_disconnect(client)
```

## MCP Server (Exposing Harn as an MCP Server)

Harn pipelines can expose tools, resources, resource templates, and
prompts as an MCP server. This lets Claude Desktop, Cursor, or any MCP
client call into your Harn code.

### Defining tools

Use `tool_registry()` and `tool_define()` to create tools, then register
them with `mcp_tools()`:

```harn
pipeline main(task) {
  var tools = tool_registry()

  tools = tool_define(tools, "greet", "Greet someone", {
    params: {name: "string"},
    handler: { args -> "Hello, " + args.name + "!" }
  })

  tools = tool_define(tools, "search", "Search files", {
    params: {query: "string"},
    handler: { args -> "results for " + args.query },
    annotations: {
      title: "File Search",
      readOnlyHint: true,
      destructiveHint: false
    }
  })

  mcp_tools(tools)
}
```

### Defining resources and prompts

```harn
pipeline main(task) {
  // Static resource
  mcp_resource({
    uri: "docs://readme",
    name: "README",
    text: "# My Agent\nA demo MCP server."
  })

  // Dynamic resource template
  mcp_resource_template({
    uri_template: "config://{key}",
    name: "Config Values",
    handler: { args -> "value for " + args.key }
  })

  // Prompt
  mcp_prompt({
    name: "review",
    description: "Code review prompt",
    arguments: [{name: "code", required: true}],
    handler: { args -> "Please review:\n" + args.code }
  })
}
```

### Running as an MCP server

```bash
harn mcp-serve agent.harn
```

All `print`/`println` output goes to stderr (stdout is the MCP
transport). The server supports the `2024-11-05` MCP protocol version
over stdio.

### Configuring in Claude Desktop

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "my-agent": {
      "command": "harn",
      "args": ["mcp-serve", "agent.harn"]
    }
  }
}
```

## ACP (Agent Client Protocol)

ACP lets editors, IDEs, and CLIs use Harn as a
coding agent backend. Communication is JSON-RPC 2.0 over stdin/stdout.

Bridge-level tool gates and daemon idle/resume notifications are documented in
[Bridge Protocol](./bridge-protocol.md).

### Running the ACP server

```bash
harn acp                    # no pipeline, uses bridge mode
harn acp pipeline.harn      # execute a specific pipeline per prompt
```

### Protocol overview

The ACP server supports these JSON-RPC methods:

| Method | Description |
|---|---|
| `initialize` | Handshake with capabilities |
| `session/new` | Create a new session (returns session ID) |
| `session/prompt` | Send a prompt to the agent for execution |
| `session/cancel` | Cancel the currently running prompt |

### Queued user messages during agent execution

ACP hosts can inject user follow-up messages while an agent is running.
Harn owns the delivery semantics inside the runtime so product apps do
not need to reimplement queue/orchestration logic.

Supported notification methods:

- `user_message`
- `session/input`
- `agent/user_message`
- `session/update` with `worker_update` content for delegated worker lifecycle events

Payload shape:

```json
{
  "content": "Please stop editing that file and explain first.",
  "mode": "interrupt_immediate"
}
```

Supported `mode` values:

- `interrupt_immediate`
- `finish_step`
- `wait_for_completion`

Runtime behavior:

- `interrupt_immediate`: inject on the next agent loop boundary immediately
- Worker lifecycle updates are emitted as structured `session/update` payloads with
  worker id/name, status, lineage metadata, artifact counts, transcript presence,
  snapshot path, execution metadata, child run ids/paths, lifecycle summaries,
  and audit-session metadata when applicable.
  Hosts can render these as background task notifications instead of scraping
  stdout.
- `finish_step`: inject after the current tool/operation completes
- `wait_for_completion`: defer until the current agent interaction yields

## Security notes

### Remote MCP OAuth

`harn mcp login` stores remote MCP OAuth tokens in the local OS keychain for
standalone CLI reuse. Treat that as durable delegated access:

- prefer the narrowest scopes the server supports
- treat configured `client_secret` values as secrets
- review remote MCP capabilities before wiring them into autonomous workflows

### Safer write defaults

Harn now propagates mutation-session audit metadata through workflow runs,
delegated workers, and bridge tool gates. Recommended host defaults remain:

- proposal-first application for direct workspace edits
- worktree-backed execution for autonomous/background workers
- explicit approval for destructive or broad-scope mutation tools

### Bridge mode

ACP internally uses Harn's host bridge so the host can retain control over
tool execution while Harn still owns agent/runtime orchestration.

Unknown builtins are delegated to the host via `builtin_call` JSON-RPC
requests. This enables the host to provide filesystem access, editor
integration, or other capabilities that Harn code can call as regular
builtins.

## A2A (Agent-to-Agent Protocol)

A2A exposes a Harn pipeline as an HTTP server that other agents can
interact with.

### Running the server

```bash
harn serve agent.harn               # default port 8080
harn serve --port 3000 agent.harn   # custom port
```

### Agent card

The server publishes an agent card at `GET /.well-known/agent.json`
describing the agent's capabilities. MCP clients and other A2A agents
use this to discover the agent.

### Task submission

Submit a task with a POST request:

```text
POST /message/send
Content-Type: application/json

{
  "message": {
    "role": "user",
    "parts": [{"type": "text", "text": "Analyze this codebase"}]
  }
}
```

### Task status

Check the status of a submitted task:

```text
GET /task/get?id=<task-id>
```

Task states follow the A2A protocol lifecycle: `submitted`, `working`,
`completed`, `failed`, `cancelled`.
