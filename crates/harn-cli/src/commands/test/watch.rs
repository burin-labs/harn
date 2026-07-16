use std::path::PathBuf;
use std::process;
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use super::UserTestRunArgs;

pub(super) async fn run(path_str: &str, args: UserTestRunArgs<'_>) {
    let path = PathBuf::from(path_str);
    if !path.exists() {
        eprintln!("Path not found: {path_str}");
        process::exit(1);
    }

    println!("Watching {path_str} for changes... (Ctrl+C to stop)\n");

    let session = crate::test_runner::TestRunSession::default();
    super::run_user_tests_once_with_session(&path, args, &session).await;

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap_or_else(|error| {
        eprintln!("Failed to create file watcher: {error}");
        process::exit(1);
    });
    watcher
        .watch(&path, RecursiveMode::Recursive)
        .unwrap_or_else(|error| {
            eprintln!("Failed to watch {path_str}: {error}");
            process::exit(1);
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

                println!("\n\x1b[2m--- file changed, re-running tests ---\x1b[0m\n");
                super::run_user_tests_once_with_session(&path, args, &session).await;
            }
            Ok(Err(error)) => eprintln!("Watch error: {error}"),
            Err(_) => break,
        }
    }
}
