mod assets;
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
mod util;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use self::router::build_router;
use self::state::PortalState;

pub(crate) async fn run_portal(dir: &str, host: &str, port: u16, open_browser: bool) {
    let run_dir = PathBuf::from(dir);
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let event_log = harn_vm::event_log::install_default_for_base_dir(&workspace_root).ok();
    let launch_program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("harn"));
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: invalid portal bind address {host}:{port}: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(PortalState {
        run_dir: run_dir.clone(),
        workspace_root,
        event_log,
        launch_program,
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
    });
    let app = build_router(state);
    let url = format!("http://{addr}");

    println!("Harn portal listening on {url}");
    println!("Watching run records in {}", run_dir.display());

    if open_browser {
        let _ = webbrowser::open(&url);
    }

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind portal listener on {addr}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("error: portal server failed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
