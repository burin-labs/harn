use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

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
                if !is_source_change(&event) {
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

fn is_source_change(event: &Event) -> bool {
    // Running a test reads its source. Access events must not feed that read
    // back into another run, but edits, creations, and removals still matter.
    matches!(
        event.kind,
        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
    ) && event.paths.iter().any(|path| {
        path.extension()
            .is_some_and(|extension| extension == "harn")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, ModifyKind};

    #[test]
    fn test_reads_do_not_trigger_another_watch_run() {
        let read = Event::new(EventKind::Access(AccessKind::Read)).add_path("test.harn".into());
        let edit = Event::new(EventKind::Modify(ModifyKind::Any)).add_path("test.harn".into());
        let unrelated = Event::new(EventKind::Modify(ModifyKind::Any)).add_path("notes.txt".into());
        assert!(
            !is_source_change(&read),
            "the test runner reads its own input"
        );
        assert!(
            is_source_change(&edit),
            "a source edit must trigger a rerun"
        );
        assert!(
            !is_source_change(&unrelated),
            "unrelated files must not trigger a rerun"
        );
    }
}
