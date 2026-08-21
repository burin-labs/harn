use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use harn_serve::FilePromptCatalog;
use serde_json::Value as JsonValue;

pub(super) struct ManifestDerivedState {
    project_root: PathBuf,
    /// Allocated when a refresh starts, so completion order cannot make an
    /// older filesystem observation authoritative again.
    next_refresh_revision: AtomicU64,
    snapshot: Mutex<ManifestDerivedSnapshot>,
}

struct ManifestDerivedSnapshot {
    refresh_revision: u64,
    manifest_source: String,
    prompt_catalog: FilePromptCatalog,
}

impl ManifestDerivedState {
    pub(super) fn discover(project_root: &Path, manifest_source: String) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            next_refresh_revision: AtomicU64::new(1),
            snapshot: Mutex::new(ManifestDerivedSnapshot {
                refresh_revision: 0,
                manifest_source,
                prompt_catalog: FilePromptCatalog::discover(project_root),
            }),
        }
    }

    pub(super) fn refresh(&self, manifest_source: String) {
        self.refresh_from(manifest_source, || {
            FilePromptCatalog::discover(&self.project_root)
        });
    }

    fn refresh_from(
        &self,
        manifest_source: String,
        discover: impl FnOnce() -> FilePromptCatalog,
    ) -> bool {
        let refresh_revision = self.next_refresh_revision.fetch_add(1, Ordering::Relaxed);
        let prompt_catalog = discover();
        let mut snapshot = self
            .snapshot
            .lock()
            .expect("manifest-derived state poisoned");
        // Discovery performs filesystem I/O outside the mutex so MCP reads
        // keep serving the last complete snapshot. The revision makes the
        // later refresh request win even when an older discovery finishes last.
        if refresh_revision <= snapshot.refresh_revision {
            return false;
        }
        *snapshot = ManifestDerivedSnapshot {
            refresh_revision,
            manifest_source,
            prompt_catalog,
        };
        true
    }

    pub(super) fn manifest_source(&self) -> String {
        self.snapshot
            .lock()
            .expect("manifest-derived state poisoned")
            .manifest_source
            .clone()
    }

    pub(super) fn prompt_list(&self) -> Vec<JsonValue> {
        self.snapshot
            .lock()
            .expect("manifest-derived state poisoned")
            .prompt_catalog
            .list()
    }

    pub(super) fn prompt_get(
        &self,
        name: &str,
        arguments: &JsonValue,
    ) -> Result<JsonValue, String> {
        self.snapshot
            .lock()
            .expect("manifest-derived state poisoned")
            .prompt_catalog
            .get(name, arguments)
    }

    pub(super) fn prompt_complete(
        &self,
        name: &str,
        argument_name: &str,
        value: &str,
    ) -> Result<JsonValue, String> {
        self.snapshot
            .lock()
            .expect("manifest-derived state poisoned")
            .prompt_catalog
            .complete(name, argument_name, value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::*;

    fn prompt_catalog(body: &str) -> (TempDir, FilePromptCatalog) {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("review.harn.prompt"), body).unwrap();
        let catalog = FilePromptCatalog::discover(temp.path());
        (temp, catalog)
    }

    #[test]
    fn older_discovery_cannot_overwrite_a_newer_refresh() {
        let root = TempDir::new().unwrap();
        let state = Arc::new(ManifestDerivedState::discover(
            root.path(),
            "initial manifest".to_string(),
        ));
        let (_older_root, older_catalog) = prompt_catalog("Older body");
        let (_newer_root, newer_catalog) = prompt_catalog("Newer body");
        let older_started = Arc::new(Barrier::new(2));
        let release_older = Arc::new(Barrier::new(2));

        let older_state = state.clone();
        let older_started_in_thread = older_started.clone();
        let release_older_in_thread = release_older.clone();
        let older = std::thread::spawn(move || {
            older_state.refresh_from("older manifest".to_string(), || {
                older_started_in_thread.wait();
                release_older_in_thread.wait();
                older_catalog
            })
        });

        older_started.wait();
        assert!(state.refresh_from("newer manifest".to_string(), || newer_catalog));
        release_older.wait();
        assert!(!older.join().unwrap());

        assert_eq!(state.manifest_source(), "newer manifest");
        assert_eq!(
            state.prompt_get("review", &serde_json::json!({})).unwrap()["messages"][0]["content"]
                ["text"],
            serde_json::json!("Newer body")
        );
    }
}
