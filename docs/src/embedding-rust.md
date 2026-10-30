# Embedding Harn in Rust

`harn-serve` exposes the same ACP agent loop used by `harn serve acp` as a
Rust API. Use it when a host application wants Harn in-process instead of
spawning the CLI as a child process.

File-backed served pipelines resolve imported types and option contracts before
execution, including `AgentSpec` re-exported by `std/agent/options`. Hosts using
the VM directly should call `harn_vm::compile_source_at_path(source, path,
pipeline_name)` for a module anchored at a filesystem path. The supplied source
is the root buffer; imports resolve through Harn's module graph. The inline
`compile_source` and `compile_source_named` functions do not load filesystem
import contracts, and none of these functions grants privileged host dispatch.

## Add the crate

Until Harn is published on crates.io, depend on the repository tag you ship
against:

```toml
[dependencies]
harn-serve = { git = "https://github.com/burin-labs/harn", tag = "v0.10.183" }
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
harn-serve = { git = "...", tag = "v0.10.183", features = ["full"] }

# Lean smoke-test harness
harn-serve = { git = "...", tag = "v0.10.183", features = ["hostlib"] }
```

When either hostlib feature is enabled, dispatch the private process guardian
at the very beginning of the embedding executable's `main`:

```rust
fn main() {
    if harn_serve::run_process_guardian_if_requested() {
        unreachable!("process guardian execution does not return");
    }

    // Normal application startup.
}
```

This must run before public CLI parsing. On Unix, owner-death-contained
commands re-exec the embedding executable in guardian mode. A private pipe
carries the prepared command, including its explicit child environment. The
guardian removes inherited credentials and does not copy that data into its
own arguments or environment.
Without the dispatch, those commands fail during startup. Windows uses a Job
Object and the function returns `false`.

On Linux, the guardian transfers confinement to the process that starts the
payload. For loopback-only commands, the namespace helper creates the private
network namespace before applying Landlock and seccomp. The guardian keeps
the ruleset descriptor open through the helper's exec.

Finer control is available one layer down: `harn-hostlib`'s `ast` feature is
the code-intelligence mechanism only. Per-family grammar features
(`grammar-web`, `grammar-systems`, `grammar-scripting`, `grammar-jvm`,
`grammar-enterprise`, `grammar-data`, or the `grammars-all` umbrella on
`full`) are opted in separately so a client that only ever edits, say,
TypeScript and Python can compile just those grammars. A language whose family
is not compiled in still parses its name and extension; its AST-precise edits
simply degrade to the text fallback. Library crates that Burin and other
in-process hosts share (`harn-lint`, `harn-rules-hostlib`, …) keep
`harn-vm`/`harn-hostlib` lean so they cannot unify `full` back on.
`scripts/measure_lean_embedding.sh` reports the dependency delta between the
lean and full configurations and gates against regressions in CI.

Note: Cargo unifies features across a build graph, so any binary that also
depends on `harn-cli` (which enables the full set) gets the full surface
regardless of what it requests from `harn-serve`. The lean configurations only
take effect in builds that do **not** pull in the full CLI.

## Construct a VM directly

Add `harn-vm`, `harn-runtime`, and `harn-stdlib` at the same repository tag.
Install the embedded standard library once during host startup, then construct
the native interpreter and register its builtins:

```rust
harn_stdlib::install().expect("install embedded standard library");
let mut vm = harn_vm::new_vm();
harn_runtime::register_vm_stdlib(&mut vm);
```

`harn-runtime` links every crate that contributes builtin families, such as
`harn-flow`. Registering through `harn_vm::register_vm_stdlib` instead installs
only the families the linker happened to keep.

Replace `Vm::new()` and `Vm::default()` with this factory. Import
`harn_vm::NativeVmExt` when using `set_bridge`, `set_connector_clients`,
`evaluate_decision`, `set_evaluation_replay`, or `execution_evidence`.

The interpreter, values, compiler, and module artifacts belong to
`harn-vm-core`; `harn-vm` re-exports their public paths. A custom execution
backend constructs `harn_vm_core::Vm::with_runtime` with its required
`Arc<dyn VmRuntime>`. That contract supplies capability dispatch, policy gates,
execution scopes, deadlines, tracing, and task cleanup. Native embeddings
should use `harn_vm::new_vm()` to install those services together.

File replacement and cross-process locking also belong to `harn-vm-core`.
Native adapters select the execution's persistent state directory and record
tool mutations. Standalone core callers default to `.harn/fs-cas-locks` under
their working directory; install `scope_conditional_replace_lock_root` when
the host owns a different state directory. Install that thread-local scope on
each worker that writes files. Windows atomic writes and conditional
replacements share the same lock namespace.

Use `harn_vm::bytecode_cache::compiler_fingerprint()` for artifact compatibility.
It covers compiler inputs and builtin authority contracts. Construct cache keys
with `CacheKey::from_source` or `CacheKey::from_module_source` so those contracts
enter the key and artifact header.

Compiler source contributions come from the linked crates. Each owner hashes
its local sources and build script; Core combines those values with its compiler
and derived-interface inputs. The identity stays stable across checkout paths
and normalized crate archives. The CLI AOT generator recomputes the same declared
inputs in its requested checkout and refuses a mismatched compiled generator.
Check-result caching uses the same mechanism for the CLI, linter, and package
resolver. Build scripts watch existing local inputs, so extracted packages do
not rebuild because a workspace sibling directory is absent.

## Resolve connector credentials

Use `harn_serve::try_load_provider_connectors` with the project directory or
source anchor before initializing package-backed connector clients. It uses
the shared `harn-package` resolver, also used by the CLI, including dependency
materialization, root overrides, provider deduplication, and explicit dependency
failures.

```rust
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let project_root = std::env::current_dir()?;
let resolved = harn_serve::try_load_provider_connectors(&project_root)?;
let outbound = resolved.outbound_secret_ids();

// Resolve these typed ids from the host's selected secret provider.
// Keep `resolved` alive while initializing its connector configurations.
for id in outbound {
    println!("{}/{}", id.namespace, id.name);
}
# Ok(())
# }
```

Inbound verification secrets are excluded from outbound requirements. An
empty set means the resolved declarations need no outbound credentials; a
missing or malformed dependency returns an error. Use
`try_load_root_provider_connectors` only when the caller deliberately needs
root declarations without resolving package contributions. Keep the returned
`ResolvedProviderConnectors` alive through initialization so concurrent
package publication cannot collect its generation.

For child launches, add these direct dependencies using the same tag as
`harn-serve`. Enable its `hostlib` feature for the guardian entrypoint shown
above; the direct dependencies keep their default features disabled so the
handoff does not add grammars or native credential-store adapters.

```toml
harn-serve = { git = "https://github.com/burin-labs/harn", tag = "v0.10.183", features = ["hostlib"] }
harn-vm = { git = "https://github.com/burin-labs/harn", tag = "v0.10.183", default-features = false }
harn-hostlib = { git = "https://github.com/burin-labs/harn", tag = "v0.10.183", default-features = false }
```

The process handoff is available in lean hostlib without additional features.
Pass the discovered outbound IDs to
`harn_vm::secrets::ParentSecretHandoff::capture` with the selected parent
provider, then use `harn_hostlib::process::spawn_harn_with_parent_secrets`.
The child independently resolves the same manifest declarations. The handoff
keeps values out of its launch environment and closes ambient provider
fallback, including when a required grant is missing.

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
    let session = agent.start_run(AcpSessionNewParams::isolated(".")).await?;
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
`EmbeddedAgentClient::load_run(id, environment_policy)` to restore a saved session,
`subscribe_session_events(...)` for live timeline updates, and
`run_view_from_path(...)` to inspect a persisted run without reading private
record internals.

Pass the host's `AcpSessionEnvironmentConfig` to both `load_run` and
`load_run_with_cwd(id, cwd, environment_policy)`. Cold restoration requires a
fresh explicit declaration; saved history contains no credentials or launcher
environment snapshot. A live load retains its established authority and refuses
a declaration that would replace it. The typed JSON-RPC load constructor uses
`AcpSessionLoadParams::new(id, environment_policy).with_cwd(cwd)`.

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

`VmHarness` carries state owned by the runtime that created it. For native
state access, replace `handle.inner()` with
`handle.host_state::<harn_vm::harness::HarnessInner>()` and handle `None` when
the handle belongs to another backend. `host_state_arc` returns a shared
native owner when an embedder needs to retain it. Sub-handles retain the same
owner; reading their names does not grant access to that owner's state.

During `session/prompt`, Harn may call back into the host over ACP JSON-RPC.
Hosts should be prepared to answer these methods:

- `host/capabilities`
- `host/call`
- `session/request_permission`
- filesystem and process methods advertised through `initialize`

If a host does not support a capability, return an empty capability response or
the relevant JSON-RPC error. Harn times host callbacks out so an unresponsive
host cannot block a prompt forever.

Harn reads `host/capabilities` once when a prompt starts. If `harn.toml`
declares host operations, Harn compares them with that response and sends a
`HARN-CAP-008` log for each missing operation. Set
`[check].require_declared_operations_served = true` to fail the prompt before
the script runs. Exact names in `runtime_installed_host_operations` are exempt.

### Answer host calls from an embedded Agents API

Implement `harn_serve::adapters::api::ApiHostResponder` and attach an `Arc` with
`ApiServerConfig::with_host_responder`. Its synchronous `respond` method receives
`host/capabilities` and `host/call` with the original ACP parameters. Return
`Result<serde_json::Value, harn_serve::adapters::acp::AcpJsonRpcError>` and advertise
only operations the responder implements. Enforce each concrete operation's
authority in the responder.

Calls run on the blocking pool, while permission and cancellation messages stay
on the API service. Without a responder, the API advertises no host capabilities
and refuses host calls. `session/request_permission` always remains on the API's
permission service.
