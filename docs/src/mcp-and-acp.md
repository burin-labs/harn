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
const client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

const info = mcp_server_info(client)
log("Connected to: ${info.name}")
```

`mcp_server_info(...)` also exposes the raw initialize response and an
`instructions` string when the server supplied one. `agent_loop(...,
{mcp_servers: [...]})` can include those instructions as advisory context in
the Harn-built system prompt; set `mcp_initialize_advisory: false` when a
harness wants to keep server-provided advice out of model context.

### Listing and calling tools

```harn
const tools = mcp_list_tools(client)
for t in tools {
  log("${t.name}: ${t.description}")
}

const content = mcp_call(client, "read_file", {path: "/tmp/data.txt"})
log(content)
```

`mcp_call` returns a string for single-text results, a list of content
dicts for multi-block results, or nil when empty. If the tool reports an
error, `mcp_call` throws.

### Experimental file inputs

Harn implements [SEP-2356][mcp-file-sep] (the leading draft, not the older
multipart upload proposal upstream closed) behind an explicit runtime
opt-in: tool schemas mark file fields with `x-mcp-file`, and clients pass
the selected file inline as an RFC 2397 `data:` URI.

```harn
harn.mcp.configure({
  experimental: {
    file_upload: {
      spec_revision: "modelcontextprotocol/modelcontextprotocol#2356",
    },
  },
})

const client = mcp_connect("python3", ["./image-server.py"])
const image = harn.mcp.upload_file(client, "photo.png", {
  accept: ["image/png", "image/jpeg"],
  max_size: 5242880,
})
const result = mcp_call(client, "describe_image", {image: image})
log(result)
```

`harn.mcp.upload_file(...)` reads a local file under Harn's filesystem
policy, validates optional `accept` / `max_size` hints, and returns
`data:<media-type>;base64,...`. Harn redacts `data:` URI payloads from
replay keys and server-side diagnostics; scripts should still avoid
printing them.

When Harn serves an MCP tool, use `harn.mcp.file_input(...)` in a
`tool_define` parameter schema. The MCP server validates incoming `data:` URIs
against the declared media-type and size constraints before invoking the
handler.

```harn
let tools = tool_registry()

tools = tool_define(tools, "inspect_upload", "Inspect a small text file", {
  parameters: {
    upload: harn.mcp.file_input({
      accept: ["text/*"],
      max_size: 64,
      description: "Small text file to inspect",
    }),
  },
  handler: { args -> "received " + args.upload },
})

mcp_tools(tools)
```

Because SEP-2356 is still draft, the wire shape can change. The opt-in records
the implemented proposal revision so experimental users have a clear cutover
point when MCP ratifies file input support.

[mcp-file-sep]: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2356

### Resources and prompts

```harn
const resources = mcp_list_resources(client)
const data = mcp_read_resource(client, "file:///tmp/config.json")

const prompts = mcp_list_prompts(client)
const prompt = mcp_get_prompt(client, "review", {code: "fn main() {}"})
```

### MCP client support matrix

Harn's standard MCP client prefers protocol version `2025-11-25` and can fall
back to `2025-06-18` when a server only advertises that version. RC mode also
understands Harn's `DRAFT-2026-v1` profile when a server exposes it through
`server/discover`. Harn advertises the `elicitation`, `sampling`, and `roots`
client capabilities. It answers server `roots/list` requests with the resolved
project roots for the active script and emits
`notifications/roots/list_changed` when that root snapshot changes. It does not
advertise task capabilities, so servers should treat MCP tasks as unavailable
when connected to Harn.

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

[[mcp]]
name = "modern"
transport = "http"
url = "https://example.com/mcp"
protocol_mode = "rc"
protocol_version = "DRAFT-2026-v1"
```

`protocol_mode = "rc"` opts a server into Harn's draft MCP client profile.
RC mode probes stdio servers with `server/discover`, sends protocol/client
metadata on every request, uses stateless Streamable HTTP headers instead of
requiring `MCP-Session-Id`, consumes cache hints from list/read calls, mirrors
valid `x-mcp-header` tool-schema annotations into `Mcp-Param-*` HTTP headers,
and resolves `input_required` tool results for roots, elicitation, and
sampling before retrying the call. Omit `protocol_mode` for the legacy
`2025-11-25` initialize/session behavior.

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
const client = mcp_ensure_active("github")
const issues = mcp_call(client, "list_issues", {repo: "burin-labs/harn"})

// Release when done — lets the registry shut it down.
mcp_release("github")

// Inspect current state.
const status = mcp_registry_status()
for s in status {
  log("${s.name}: lazy=${s.lazy} active=${s.active} refs=${s.ref_count}")
}
```

### Server cards (MCP v2.1)

A Server Card is a JSON document that advertises a server's identity,
capabilities, and tool catalog without requiring a connection. Harn
consumes cards for discoverability and can publish its own when running
as an MCP server.

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
const card = mcp_server_card("notion")
log(card.description)
for t in card.tools {
  log("- ${t.name}")
}

// Or pass a URL / path directly.
const card = mcp_server_card("./agents/my-agent-card.json")
```

Cards are cached in-process with a 5-minute TTL, so repeated calls skip
the remote fetch. Skill matchers can factor card metadata into scoring
without paying connection cost.

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
  const tools = mcp_list_tools(mcp.filesystem)
  const content = mcp_call(mcp.filesystem, "read_file", {path: "/tmp/data.txt"})
  log(content)
}
```

If a server fails to connect, a warning is printed to stderr and that
server is omitted from the `mcp` dict. Other servers still connect
normally.

For HTTP MCP servers, Harn can reuse OAuth tokens stored with the CLI:

```bash
harn mcp discover https://www.notion.com --json
harn mcp redirect-uri
harn mcp login notion
harn mcp status notion
```

OAuth client authentication uses one `auth` table. With no explicit
configuration, `harn mcp login` prefers Client ID Metadata Document (CIMD)
registration using Harn's published metadata document at
`https://harnlang.com/.well-known/oauth-client.json`; if the authorization
server does not advertise CIMD support, Harn falls back to dynamic client
registration.

For a pre-registered client, store the client secret with `harn connect
api-key` and reference it from `auth.client_secret_id`:

```bash
harn connect api-key \
  --connector internal-mcp \
  --secret-id mcp/internal-client-secret
```

```toml
[[mcp]]
name = "internal"
transport = "http"
url = "https://mcp.example.com"
auth = { mode = "byo", client_id = "registered-client", client_secret_id = "mcp/internal-client-secret", scopes = "read:docs write:docs" }
```

For a static bearer token or API key, reuse the same secret-store path:

```bash
harn connect api-key \
  --connector internal-mcp \
  --secret-id mcp/internal-bearer
```

```toml
[[mcp]]
name = "internal"
transport = "http"
url = "https://mcp.example.com"
auth = { mode = "static", secret_id = "mcp/internal-bearer" }
```

The older top-level `client_id`, `client_secret`, `scopes`, and `auth_token`
fields are accepted for local manifests, but new host-generated config should
write the `auth = { mode = ... }` form so TUI and GUI clients do not need their
own OAuth configuration model.

### HTTP discovery and OAuth debugging

`harn mcp discover <url>` fetches the unofficial
`/.well-known/mcp.json` descriptor from the URL's origin and prints the
Streamable HTTP endpoint when one is published. This is intentionally separate
from OAuth discovery: use it to find where to connect, then let normal MCP
OAuth protected-resource metadata discover where to authorize.

```bash
harn mcp discover https://www.notion.com
harn mcp discover https://www.notion.com --json
```

For remote OAuth, Harn owns the whole browser flow, token exchange, token
storage, and refresh path. Tokens are keyed by `(resource, issuer, client_id)`
and all Harn surfaces share the same store, so a token minted by
`harn mcp login` is the token reused by `harn run`, GUI hosts, and ACP bridge
requests. Refresh is single-flight across threads and processes: Harn takes an
in-process mutex plus a file lock, reloads the token inside the lock, and only
one caller sends the refresh request. This matters for authorization servers
that rotate refresh tokens and revoke the grant when an old refresh token is
reused.

Refresh-token behavior is intentionally conservative:

- If refresh succeeds and the server returns a new refresh token, Harn stores
  the rotated token before releasing the lock.
- If refresh fails with a transient network/server error, Harn keeps the stored
  token so a later call can retry.
- If refresh fails with OAuth `invalid_grant`, Harn treats it as terminal
  because it may indicate revocation or refresh-token reuse detection. Harn
  deletes the stored token and active-client index, then reports that
  re-authorization is required.
- Token endpoint response bodies are never printed in diagnostics. Harn keeps
  the OAuth error code, HTTP status, and omitted body length so logs remain
  useful without leaking credentials.

Useful checks when debugging a remote server:

```bash
harn mcp discover https://server.example --json
harn mcp status server-name --json
harn mcp logout server-name
harn mcp login server-name
harn mcp login --reauth --only server-name
```

When a harness needs custom auth policy, keep secrets and token refresh in
Harn's OAuth substrate and customize the manifest-level `auth` mode, scopes,
client metadata, or static bearer secret. Harn scripts should not implement
their own refresh-token storage loops unless they are deliberately connecting
to a non-MCP service through a custom integration.

The split is deliberate. Rust owns the host-security substrate: protocol
negotiation, HTTP auth, token exchange, keyring writes, file locks, redaction,
and generated host bindings. Harn code should own orchestration policy above
that layer: which MCP servers a harness uses, when a lazy server starts,
which scopes a workflow asks for, how diagnostics are presented to an operator,
and what fallback path an agent takes after `auth_required`.

### Example: filesystem MCP server

A complete example connecting to the filesystem MCP server, writing a
file, and reading it back:

```harn
const client = mcp_connect("npx", ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])

mcp_call(client, "write_file", {path: "/tmp/hello.txt", content: "Hello from Harn!"})
const content = mcp_call(client, "read_file", {path: "/tmp/hello.txt"})
log(content)

const entries = mcp_call(client, "list_directory", {path: "/tmp"})
log(entries)

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
  let tools = tool_registry()

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

This exposes `harn.code.search_examples`, `harn.code.generate_harn_api`, and
`harn.code.execute_composition`. The executor still runs through Harn's
read-only composition path and returns the reduced snippet result by default
instead of an opaque "execute code" result. Hybrid servers can call
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
The server supports the `2025-11-25` and `2025-06-18` MCP protocol versions on
both transports, plus Harn's draft profile where advertised.

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
| `roots/list` | Supported outbound from script-driven handlers through `mcp_client_roots()` / `harn.mcp.client_roots()` |
| `sampling/createMessage` | Server-initiated sampling against the connected client is not currently emitted by the orchestrator-mode catalog; Harn declares the `sampling` capability when acting as a client (see the [client matrix](#mcp-client-support-matrix)). |
| `elicitation/create` | Supported outbound from script-driven handlers via `mcp_elicit(...)`; inbound client requests to the server are rejected with an explicit unsupported-feature error |
| `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel` | Supported for task-augmented orchestrator tool calls |
| `tools/call` with `params.task` | Supported for tools that advertise optional task execution; rejected with `-32602` for non-taskable tools |

#### Publishing a server card

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
runtime backend. Communication is JSON-RPC 2.0 over stdin/stdout or
WebSocket text frames.

Bridge-level tool gates and daemon idle/resume notifications are documented in
[Bridge protocol](./bridge-protocol.md).

### Running the ACP server

```bash
harn serve acp pipeline.harn      # execute a specific pipeline per prompt
harn serve acp --transport websocket --bind 127.0.0.1:8789 pipeline.harn
```

The packaged ACP adapter is exposed through `harn serve acp`. Pass
`--api-key <key>` or set `HARN_SERVE_API_KEY` to require clients to complete
ACP `authenticate` before creating sessions. Harn advertises the configured
methods in `initialize.authMethods`; unauthenticated protected requests return
ACP `auth_required` with the same method details. WebSocket clients may also
send the key as `Authorization: Bearer <key>` or `X-API-Key` during the
upgrade; header authentication pre-authorizes that ACP connection while
headerless clients can still use the in-band ACP `authenticate` method.

### Protocol overview

The ACP server supports these JSON-RPC methods:

| Method | Description |
|---|---|
| `initialize` | Handshake with capabilities |
| `authenticate` | Authenticate the ACP connection when `authMethods` is non-empty |
| `session/new` | Create a new session (returns session ID) |
| `session/load` | Replay persisted session events and reattach a live in-process session |
| `session/resume` | Resume a live or loaded session without replay notifications |
| `session/fork` | Fork an existing session into an independent branch |
| `session/list` | List active sessions known to the ACP adapter |
| `session/prompt` | Send a prompt to the agent for execution |
| `session/inject` | Accept a pending user message for a running turn and return its `messageId` |
| `session/revoke_inject` | Revoke a pending injected user message before delivery |
| `session/replace_inject` | Replace pending injected content without changing its queue position |
| `session/remind` | Queue a typed system reminder for the active agent loop |
| `session/pending_injections` | Inspect pending bridge user-message and reminder injections |
| `session/revoke_reminder` | Revoke a pending bridge reminder before delivery |
| `session/cancel` | Cancel the currently running prompt |
| `session/cancel_tool_call` | Cancel one in-flight tool call without tearing down the session |
| `session/truncate` | Truncate the session transcript to a requested prefix |
| `session/rollback` | Roll back the last completed prompt turn, including tracked filesystem pre-images |
| `session/redo` | Reapply the most recent rollback while the redo stack is still valid |
| `session/close` | Close a session and cancel any active prompt |
| `session/stop` | Deprecated alias for `session/close` |
| `session/set_mode` | Switch the active session mode |
| `session/set_config_option` | Switch a preferred ACP session config option (`mode`, `model`, `thought_level`, or `budget`) |
| `harn.session_workspace_roots` | Return the live session's primary workspace anchor and mounted roots |
| `harn.session_add_root` | Mount an additional root into the live session |
| `harn.session_reanchor` | Replace the live session's primary workspace anchor |
| `harn.session_view.query` | Return the stable `harn.session_view.v1` projection for a live or persisted session |
| `harn.session_timeline.query` | Return the Harn-owned redacted session timeline snapshot |
| `harn.session_timeline.subscribe` | Subscribe to newly appended timeline events |
| `harn.session_timeline.unsubscribe` | Stop a timeline subscription |
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

During `initialize`, Harn mirrors the public `agentCapabilities` object from
upstream ACP: `loadSession`, `promptCapabilities`, `mcpCapabilities`,
`session.inject`, and the canonical `sessionCapabilities.close` and
`sessionCapabilities.list` flags are advertised in their upstream locations.
Harn advertises `session.inject.modes = ["queue", "steer"]` and
`session.inject.pending.replace = true`. Harn also advertises
`session.remind.modes = ["interrupt_immediate", "finish_step", "audit_only"]`
and `session.remind.pending = {list: true, revoke: true}` for host-side
system-reminder queue controls. Harn advertises
`sessionCapabilities.rollback` and `sessionCapabilities.redo` for the
completed-turn checkpoint stack; both methods return typed success/failure
statuses and reject while a prompt is active. Harn-only methods such as
`session/fork` remain documented extensions instead of being inserted into
upstream `sessionCapabilities`.

`session/rollback` and `session/redo` are Harn-owned session primitives. Harn
moves the transcript to the checkpoint boundary and, when hostlib filesystem
snapshots are available, the ACP adapter restores the matching file pre-images
in the same request. Rollback captures redo filesystem snapshots before
restoring the older state. Redo is invalidated by a new transcript mutation,
workspace reanchor/root change, staged filesystem commit/discard, or direct
tool-call restore.

Example response:

```json
{
  "status": "rolled_back",
  "checkpointId": "turn_...",
  "beforeMessageCount": 1,
  "afterMessageCount": 2,
  "fsSnapshotIds": ["tc_42"],
  "redoFsSnapshotIds": ["turn_...:redo:tc_42"],
  "fsRestores": [
    {
      "snapshotId": "tc_42",
      "restoredPaths": ["notes.md"],
      "skippedPathsWithReasons": []
    }
  ]
}
```

### Session workspace anchors

The Harn workspace-anchor extension exposes the same live VM session state used
by `agent_session_workspace_anchor`, `agent_session_add_root`, and
`agent_session_reanchor`. All three methods require `sessionId`/`session_id`
and return the current `workspaceAnchor`.

`harn.session_workspace_roots` is a read path that also seeds a missing anchor
from the live session's `cwd`, which preserves compatibility with sessions
opened before first-class anchors were attached.

```json
{
  "method": "harn.session_workspace_roots",
  "params": { "sessionId": "sess_abc" }
}
```

`harn.session_add_root` accepts `path` (or `root`), optional `mountMode`
(`read_only`, `extend`, or `sandboxed`), and optional `reason`. It validates
that the mounted path exists and is readable before mutating the session,
records the canonical `RootMounted` transcript event, and clears session-local
permission grants.

```json
{
  "method": "harn.session_add_root",
  "params": {
    "sessionId": "sess_abc",
    "path": "/workspace/tools",
    "mountMode": "extend"
  }
}
```

`harn.session_reanchor` accepts `path` (or `primary`) plus optional `reason`,
`carryTranscript`, and `compact`. The live ACP path currently supports the
default `carryTranscript: true` and `compact: false` shape; richer fork/compact
handoffs still belong to `agent_session_reanchor` until ACP has a host-visible
session handoff envelope.

```json
{
  "method": "harn.session_reanchor",
  "params": {
    "sessionId": "sess_abc",
    "path": "/workspace/next"
  }
}
```

### Stable session and run views

Use `harn.session_view.query` when an ACP client needs a durable projection
instead of private persisted run-record fields. The query accepts
`sessionId`/`session_id` and optional `runPath`/`run_path` or `runId`/`run_id`;
the response schema is `harn.session_view.v1`. A single persisted run can also
be inspected as `harn.run_view.v1` with the CLI:

```bash
harn runs view --json .harn-runs/<run>.json
harn runs view --json --session .harn-runs/
```

Rust embedders get the same facade through `EmbeddedAgentClient`:
`start_run(...)`, `load_run(...)`, `resume_run(...)`, `send_user_input(...)`,
`cancel_session(...)`, `subscribe_session_events(...)`, `session_view(...)`,
and `run_view_from_path(...)` all use the ACP/server projection paths instead
of private record parsing.

`send_user_input(...)` decodes Harn's optional
`result._meta.harn.terminal` extension into the typed prompt result. For
agent-loop prompts, embedders should report completion only when
`terminal.kind` is `natural`; `stopReason: end_turn` can also accompany policy
or unknown stops.

### Session timeline extension

`harn.session_timeline.query` projects existing Harn observability records into
one client-facing timeline shape. The query accepts `sessionId`/`session_id`,
optional `runId`/`run_id`, optional `runPath`/`run_path`, optional
`projectId`/`project_id`, optional `fromCursor`, and optional `limit`. The
response includes:

- `schemaVersion`
- `cursor.topics`, a per-event-log-topic cursor map for later queries
- `nodes`, ordered timeline entries with stable `id`, `parentId`, `children`,
  `category`, `kind`, `status`, redacted `attributes`, source `references`, and
  causal `links`

Span nodes come from run trace spans when a run record is supplied to the VM API
or when ACP can load a persisted run record from `runPath` or from `runId` in
the default Harn run directory. ACP queries also use the active event log and
include session agent events plus channel lifecycle/audit events. Channel match
nodes link back to the matching channel emit node through
`links.kind = "channel_emit"`. Batched channel match nodes also include
`links.kind = "channel_batch_member"` entries for the recorded constituent event
ids.

```json
{
  "method": "harn.session_timeline.query",
  "params": {
    "sessionId": "sess_abc",
    "runId": "run_123",
    "fromCursor": {
      "topics": {
        "observability.agent_events.sess_abc": 42
      }
    }
  }
}
```

`harn.session_timeline.subscribe` accepts the same query fields plus an optional
`subscriptionId`. It returns `{subscriptionId, updateMethod}` and then emits
`harn.session_timeline.update` notifications:

```json
{
  "method": "harn.session_timeline.update",
  "params": {
    "subscriptionId": "timeline-1",
    "update": {
      "schemaVersion": 1,
      "cursor": {"topics": {"observability.agent_events.sess_abc": 43}},
      "node": {"id": "event:observability.agent_events.sess_abc:43"}
    }
  }
}
```

Use `harn.session_timeline.unsubscribe` with `subscriptionId` to stop the live
stream. Closing a session also cancels subscriptions scoped to that session.

### Session forking

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

### Session modes

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
        { "value": "claude-sonnet-5", "name": "claude-sonnet-5 (anthropic/claude-sonnet-5)", "description": "tier: frontier" }
      ]
    },
    {
      "id": "thought_level",
      "name": "Thought Level",
      "category": "model",
      "type": "select",
      "currentValue": "@inherit",
      "options": [
        { "value": "@inherit", "name": "Inherit script default", "description": "..." },
        { "value": "auto",     "name": "Auto",   "description": "..." },
        { "value": "off",      "name": "Off",    "description": "..." },
        { "value": "minimal",  "name": "Minimal", "description": "..." },
        { "value": "low",      "name": "Low",    "description": "..." },
        { "value": "medium",   "name": "Medium", "description": "..." },
        { "value": "high",     "name": "High",   "description": "..." },
        { "value": "xhigh",    "name": "Extra High", "description": "..." }
      ]
    },
    {
      "id": "budget",
      "name": "Call Budget",
      "category": "_harn_budget",
      "type": "select",
      "currentValue": "@inherit",
      "options": [
        { "value": "@inherit", "name": "Inherit server default", "description": "..." },
        { "value": "off", "name": "No session budget", "description": "..." },
        { "value": "{\"llm_cost_usd\":0.05,\"llm_tokens\":200000}", "name": "$0.05 and 200k tokens", "description": "..." }
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

### Session model pin

`session/set_config_option(configId="model")` pins an LLM model selector on
the session so subsequent `llm_call` invocations without an explicit
`model:` option resolve to the pinned value instead of the
`HARN_LLM_MODEL` / providers.toml default. This is the wire surface
behind editor `/model` commands (Crush, OpenCode `<leader>m`, Codex
`/model`) and replaces the "the change takes effect after `/clear`"
workaround.

ACP clients can request the full picker source of truth with the Harn
extension method `_harn/providerCatalog`. The result is the provider
catalog v5 artifact shape, normalized through the runtime's effective
overlays. Clients should use that
catalog for model display names, aliases, auth env names, local/cloud
classification, context windows, tool-support hints, availability, and
pricing, family dimensions, and presets instead of vendoring picker rows.
Consumers that need dispatchable
provider/model route data should read `routing_routes` rather than rebuilding
that join from provider and model rows.

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "session/set_config_option",
  "params": {
    "sessionId": "sess_abc",
    "configId": "model",
    "value": "claude-sonnet-5"
  }
}
```

Accepted selector forms:

- a known alias from the llm.toml catalog (e.g. `claude-sonnet-5`)
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

### Session thought level

`session/set_config_option(configId="thought_level")` pins Harn's
provider-aware reasoning policy on the session. The value is intentionally
higher-level than any single provider API: Harn lowers it to the selected
route's native thinking representation before the provider call.

```json
{
  "jsonrpc": "2.0",
  "id": 43,
  "method": "session/set_config_option",
  "params": {
    "sessionId": "sess_abc",
    "configId": "thought_level",
    "value": "high"
  }
}
```

Accepted values are `auto`, `off`, `minimal`, `low`, `medium`, `high`,
and `xhigh`; `none`, `disabled`, `no_think`, and `nothink` are accepted
aliases for `off`. Setting `"@inherit"` clears the pin.

Lowering examples:

- OpenAI reasoning routes receive `reasoning_effort` / typed
  `thinking: {mode: "effort", level: ...}` where supported.
- Gemini routes receive typed Harn thinking that the provider adapter lowers
  to `thinkingBudget`, dynamic thinking, or the supported compatibility shape.
- Anthropic routes use adaptive thinking where a model requires it and
  budgeted extended thinking where that is still supported.
- Qwen-style local/open-compatible routes use `thinking: {mode: "disabled"}`
  for `off`, which triggers Harn's capability-driven `/no_think` injection
  or `chat_template_kwargs.enable_thinking=false` where the transport
  supports it.

Per-call options still win: `llm_call(..., {thinking: ...})` and
`llm_call(..., {effort: ...})` bypass the session thought pin.
`session/fork` carries the parent's thought pin to the child, and later
changes remain branch-local.

### Session budget

`AcpServerConfig::with_budget(BudgetSpec)` installs an inherited resource
budget for every ACP session. The guard is applied around each
`session/prompt` turn on the same runtime thread that executes the VM, using
the same `BudgetSpec`/`call_budget` path as HTTP `@budget(...)` dispatch.
Use `with_llm_cost_budget(...)` or `with_llm_token_budget(...)` when the
embedder only needs an LLM-specific ceiling.

Clients can re-arm or disable the session budget without restarting the
server by setting `configId: "budget"`:

```json
{
  "jsonrpc": "2.0",
  "id": 44,
  "method": "session/set_config_option",
  "params": {
    "sessionId": "sess_abc",
    "configId": "budget",
    "value": "{\"llm_cost_usd\":0.05,\"llm_tokens\":200000}"
  }
}
```

The value is a string because ACP config options are string selectors. Harn
accepts `@inherit` to use the server default, `off` to disable the session
budget, or a compact JSON object with any of `llm_cost_usd`, `llm_tokens`,
`mcp_calls`, and `pg_queries`. Exhaustion uses the existing budget error and
agent-event path, so hosts that already render `_harn/agentEvent`
`budget_exhausted` updates do not need a new event type.

### Queued user messages and reminders during agent execution

ACP hosts can inject user follow-up messages while an agent is running.
Harn owns the delivery semantics inside the runtime so product apps do
not need to reimplement queue/orchestration logic. Clients call
`session/inject`, which responds as soon as Harn accepts the pending message
and returns an agent-owned `messageId`. Delivery is later echoed as a
`session/update` notification with `sessionUpdate: "user_message"` and the
same `messageId`. User-message methods remain user-role only; ambient or
system-style context uses `session/remind`.

Relevant methods and notifications:

- `session/inject`
- `session/revoke_inject`
- `session/replace_inject`
- `session/remind`
- `session/pending_injections`
- `session/revoke_reminder`
- `session/update` with `worker_update` content for delegated worker lifecycle events

Pending inject payload shape:

```json
{
  "sessionId": "sess_abc",
  "mode": "steer",
  "content": [
    {
      "type": "text",
      "text": "Please stop editing that file and explain first."
    }
  ]
}
```

`mode: "steer"` delivers at the next safe operation boundary. `mode: "queue"`
delivers at end-of-interaction. While the message is pending, callers can
revoke it with `session/revoke_inject` or replace only its content with
`session/replace_inject`; replacement preserves the original `messageId`, mode,
and queue position. Revoke is idempotent for known revoked ids. Revoke or
replace after delivery returns an `already_delivered` error, and unknown ids
return `unknown_message_id`.

Reminder payload shape:

```json
{
  "body": "The host noticed new repository instructions; re-read AGENTS.md.",
  "tags": ["host_context"],
  "dedupe_key": "repo-instructions",
  "ttl_turns": 2,
  "role_hint": "system",
  "mode": "finish_step",
  "_meta": {
    "harn": {
      "source": "workspace-index"
    }
  }
}
```

Reminder `mode` values:

- `interrupt_immediate`
- `finish_step`
- `audit_only`

Pending inject runtime behavior:

- `steer`: deliver after the current operation finishes.
- `queue`: deliver at the end of the current interaction.
- Pending injects are session-scoped and survive `session/cancel` and
  suspend/resume bridge replacement; clients that want to clear them should
  revoke them before cancelling.
- Pending, revoked, and pre-replacement content never enter transcript replay.
  Replay shows only delivered `user_message` updates.

Reminder runtime behavior:

- `session/remind`: queues a typed system reminder instead of a user-role
  message. Malformed reminder payloads are rejected with `HARN-RMD-002`.
  `session/inject` never creates reminders.
- `interrupt_immediate`: drain at the next eligible bridge checkpoint,
  including `pre_tool_dispatch` when the reminder arrives between LLM output
  and tool dispatch.
- `finish_step`: drain at the next iteration boundary so the model sees the
  reminder on the next prompt build.
- `audit_only`: drain at `loop_exit` for transcript audit only; no later model
  call sees it.
- `session/pending_injections`: returns `{pendingCount, injections}` for the
  bridge queue. Rows are FIFO ordered and include `kind: "user"` or
  `kind: "reminder"` plus stable `messageId` / `reminderId` identifiers.
- `session/revoke_reminder`: removes a queued reminder by `reminderId` before
  a checkpoint drains it. Revoke is idempotent for known revoked ids; races
  after delivery return `already_delivered`, and unknown ids return
  `unknown_reminder_id`.
- `session/cancel_tool_call`: abort one in-flight tool call (e.g. user
  clicked stop on a runaway `git push --force`) without closing the
  session. Params: `sessionId`, `toolCallId`, optional `reason`
  (surfaced to the model), and optional `injectReminder: bool = true`
  (queues a system reminder so the model knows it was stopped by the
  host). The response shape is
  `{status: "cancelled" | "already_cancelled" | "not_found", callId, tool}`.
  The cancelled call returns to the loop as a `status: "cancelled"`
  tool_result so the model can distinguish "the host stopped me" from
  "the tool errored." Pairs with the in-VM `cancel_in_flight_tool_call`
  Harn builtin — both surfaces share the registry.
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
- expect HTTP MCP servers to publish RFC 9728 protected-resource metadata and
  OAuth/OIDC authorization-server metadata; Harn validates issuer binding before
  storing or refreshing tokens
- use local environment variables or client-managed secrets for stdio MCP
  servers; the HTTP OAuth discovery flow does not apply to local stdio launches
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

Delegated peers can include an RFC 8693 actor chain in
`message.metadata.actor_chain`. Harn validates that shape, stores it on the A2A
task metadata, and appends the served agent as the current `act` hop before
executing the exported Harn function. Scripts can inspect the resulting chain
with `agent_session_actor_chain()`.

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
