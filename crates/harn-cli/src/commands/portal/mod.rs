mod assets;
mod dlq;
mod dto;
mod errors;
mod handlers;
mod highlight;
mod launch;
mod llm;
mod query;
mod router;
mod run_analysis;
mod skill_events;
mod state;
mod transcript;
mod ts_bindings;
mod util;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use self::router::build_router;
use self::state::PortalState;

/// Render the portal's TypeScript API types; see [`ts_bindings`].
pub(crate) fn render_ts_types() -> String {
    ts_bindings::render()
}

pub(crate) async fn run_portal(
    dir: &str,
    manifest: Option<PathBuf>,
    persona_state_dir: PathBuf,
    host: &str,
    port: u16,
    open_browser: bool,
    allow_remote_launch: bool,
) {
    let run_dir = PathBuf::from(dir);
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let event_log = harn_vm::event_log::install_default_for_base_dir(&workspace_root).ok();
    let launch_program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("harn"));
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            cli_diagnostic!("error: invalid portal bind address {host}:{port}: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(PortalState {
        run_dir: run_dir.clone(),
        workspace_root,
        persona_manifest: manifest,
        persona_state_dir,
        event_log,
        launch_program,
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
        mutation_endpoints_enabled: addr.ip().is_loopback() || allow_remote_launch,
    });
    let app = build_router(state.clone());
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            cli_diagnostic!("error: failed to bind portal listener on {addr}: {e}");
            std::process::exit(1);
        }
    };
    let bound_addr = listener.local_addr().unwrap_or_else(|error| {
        crate::fatal_error(format!("failed to inspect portal listener: {error}"))
    });
    let url = format!("http://{bound_addr}");
    cli_diagnostic!("Harn portal listening on {url}");
    cli_diagnostic!("Watching run records in {}", run_dir.display());
    if !state.mutation_endpoints_enabled {
        cli_diagnostic!(
            "Launch and replay endpoints are disabled for non-loopback binds; pass --allow-remote-launch to enable them."
        );
    }
    if open_browser {
        let _ = webbrowser::open(&url);
    }
    if let Err(e) = axum::serve(listener, app).await {
        cli_diagnostic!("error: portal server failed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
