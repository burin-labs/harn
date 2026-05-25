//! Shared in-process spawner for the generic `harn-serve::McpServer`.
//!
//! The spawner pre-binds the TCP listener so the test knows the URL
//! up-front and hands the listener directly to
//! [`McpServer::run_http_from_listener`]. That means the kernel has
//! already accepted the bind by the time we return — there is no
//! drop/rebind window, no poll-until-listening loop, and no wall-clock
//! deadline. Tests get a fully-described `base_url` they can hit
//! immediately.

use std::sync::Arc;

use harn_serve::{
    bind_listener, DispatchCore, DispatchCoreConfig, HttpTlsConfig, McpHttpServeOptions, McpServer,
    McpServerConfig,
};
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Default tool surface every spawned server exposes — a plain `echo`
/// function so tests can exercise the full `tools/list` and `tools/call`
/// wire without standing up project-specific .harn fixtures.
pub const DEFAULT_ECHO_SCRIPT: &str = r#"
pub fn echo(message: string) -> string {
  return message
}
"#;

/// Running generic MCP server. Dropping it aborts the spawned task;
/// the held `TempDir` survives until then so the script file stays
/// valid for the duration of the server's lifetime.
pub struct GenericMcpServer {
    pub base_url: String,
    join: JoinHandle<Result<(), String>>,
    _temp: TempDir,
}

impl Drop for GenericMcpServer {
    fn drop(&mut self) {
        self.join.abort();
    }
}

/// Spawn a generic MCP server backed by [`DEFAULT_ECHO_SCRIPT`] on an
/// ephemeral 127.0.0.1 port.
pub async fn spawn() -> GenericMcpServer {
    spawn_with_script(DEFAULT_ECHO_SCRIPT).await
}

/// Spawn a generic MCP server backed by an arbitrary `.harn` source.
pub async fn spawn_with_script(script_source: &str) -> GenericMcpServer {
    let temp = TempDir::new().expect("tempdir");
    let script = temp.path().join("server.harn");
    std::fs::write(&script, script_source).expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(McpServer::new(McpServerConfig::new(core)));

    let listener = bind_listener("127.0.0.1:0".parse().expect("bind addr")).expect("bind listener");
    let local_addr = listener.local_addr().expect("local addr");
    let base_url = format!("http://{local_addr}/mcp");

    let options = McpHttpServeOptions {
        bind: local_addr,
        path: "/mcp".to_string(),
        sse_path: "/sse".to_string(),
        messages_path: "/messages".to_string(),
        tls: HttpTlsConfig::plain(),
    };
    let join = tokio::spawn(async move { server.run_http_from_listener(listener, options).await });

    GenericMcpServer {
        base_url,
        join,
        _temp: temp,
    }
}
