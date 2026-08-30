use std::collections::HashSet;
use std::path::Path;
use std::process;

use notify::{Event, EventKind, RecursiveMode, Watcher};

use super::{run_file, CliLlmMockMode, RunProfileOptions};

pub(crate) async fn run_watch(path: &str, denied_builtins: HashSet<String>) {
    let abs_path = std::fs::canonicalize(path).unwrap_or_else(|error| {
        eprintln!("Error: {error}");
        process::exit(1);
    });
    let watch_dir = abs_path.parent().unwrap_or(Path::new("."));

    eprintln!("\x1b[2m[watch] running {path}...\x1b[0m");
    run_file(
        path,
        false,
        denied_builtins.clone(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let _watcher = {
        let tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |result: Result<Event, _>| {
            if let Ok(event) = result {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let has_harn = event
                        .paths
                        .iter()
                        .any(|path| path.extension().is_some_and(|ext| ext == "harn"));
                    if has_harn {
                        let _ = tx.blocking_send(());
                    }
                }
            }
        })
        .unwrap_or_else(|error| {
            eprintln!("Error setting up file watcher: {error}");
            process::exit(1);
        });
        watcher
            .watch(watch_dir, RecursiveMode::Recursive)
            .unwrap_or_else(|error| {
                eprintln!("Error watching directory: {error}");
                process::exit(1);
            });
        watcher
    };

    eprintln!(
        "\x1b[2m[watch] watching {} for .harn changes (ctrl-c to stop)\x1b[0m",
        watch_dir.display()
    );

    loop {
        rx.recv().await;
        // Debounce: let bursts of events settle for 200ms before re-running.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        eprintln!();
        eprintln!("\x1b[2m[watch] change detected, re-running {path}...\x1b[0m");
        run_file(
            path,
            false,
            denied_builtins.clone(),
            Vec::new(),
            CliLlmMockMode::Off,
            None,
            RunProfileOptions::default(),
        )
        .await;
    }
}
