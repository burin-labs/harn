# MCP and ACP integration

Harn has built-in support for the Model Context Protocol (MCP), Agent
Client Protocol (ACP), and Agent-to-Agent (A2A) protocol. This guide
covers how to use each from both client and server perspectives.

For a scan-friendly status table across all protocol entry points, see the
[protocol support matrix](./protocol-support.md). For shared serving internals,
see [Outbound workflow server](./harn-serve.md). Harn-owned ACP/MCP extension
fields are specified in [Harn ACP/MCP extensions v1](./spec/harn-extensions/v1.md).

## MCP client (connecting to MCP servers)

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

`mcp_server_info(...)` also exposes the raw initialize response and an
`instructions` string when the server supplied one. `agent_loop(...,
{mcp_servers: [...]})` can include those instructions as advisory context in
the Harn-built system prompt; set `mcp_initialize_advisory: false` when a
harness wants to keep server-provided advice out of model context.

### Listing and calling tools

```harn
let tools = mcp_list_tools(client)
for t in tools {
  println("${t.name}: ${t.description}")
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

### MCP client support matrix

Harn's MCP client negotiates protocol version `2025-11-25` and advertises
the `elicitation`, `sampling`, and `roots` client capabilities. It answers
server `roots/list` requests with the resolved project roots for the active
script and emits `notifications/roots/list_changed` when that root snapshot
changes. It does not advertise task capabilities, so servers should treat MCP
tasks as unavailable when connected to Harn.

| Method or feature | Harn as MCP client |
|---|---|
| `initialize`, `notifications/initialized` | Supported |
| `ping` | Supported when Harn sends it to a server |
| `tools/list`, `tools/call` | Supported through `mcp_list_tools` and `mcp_call` |
| `resources/list`, `resources/read`, `resources/templates/list` | Supported through resource builtins |
| `prompts/list`, `prompts/get` | Supported through prompt builtins |
| `completion/complete` | Not exposed as a Harn builtin |
| `roots/list` | Supported; Harn advertises `roots.listChanged`, serves resolved script/project roots, and exposes the same data through `harn.mcp.roots()` / `mcp_roots()` |
| `sampling/createMessage` | Supported; Harn advertises sampling and dispatches inbound requests to the host bridge (`capability="mcp"`, `operation="sample"`). Approved requests route to Harn's `llm_call` and return `{role, content, model, stopReason}` to the originating server. Without an installed bridge, requests are declined with a structured `mcp.samplingDeclined` error so servers can fall back gracefully. |
| `elicitation/create` | Supported; Harn advertises elicitation and dispatches inbound requests to the host bridge (`capability="mcp"`, `operation="elicit"`) |
| MCP task methods and task-augmented requests | Unsupported; Harn does not advertise task support |

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

### Lazy boot

Servers marked `lazy = true` are NOT booted at pipeline startup. They
start on the first `mcp_call`, `mcp_ensure_active("name")`, or skill
activation that declares the server in `requires_mcp`. This keeps cold
starts fast when many servers are declared but only a few are needed
per run.

```toml
[[mcp]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
lazy = true
keep_alive_ms = 30_000   # keep the process alive 30s after last release

[[mcp]]
name = "datadog"
command = "datadog-mcp"
lazy = true
```

**Ref-counting**: each skill activation or explicit
`mcp_ensure_active(name)` call bumps a binder count. On deactivation or
`mcp_release(name)`, the count drops. When it reaches zero, Harn
disconnects the server — immediately if `keep_alive_ms` is absent, or
after the window elapses if set.

Explicit control from user code:

```harn
// Start the lazy server and hold it open.
let client = mcp_ensure_active("github")
let issues = mcp_call(client, "list_issues", {repo: "burin-labs/harn"})

// Release when done — lets the registry shut it down.
mcp_release("github")

// Inspect current state.
let status = mcp_registry_status()
for s in status {
  println("${s.name}: lazy=${s.lazy} active=${s.active} refs=${s.ref_count}")
}
```

### Server Cards (MCP v2.1)

A Server Card is a small JSON document that advertises a server's
identity, capabilities, and tool catalog **without requiring a
connection**. Harn consumes cards for discoverability and can publish
its own when running as an MCP server.

Declare a card source in `harn.toml`:

```toml
[[mcp]]
name = "notion"
transport = "http"
url = "https://mcp.notion.com/mcp"
card = "https://mcp.notion.com/.well-known/mcp-card"

[[mcp]]
name = "local-agent"
command = "my-agent"
lazy = true
card = "./agents/my-agent-card.json"
```

Fetch it from a pipeline:

```harn,ignore
// Look up by registered server name.
let card = mcp_server_card("notion")
println(card.description)
for t in card.tools {
  println("- ${t.name}")
}

// Or pass a URL / path directly.
let card = mcp_server_card("./agents/my-agent-card.json")
```

Cards are cached in-process with a 5-minute TTL — repeated calls are
free. Skill matchers can factor card metadata into scoring without
paying connection cost.

### Skill-scoped MCP binding

Skills can declare the MCP servers they need via `requires_mcp` (or the
equivalent `mcp`) frontmatter field. On activation, Harn ensures every
listed server is running; on deactivation, it releases them.

```harn,ignore
skill github_triage {
  description: "Triage GitHub issues and cut fixes",
  when_to_use: "User mentions a GitHub issue or PR by number",
  requires_mcp: ["github"],
  allowed_tools: ["list_issues", "create_pr", "add_comment"],
  prompt: "You are a triage assistant...",
}
```

When `agent_loop` activates `github_triage`, the lazy `github` MCP
server boots (if configured that way) and its process stays alive for
as long as the skill is active. When the skill deactivates, the server
is released — and if no other skill holds it, the process shuts down
(respecting `keep_alive_ms`).

Transcript events emitted along the way: `skill_mcp_bound`,
`skill_mcp_unbound`, `skill_mcp_bind_failed`.

### MCP tools in the tool-search index

When an LLM uses `tool_search` (progressive tool disclosure), MCP tools
are auto-tagged with both `mcp:<server>` and `<server>` in the BM25
corpus. That means a query like `"github"` or `"mcp:github"` surfaces
every tool from that server even when the tool's own name and
description don't contain the word. Tools returned by `mcp_list_tools`
carry an `_mcp_server` field that the indexer consumes automatically —
no extra wiring needed.

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

## MCP server (exposing Harn as an MCP server)

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
    parameters: {name: "string"},
    handler: { args -> "Hello, ${args.name}!" }
  })

  tools = tool_define(tools, "search", "Search files", {
    parameters: {query: "string"},
    handler: { args -> "results for ${args.query}" },
    annotations: {
      title: "File Search",
      readOnlyHint: true,
      destructiveHint: false
    }
  })

  mcp_tools(tools)
}
```

For large connector packages, use the compact governed Code Mode profile from
`std/composition`:

```harn
import { composition_mcp_tools } from "std/composition"

pipeline main(task) {
  mcp_tools(composition_mcp_tools())
}
```

This exposes `harn.code.search_examples` and
`harn.code.execute_composition`. The executor still runs through Harn's
read-only composition path and returns child binding calls/results rather than
an opaque "execute code" result. Hybrid servers can call
`composition_mcp_tools(existing_registry)` to expose ordinary tools and the
compact Code Mode profile together.

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
    handler: { args -> "value for ${args.key}" }
  })

  // Prompt
  mcp_prompt({
    name: "review",
    description: "Code review prompt",
    arguments: [{name: "code", required: true}],
    handler: { args -> "Please review:\n${args.code}" }
  })
}
```

### Running as an MCP server

```bash
harn serve mcp agent.harn
```

`harn serve mcp` auto-detects whether the script exposes its surface
through `pub fn` exports or through the `mcp_tools(...)` /
`mcp_resource(...)` / `mcp_prompt(...)` registration builtins shown
above and serves the appropriate one over stdio or Streamable HTTP. All
`print`/`println` output goes to stderr when stdio is the MCP transport.
The server supports the `2025-11-25` MCP protocol version on both
transports.

List endpoints are cursor-paginated. `tools/list`, `resources/list`,
`resources/templates/list`, and `prompts/list` return up to 100 entries by
default and include `nextCursor` when another page is available. Set
`HARN_MCP_LIST_PAGE_SIZE` to a positive integer to change the per-page limit.

### MCP server support matrix

Harn's MCP servers implement the core tool/resource/prompt path and explicitly
reject latest-spec features that are out of scope for this release with a
JSON-RPC error containing `error.data.type = "mcp.unsupportedFeature"`.
Servers advertise `resources.subscribe` when resources are present. The
orchestrator MCP server wires `harn://topic/*` resources to EventLog-backed
`notifications/resources/updated`; script-driven servers accept subscriptions
for registered resource URIs.

| Method or feature | Harn as MCP server |
|---|---|
| `initialize`, `notifications/initialized`, `ping` | Supported |
| `logging/setLevel` | Accepted |
| `tools/list`, `tools/call` | Supported |
| `notifications/progress`, `notifications/cancelled` | Supported for long-running tool calls |
| `resources/list`, `resources/read`, `resources/templates/list` | Supported for registered entries, cards, package context, prompt sources, and orchestrator EventLog resources |
| `resources/subscribe`, `resources/unsubscribe` | Supported for registered resource URIs; orchestrator topic resources emit update notifications |
| `prompts/list`, `prompts/get` | Supported for registered prompts |
| `completion/complete` | Supported for prompt arguments and resource template arguments |
| `roots/list` | Explicitly unsupported; client-side roots are not served by Harn |
| `sampling/createMessage` | Server-initiated sampling against the connected client is not currently emitted by the orchestrator-mode catalog; Harn declares the `sampling` capability when acting as a client (see the [client matrix](#mcp-client-support-matrix)). |
| `elicitation/create` | Supported outbound from script-driven handlers via `mcp_elicit(...)`; inbound client requests to the server are rejected with an explicit unsupported-feature error |
| `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` | Supported for task-augmented orchestrator tool calls |
| `tools/call` with `params.task` | Supported for tools that advertise optional task execution; rejected with `-32602` for non-taskable tools |

#### Publishing a Server Card

Attach a Server Card so clients can discover your server's identity and
capabilities before connecting:

```bash
harn serve mcp agent.harn --card ./card.json
```

The card JSON is embedded in the `initialize` response's
`serverInfo.card` field and also exposed as a read-only resource at
`well-known://mcp-card`. Minimal shape:

```json
{
  "name": "my-agent",
  "version": "1.0.0",
  "description": "Short one-line summary shown in pickers.",
  "protocolVersion": "2025-11-25",
  "capabilities": { "tools": true, "resources": false, "prompts": false },
  "tools": [
    {"name": "greet", "description": "Greet someone by name"}
  ]
}
```

`--card` also accepts an inline JSON string for ad-hoc publishing:
`--card '{"name":"demo","description":"…"}'`.

### Configuring in Claude Desktop

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "my-agent": {
      "command": "harn",
      "args": ["serve", "mcp", "agent.harn"]
    }
  }
}
```

## ACP (Agent Client Protocol)

ACP lets host applications and local clients use Harn as a
runtime backend. Communication is JSON-RPC 2.0 over stdin/stdout.

Bridge-level tool gates and daemon idle/resume notifications are documented in
[Bridge protocol](./bridge-protocol.md).

### Running the ACP server

```bash
harn serve acp pipeline.harn      # execute a specific pipeline per prompt
```

The packaged ACP adapter is exposed through `harn serve acp`. Pass
`--api-key <key>` or set `HARN_SERVE_API_KEY` to require clients to complete
ACP `authenticate` before creating sessions. Harn advertises the configured
methods in `initialize.authMethods`; unauthenticated protected requests return
ACP `auth_required` with the same method details.

### Protocol overview

The ACP server supports these JSON-RPC methods:

| Method | Description |
|---|---|
| `initialize` | Handshake with capabilities |
| `authenticate` | Authenticate the ACP connection when `authMethods` is non-empty |
| `session/new` | Create a new session (returns session ID) |
| `session/fork` | Fork an existing session into an independent branch |
| `session/list` | List active sessions known to the ACP adapter |
| `session/prompt` | Send a prompt to the agent for execution |
| `session/cancel` | Cancel the currently running prompt |
| `session/truncate` | Truncate the session transcript to a requested prefix |
| `session/close` | Close a session and cancel any active prompt |
| `session/stop` | Deprecated alias for `session/close` |
| `session/set_mode` | Switch the active session mode |
| `session/set_config_option` | Switch a preferred ACP session config option (`mode` or `model`) |
| `workflow/signal` | Enqueue a workflow signal message in the current session workspace |
| `workflow/query` | Read a named workflow query value from the current session workspace |
| `workflow/update` | Send a workflow update request and wait for a response |
| `workflow/pause` | Mark a workflow paused and enqueue a control message |
| `workflow/resume` | Mark a workflow resumed and enqueue a control message |

`workflow/*` methods also accept the `harn.workflow.*` aliases. They expect
`workflowId`, plus `name` where applicable, optional `payload`, and
`timeoutMs` for `workflow/update`. These methods resolve workflow state against
the session's configured working directory, so they operate on the same durable
`.harn/workflows/<workflowId>/state.json` tree that the in-language builtins
use.

During `initialize`, Harn keeps the public `agentCapabilities` object aligned
with upstream ACP: `loadSession`, `promptCapabilities`, `mcpCapabilities`, and
the canonical `sessionCapabilities.close` and `sessionCapabilities.list` flags
are advertised in their upstream locations. Harn-only methods such as
`session/fork` remain documented extensions instead of being inserted into
upstream `sessionCapabilities`.

### Session Forking

`session/fork` promotes Harn's runtime transcript branching to a host-visible
ACP method. The request shape is:

```json
{
  "session_id": "sess_parent",
  "keep_first": 3,
  "id": "sess_branch",
  "branch_name": "left"
}
```

- `session_id` is required and identifies the source session to fork.
- `keep_first` is optional; when present Harn uses
  `agent_session_fork_at(session_id, keep_first, id?)`.
- Without `keep_first`, Harn uses `agent_session_fork(session_id, id?)`.
- `id` is optional; when omitted Harn mints a fresh session id.
- `branch_name` is optional session metadata that Harn mirrors into the
  forked session's title and `_meta.branch_name`.

Successful responses return the new branch id plus fork metadata:

```json
{
  "sessionId": "sess_branch",
  "state": "forked",
  "parent_id": "sess_parent",
  "branched_at": 3
}
```

When a fork is created, Harn also emits a `session/update` notification with
`sessionUpdate: "session_info_update"` and `_meta.state = "forked"` so ACP
hosts can render branch-aware session UIs without scraping text output. The
forked session gets its own stream; subscriber sinks and in-flight prompt state
are not copied from the parent.

### Session Modes

Harn exposes ACP
[session config options](https://agentclientprotocol.com/protocol/session-config-options)
and the legacy
[session-modes](https://agentclientprotocol.com/protocol/session-modes) field
from the same mode catalog. Clients that understand `configOptions` should use
it; Harn keeps `modes` available for clients still on `session/set_mode`.
`session/new` and `session/load` return both shapes:

```json
{
  "configOptions": [
    {
      "id": "mode",
      "name": "Session Mode",
      "category": "mode",
      "type": "select",
      "currentValue": "ask",
      "options": [
        { "value": "ask",       "name": "Ask",       "description": "..." },
        { "value": "architect", "name": "Architect", "description": "..." },
        { "value": "code",      "name": "Code",      "description": "..." },
        { "value": "shadow",    "name": "Shadow",    "description": "..." }
      ]
    },
    {
      "id": "model",
      "name": "LLM Model",
      "category": "model",
      "type": "select",
      "currentValue": "@inherit",
      "options": [
        { "value": "@inherit",         "name": "Inherit ambient default", "description": "..." },
        { "value": "claude-sonnet-4-6", "name": "claude-sonnet-4-6 (anthropic/claude-sonnet-4-6)", "description": "tier: frontier" }
      ]
    }
  ],
  "modes": {
    "currentModeId": "ask",
    "availableModes": [
      { "id": "ask",       "name": "Ask",       "description": "..." },
      { "id": "architect", "name": "Architect", "description": "..." },
      { "id": "code",      "name": "Code",      "description": "..." },
      { "id": "shadow",    "name": "Shadow",    "description": "..." }
    ]
  }
}
```

Mode semantics:

- `ask` — conservative default, mapped to Harn's `act_with_approval`
  autonomy tier.
- `architect` — read-only planning mode. Builtins that mutate the workspace,
  execute processes, or hit the network are rejected by the VM policy gate
  before they run. Maps to Harn's `suggest` autonomy tier.
- `code` — full tool access. Harn leaves host/runtime capability resolution
  authoritative instead of installing an extra ceiling. Maps to `act_auto`.
- `shadow` — proposal-only mode mapped to Harn's `shadow` autonomy tier.
  Side effects are blocked before they touch the workspace.

Preferred mode switching:

```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "method": "session/set_config_option",
  "params": { "sessionId": "sess_abc", "configId": "mode", "value": "architect" }
}
```

Legacy mode switching:

```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "method": "session/set_mode",
  "params": { "sessionId": "sess_abc", "modeId": "architect" }
}
```

`session/set_config_option` responds with the complete `configOptions` state.
Both switching methods emit `session/update` notifications with
`sessionUpdate: "current_mode_update"` and
`sessionUpdate: "config_option_update"` when the selected mode changes.
Re-setting the same mode is a no-op (ack only, no notification).
`session/fork` carries the parent's active mode over to the new branch and
echoes it back on the fork response.

The selected mode is enforced for the next `session/prompt`: while the
prompt runs, the matching capability policy is pushed onto the VM
execution stack and popped when the prompt completes. Harn derives that
policy from the same `AutonomyTier` machinery used by trigger dispatch, so
ACP sessions and runtime autonomy stay aligned. The conformance case
`acp_architect_mode_blocks_destructive_writes_in_prompt` locks this behavior
end-to-end.

### Session Model Pin

`session/set_config_option(configId="model")` pins an LLM model selector on
the session so subsequent `llm_call` invocations without an explicit
`model:` option resolve to the pinned value instead of the
`HARN_LLM_MODEL` / providers.toml default. This is the wire surface
behind editor `/model` commands (Crush, OpenCode `<leader>m`, Codex
`/model`) and replaces the "the change takes effect after `/clear`"
workaround.

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "session/set_config_option",
  "params": {
    "sessionId": "sess_abc",
    "configId": "model",
    "value": "claude-sonnet-4-6"
  }
}
```

Accepted selector forms:

- a known alias from the llm.toml catalog (e.g. `claude-sonnet-4-6`)
- an explicit `provider:model` or `provider/model` pair where the
  provider is in `providers.toml`
- a model id present in the catalog (`model_catalog_entry`)

Setting the value to the sentinel `"@inherit"` (also accepted: empty
string) clears the pin and reverts the session to the ambient default.
The ACP wire surface is intentionally curated; scripts that need
ad-hoc selectors still pass `model:` directly to `llm_call`.

Per-call options always win over the pin — invoking
`llm_call(..., {model: "..."})` ignores the session pin so prompts that
deliberately route to a different model aren't silently rebound. The
existing conversation buffer, transcript metadata, and memory context
are untouched by a model swap; only the resolver for the next prompt's
default model changes. Pinning rejects unregistered providers with a
`-32602` error tagged `invalid_model`.

Notification semantics match `mode`: a `session/update` with
`sessionUpdate: "config_option_update"` is emitted whenever the pin
actually changes (re-pinning to the same selector is a silent ack).
`session/fork` carries the parent's pin over to the new branch
(matching how `tool_format` and the session-level system prompt
inherit), so a branch starts with the same effective model as the
parent. Setting a different pin on the child stays local.

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
- Bridge-mode logs also stream boot timing records (`ACP_BOOT` with
  `compile_ms`, `vm_setup_ms`, `vm_baseline_cache`, and `execute_ms`) and live
  `span_end` duration events while a prompt is still running, so hosts do not
  need to wait for the final stdout flush to surface basic timing telemetry.
  File-backed prompts keep the compile cache and VM baseline cache separate:
  repeated turns reuse stable stdlib/project/source setup, then instantiate a
  clean execution VM for prompt globals, output, bridge handles, tasks,
  cancellation, sync primitives, shared state, and tracing.
- `finish_step`: inject after the current tool/operation completes
- `wait_for_completion`: defer until the current agent interaction yields

### Typed pipeline returns (Harn → ACP boundary)

Pipelines are what produce ACP events (`agent_message_chunk`,
`tool_call`, `tool_call_update`, `plan`, `sessionUpdate`). Declaring a
return type on a pipeline turns the Harn→ACP boundary into a
type-checked contract instead of an implicit shape that only the bridge
validates:

```harn
type PipelineResult = {
  text: string | nil,
  events: list<dict> | nil,
}

pub pipeline ghost_text(task) -> PipelineResult {
  return {
    text: "hello",
    events: [],
  }
}
```

The type checker verifies every `return <expr>` against the declared
type, so drift between pipeline output and bridge expectation is caught
before the Swift/TypeScript bridge ever sees the message.

Public pipelines without an explicit return type emit the
`pipeline-return-type` lint warning. Explicit return types on the
Harn→ACP boundary will be required in a future release; the warning is
a one-release deprecation window.

Well-known entry pipelines (`default`, `main`, `auto`, `test`) are
exempt from the warning because their return value is host-driven, not
consumed by a protocol bridge.

Canonical ACP envelope types are provided as Harn type aliases in
`std/acp` — `SessionUpdate`, `AgentMessageChunk`, `ToolCall`,
`ToolCallUpdate`, `Plan`, and `Handoff` — and can be used directly as pipeline
return types so a pipeline's contract matches the ACP schema
byte-for-byte.

`std/plan` provides Harn-owned `emit_plan` and `update_plan` tools plus a
`harn.plan.v1` artifact shape. ACP keeps emitting the standard
`sessionUpdate: "plan"` `entries` array; when the source is a Harn plan
artifact, the update also carries `harnPlan` with the normalized steps,
assumptions, open questions, verification commands, and approval state.

When a workflow emits a typed handoff artifact, ACP also mirrors it as a
structured `session/update` with `sessionUpdate: "handoff"`, so hosts can show
handoff lifecycle entries without scraping transcript prose.

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

A2A exposes exported Harn functions as a peer-agent HTTP server that other
agents can interact with. The server implements A2A protocol version 0.3.0 and
uses the shared `harn-serve` dispatch core.

### Running the server

```bash
harn serve a2a agent.harn             # explicit A2A
harn serve agent.harn                 # legacy shorthand for A2A
harn serve a2a --port 3000 agent.harn
```

### Agent card

The server publishes an A2A AgentCard at
`GET /.well-known/agent-card.json`, with compatibility aliases at
`GET /.well-known/a2a-agent`, `GET /.well-known/agent.json`, and
`GET /agent/card`. The card advertises each exported `pub fn` as an A2A skill
through `preferredTransport`, `additionalInterfaces`, default input/output
modes, capabilities, and security declarations. Set
`--card-signing-secret` or `HARN_SERVE_A2A_CARD_SECRET` to attach an HS256
signature envelope to the card.

### Task submission

Submit a task with a JSON-RPC request:

```text
POST /
Content-Type: application/json

{
  "jsonrpc": "2.0",
  "id": "task-1",
  "method": "message/send",
  "params": {
    "function": "triage",
    "configuration": {"blocking": true},
    "message": {
      "role": "user",
      "parts": [{"type": "text", "text": "Analyze this codebase"}]
    }
  }
}
```

Use `message/send` with `configuration.returnImmediately = true` for an
asynchronous task, `configuration.blocking = true` for a blocking task, and
`message/stream` or `tasks/resubscribe` for SSE task updates. Harn preserves
the legacy `a2a.*`, `tasks/send`, and `tasks/send_and_wait` names for one minor
cycle and marks those HTTP responses with a `Deprecation` header. If the served
file exports exactly one function, the function selector can be omitted.

### Canonical HTTP+JSON/REST binding

The same task lifecycle is also exposed over the canonical A2A 0.3.0
HTTP+JSON/REST binding under `/v1`. The agent card advertises this
transport in `additionalInterfaces` alongside the JSON-RPC entry, e.g.

```json
"additionalInterfaces": [
  {"url": "https://agent.example", "transport": "JSONRPC"},
  {"url": "https://agent.example/v1", "transport": "HTTP+JSON"}
]
```

REST endpoints (per the spec's AIP-136 custom-method form):

| Method                                             | Path                                                       | Body                          |
| -------------------------------------------------- | ---------------------------------------------------------- | ----------------------------- |
| `POST`                                             | `/v1/message:send`                                         | `MessageSendParams`           |
| `POST`                                             | `/v1/message:stream`                                       | `MessageSendParams` (returns SSE) |
| `GET`                                              | `/v1/tasks/{id}`                                           | —                             |
| `POST`                                             | `/v1/tasks/{id}:cancel`                                    | —                             |
| `POST`                                             | `/v1/tasks/{id}:subscribe`                                 | — (returns SSE)               |
| `POST`                                             | `/v1/tasks/{id}/pushNotificationConfigs`                   | `PushNotificationConfig`      |
| `GET`                                              | `/v1/tasks/{id}/pushNotificationConfigs`                   | —                             |
| `GET`                                              | `/v1/tasks/{id}/pushNotificationConfigs/{configId}`        | —                             |
| `DELETE`                                           | `/v1/tasks/{id}/pushNotificationConfigs/{configId}`        | —                             |
| `GET`                                              | `/v1/card`                                                 | — (authenticated extended card) |

REST responses return the bare result payload (e.g. a `Task` object)
rather than a JSON-RPC envelope. Errors are returned with an HTTP 4xx/5xx
status and a JSON-RPC error body. The legacy non-canonical paths
(`/message/send`, `/message/stream`, `/tasks/send`, `/tasks/send_and_wait`,
`/tasks/cancel`, `/tasks/resubscribe`) keep working for one minor cycle
and emit a `Deprecation: true` header plus a `Warning: 299 …` advisory
that points to the canonical replacement.

### Task status

Check the status of a submitted task:

```text
POST /
Content-Type: application/json

{"jsonrpc":"2.0","id":"get-1","method":"tasks/get","params":{"id":"<task-id>"}}
```

Task states follow the A2A 0.3.0 lifecycle:

- **Active**: `submitted`, `working`.
- **Pause states (non-terminal)**: `input-required` is set while a HITL
  primitive (`ask_user`, `request_approval`, `dual_control`,
  `escalate_to`) is suspended on a waitpoint; the SSE stream carries a
  paired `hitl` event with the request payload, then flips back to
  `working` when the response arrives. `auth-required` is set when a
  downstream call inside the script raises an auth-classified error
  (e.g. an HTTP 401 from an LLM provider); the client is expected to
  re-authenticate and resubscribe.
- **Terminal**: `completed`, `failed`, `cancelled`, `rejected`.
  `rejected` is returned synchronously when the dispatch core's
  `AuthPolicy` denies the caller before any script work runs (the
  caller cannot recover by retrying with the same credentials —
  policy/credentials must change).

Completed task payloads also include `metadata.handoff_ids` and
`metadata.handoffs` when the served function returned typed handoff artifacts,
so remote personas can consume the handoff artifact directly instead of
replaying the source transcript.

### Vendor workflow control methods

In addition to the standard task lifecycle calls, Harn's A2A adapter accepts
vendor JSON-RPC methods for durable workflow control:

- `a2a.WorkflowSignal` or `harn.workflow.signal`
- `a2a.WorkflowQuery` or `harn.workflow.query`
- `a2a.WorkflowUpdate` or `harn.workflow.update`
- `a2a.WorkflowPause` or `harn.workflow.pause`
- `a2a.WorkflowResume` or `harn.workflow.resume`

These methods expect `workflowId`, optional `name`, optional `payload`, and
`timeoutMs` for updates. They resolve workflow state relative to the served
pipeline's workspace, which makes them compatible with the same persisted
workflow mailbox used by the Harn builtins and ACP surface.
