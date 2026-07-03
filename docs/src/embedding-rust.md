# Embedding Harn in Rust

`harn-serve` exposes the same ACP agent loop used by `harn serve acp` as a
Rust API. Use it when a host application wants Harn in-process instead of
spawning the CLI as a child process.

## Add the crate

Until Harn is published on crates.io, depend on the repository tag you ship
against:

```toml
[dependencies]
harn-serve = { git = "https://github.com/burin-labs/harn", tag = "v0.9.6" }
serde_json = "1"
tokio = { version = "1", features = ["rt", "sync"] }
```

Harn uses deep generated and macro-expanded types. Embedders should match the
crate setting:

```rust
#![recursion_limit = "256"]
```

## Choosing a feature set

In-process embedding links `harn-serve` -> `harn-hostlib` -> `harn-vm`
directly into the host binary, so the host pays the compile and link cost of
whatever those crates pull in. `harn-serve` ships lean by default and lets the
embedder opt into the heavyweight families it actually needs:

| `harn-serve` feature | Adds | Use for |
| --- | --- | --- |
| *(default)* | Nothing beyond the core agent loop | Smallest build; host issues no hostlib tool calls and no Postgres queries |
| `hostlib` | hostlib deterministic tools (file I/O, search, process, secret store) — **no** tree-sitter or grammars | Lightweight smoke evals; agent edits via text patches only |
| `hostlib-full` | `hostlib` **+** the full code-intelligence surface (tree-sitter + all ~27 grammar families) | Parity-critical evals that exercise AST-precise edits, rename, the symbol index |
| `vm-postgres` | The VM's `pg.*` builtins (`sqlx-postgres` and its ~130 transitive crates) | Hosts whose scripts talk to Postgres |
| `vm-sqlite` | The VM's `sqlite_*` builtins | Hosts whose scripts inspect or write local SQLite databases |
| `full` | `hostlib-full` + `vm-postgres` + `vm-sqlite` | One-flag CLI parity |

The split exists because the code-intelligence grammars are C-compiled crates:
without it, every in-process client — including a tiny smoke eval — compiled
all ~27 grammars and the entire sqlx tree before the first transcript could
start.

**Burin's Rust TUI** should select per eval tier:

- **Parity-critical evals** (anything that may invoke AST-precise edits,
  `rename_symbol`, or the code index) → `features = ["full"]` (or
  `["hostlib-full"]` if the eval never touches Postgres). This matches
  `harn-cli` exactly.
- **Lightweight smoke tests** (cutover gate, startup checks, evals that only
  read/write whole files) → default, or `["hostlib"]` if the script names a
  deterministic tool builtin.

```toml
# Parity-critical eval harness
harn-serve = { git = "...", tag = "v0.9.6", features = ["full"] }

# Lean smoke-test harness
harn-serve = { git = "...", tag = "v0.9.6", features = ["hostlib"] }
```

Finer control is available one layer down: `harn-hostlib` exposes per-family
grammar features (`grammar-web`, `grammar-systems`, `grammar-scripting`,
`grammar-jvm`, `grammar-enterprise`, `grammar-data`) so a client that only ever
edits, say, TypeScript and Python can compile just those grammars. A language
whose family is not compiled in still parses its name and extension; its
AST-precise edits simply degrade to the text fallback.
`scripts/measure_lean_embedding.sh` reports the dependency delta between the
lean and full configurations and gates against regressions in CI.

Note: Cargo unifies features across a build graph, so any binary that also
depends on `harn-cli` (which enables the full set) gets the full surface
regardless of what it requests from `harn-serve`. The lean configurations only
take effect in builds that do **not** pull in the full CLI.

## Start an embedded agent

`EmbeddedAgentClient` owns the required worker thread, current-thread Tokio
runtime, ACP JSON-RPC channels, and event demux. Use it when a Rust host wants
the stable lifecycle facade instead of hand-routing JSON-RPC lines:

```rust
#![recursion_limit = "256"]

use harn_serve::{
    AcpServerConfig, AcpSessionNewParams, AcpSessionPromptParams,
    EmbeddedAgentClient, EmbeddedAgentEvent,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = EmbeddedAgentClient::spawn(AcpServerConfig::new(None)).await?;
    let session = agent.start_run(AcpSessionNewParams::cwd(".")).await?;
    let view = agent.session_view(session.session_id.clone()).await?;
    assert_eq!(view.schema, "harn.session_view.v1");

    let _prompt_id = agent.begin_user_input(AcpSessionPromptParams::text(
        session.session_id,
        "Summarize this workspace.",
    ))?;
    loop {
        match agent.next_event().await? {
            EmbeddedAgentEvent::HostRequest { id, method, params, .. } => {
                // Answer approval/auth/capability callbacks through the same
                // JSON-RPC channel. Shape `result` to the callback method.
                let result = approve_callback(&method, params)?;
                agent.respond_to_host_request(id, result)?;
            }
            EmbeddedAgentEvent::SessionUpdate { update, .. } => {
                render_update(update);
            }
            EmbeddedAgentEvent::RequestCompleted { result, .. } => {
                let _prompt_result = result;
                break;
            }
            EmbeddedAgentEvent::RequestFailed { error, .. } => {
                return Err(std::io::Error::other(error.message).into());
            }
            _ => {}
        }
    }

    agent.shutdown();
    agent.join()?;
    Ok(())
}

# fn approve_callback(
#     _method: &str,
#     _params: serde_json::Value,
# ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
#     Ok(serde_json::json!({}))
# }
# fn render_update(_update: serde_json::Value) {}
```

The ACP channel server is `!Send` because it owns a `LocalSet` and uses
`spawn_local`. `EmbeddedAgentClient` keeps that detail out of the host: the
server future is built and driven entirely on the dedicated worker thread,
while the host gets typed lifecycle calls, `harn.session_view.v1` /
`harn.run_view.v1` helpers, and a single event stream. Use
`EmbeddedAgentClient::load_run(...)` / `resume_run(...)` to reattach persisted
sessions, `subscribe_session_events(...)` for live timeline updates, and
`run_view_from_path(...)` to inspect a persisted run without reading private
record internals.

## Send typed ACP requests

Use `AcpJsonRpcRequest` plus the request param structs instead of hand-building
JSON:

```rust
use harn_serve::{
    AcpContentBlock, AcpJsonRpcRequest, AcpSessionInjectMode,
    AcpSessionInjectParams, AcpSessionPromptParams,
};

let prompt = AcpJsonRpcRequest::session_prompt(
    2,
    AcpSessionPromptParams::text("session-id", "Summarize this workspace."),
)
.into_json_value()
.expect("prompt request serializes");

let inject = AcpJsonRpcRequest::session_inject(
    3,
    AcpSessionInjectParams::new(
        "session-id",
        AcpSessionInjectMode::Steer,
        vec![AcpContentBlock::text("Stop after the current tool call.")],
    ),
)
.into_json_value()
.expect("inject request serializes");
```

The typed helpers preserve ACP wire names such as `sessionId`, `messageId`, and
`toolCallId`, so serialized values can be sent directly to `EmbeddedAgent`,
stdio ACP, or ACP WebSocket.

## Own the runtime yourself

Use `run_acp_channel_server_with_handle` when the host already owns a dedicated
thread and current-thread Tokio runtime:

```rust
use harn_serve::{run_acp_channel_server_with_handle, AcpServerConfig};
use tokio::sync::mpsc;

let (request_tx, request_rx) = mpsc::unbounded_channel();
let (response_tx, response_rx) = mpsc::unbounded_channel();
let (server, handle) =
    run_acp_channel_server_with_handle(AcpServerConfig::new(None), request_rx, response_tx);

let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("start ACP runtime");
let control = handle.clone();
control.shutdown();
runtime.block_on(server);
```

`AcpChannelHandle::wait_ready()` resolves once the loop can receive requests.
`shutdown()` asks the loop to drain and stop, and `wait_terminated()` resolves
after the loop exits.

## Custom output sinks

Advanced embedders that already run on a compatible runtime can drive
`AcpServer` directly and provide their own output sink:

```rust
use harn_serve::{AcpOutput, AcpServer, AcpServerConfig};

let output = AcpOutput::callback(|line| {
    eprintln!("ACP -> host: {line}");
});
let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), output);
```

For most applications, prefer `EmbeddedAgent`; it packages the worker-thread
lifecycle and prevents accidentally moving the `!Send` server future across
threads.

## Host callback surface

During `session/prompt`, Harn may call back into the host over ACP JSON-RPC.
Hosts should be prepared to answer these methods:

- `host/capabilities`
- `host/call`
- `session/request_permission`
- filesystem and process methods advertised through `initialize`

If a host does not support a capability, return an empty capability response or
the relevant JSON-RPC error. Harn times host callbacks out so an unresponsive
host cannot block a prompt forever.
