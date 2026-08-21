use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::Watcher;

use super::derived_state::ManifestDerivedState;

pub(super) fn start_cache_refresh_watcher(
    project_root: PathBuf,
    config_path: PathBuf,
    derived_state: Arc<ManifestDerivedState>,
) -> Option<notify::RecommendedWatcher> {
    let project_root_for_callback = project_root.clone();
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let prompt_changed = event.paths.iter().any(|path| {
            !is_package_generation_path(path, &project_root_for_callback)
                && is_prompt_reload_path(path)
        });
        let manifest_changed = event.paths.iter().any(|path| {
            !is_package_generation_path(path, &project_root_for_callback)
                && is_manifest_reload_path(path)
        });
        let package_changed = event
            .paths
            .iter()
            .any(|path| is_package_reload_path(path.as_path(), &project_root_for_callback));

        if !prompt_changed && !manifest_changed && !package_changed {
            return;
        }

        if prompt_changed || manifest_changed || package_changed {
            let manifest_source = std::fs::read_to_string(&config_path).unwrap_or_default();
            derived_state.refresh(manifest_source);
        }
    })
    .ok()?;
    watch_with_deadline(watcher, &project_root)
}

/// How long to wait for the platform watcher to accept a registration.
///
/// Registering is a handshake with the backend's own thread, not real work, so
/// a wait this long means that thread is not coming back.
const WATCH_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Register `project_root` with `watcher`, giving up if the backend never
/// answers.
///
/// `notify`'s Windows backend registers by handing the request to its server
/// thread and then blocking on an unacknowledged channel receive
/// (`send_action_require_ack`). That receive has no timeout: if the server
/// thread does not acknowledge, `watch` never returns. It runs on the caller's
/// thread, so the whole process stops — no error, no log line, no indication of
/// which call is stuck. The inotify and FSEvents backends have no such
/// handshake, which is why this can only wedge on Windows.
///
/// So registration happens on a thread we are willing to abandon. A backend
/// that never answers costs us automatic catalog refresh and a warning line
/// instead of the server. The abandoned thread keeps the watcher, which is the
/// only place it can safely be dropped.
fn watch_with_deadline(
    mut watcher: notify::RecommendedWatcher,
    project_root: &Path,
) -> Option<notify::RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel();
    let root = project_root.to_path_buf();
    std::thread::spawn(move || {
        let registered = watcher
            .watch(&root, notify::RecursiveMode::Recursive)
            .map(|()| watcher);
        // Fails only if we already gave up and dropped the receiver.
        let _ = tx.send(registered);
    });
    match rx.recv_timeout(WATCH_REGISTRATION_TIMEOUT) {
        Ok(Ok(watcher)) => Some(watcher),
        Ok(Err(error)) => {
            eprintln!("[harn] warning: filesystem watch unavailable: {error}");
            None
        }
        Err(_) => {
            eprintln!(
                "[harn] warning: registering a filesystem watch on {} did not complete within {}s; \
                 continuing without automatic MCP catalog refresh",
                project_root.display(),
                WATCH_REGISTRATION_TIMEOUT.as_secs()
            );
            None
        }
    }
}

fn is_prompt_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml" || name.ends_with(".harn.prompt"))
}

fn is_manifest_reload_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "harn.toml")
}

fn is_package_reload_path(path: &Path, project_root: &Path) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    relative == Path::new(".harn").join("package-current.toml")
}

fn is_package_generation_path(path: &Path, project_root: &Path) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    relative.starts_with(Path::new(".harn").join("package-generations"))
}

#[cfg(test)]
mod package_reload_tests {
    use super::*;

    #[test]
    fn only_atomic_package_pointer_is_a_package_publication_event() {
        let root = Path::new("workspace");
        assert!(is_package_reload_path(
            Path::new("workspace/.harn/package-current.toml"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/harn.lock"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/.harn/package-generations/generation-a/harn.lock"),
            root
        ));
        assert!(!is_package_reload_path(
            Path::new("workspace/.harn/packages/acme/harn.toml"),
            root
        ));
        assert!(is_package_generation_path(
            Path::new("workspace/.harn/package-generations/generation-a/packages/acme/harn.toml"),
            root
        ));
    }
}
