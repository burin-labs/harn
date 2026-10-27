use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use super::UserTestRunArgs;

pub(super) async fn run(path_str: &str, args: UserTestRunArgs<'_>) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        crate::fatal_error(format!("Path not found: {path_str}"));
    }

    cli_diagnostic!("Watching {path_str} for changes... (Ctrl+C to stop)");

    let session = crate::test_runner::TestRunSession::default();
    super::run_user_tests_once_with_session(&path, args.clone(), &session).await;

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap_or_else(|error| {
        crate::fatal_error(format!("Failed to create file watcher: {error}"));
    });
    watcher
        .watch(&path, RecursiveMode::Recursive)
        .unwrap_or_else(|error| {
            crate::fatal_error(format!("Failed to watch {path_str}: {error}"));
        });

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let is_harn = event.paths.iter().any(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "harn")
                });
                if !is_harn {
                    continue;
                }

                // Debounce: drain any additional events within 100ms.
                while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}

                cli_diagnostic!("\x1b[2m--- file changed, re-running tests ---\x1b[0m");
                super::run_user_tests_once_with_session(&path, args.clone(), &session).await;
            }
            Ok(Err(error)) => cli_diagnostic!("Watch error: {error}"),
            Err(_) => break,
        }
    }
}
