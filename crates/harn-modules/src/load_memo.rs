//! Process-wide memo of each module's graph projection.
//!
//! Every graph build used to read, lex, and parse every module it reached.
//! Batch commands build one graph per entry, so a module imported by most
//! entries was parsed once per entry: one cold precompile of a large pipeline
//! tree lexed about 270 times more source than the tree holds.
//!
//! A module's projection is a function of its path, its source text, and the
//! result of resolving each import it names. The memo keeps the text and the
//! projection, whose import list records every resolution it consulted. A hit
//! needs identical text and, re-asked now, identical resolutions, so an edited
//! module, a newly created import target, or a reinstalled package generation
//! reprojects exactly as a cold process would.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::package_imports::resolve_import_path_with_snapshots;
use crate::package_snapshot::PackageSnapshot;
use crate::ModuleInfo;

struct Remembered {
    source: String,
    module: ModuleInfo,
}

/// One entry per module path: a new source text replaces the old one, so a
/// long-lived editor process holds at most one projection per file.
fn memo() -> &'static Mutex<HashMap<PathBuf, Arc<Remembered>>> {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, Arc<Remembered>>>> = OnceLock::new();
    MEMO.get_or_init(Mutex::default)
}

/// The remembered projection of `path`, when `source` is the text it was
/// projected from and every import still resolves where it did.
pub(crate) fn recall(
    path: &Path,
    source: &str,
    package_snapshots: &[PackageSnapshot],
) -> Option<ModuleInfo> {
    let remembered = memo()
        .lock()
        .expect("module load memo poisoned")
        .get(path)
        .cloned()?;
    if remembered.source != source {
        return None;
    }
    let resolutions_hold = remembered.module.imports.iter().all(|import| {
        resolve_import_path_with_snapshots(path, &import.raw_path, package_snapshots) == import.path
    });
    resolutions_hold.then(|| remembered.module.clone())
}

pub(crate) fn remember(path: &Path, source: String, module: &ModuleInfo) {
    memo().lock().expect("module load memo poisoned").insert(
        path.to_path_buf(),
        Arc::new(Remembered {
            source,
            module: module.clone(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::build;

    #[test]
    fn remembered_projection_follows_new_import_targets_and_edits() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("entry.harn");
        let dep = dir.path().join("dep.harn");
        std::fs::write(
            &entry,
            "import { helper } from \"./dep\"\npub fn run() { 1 }\n",
        )
        .unwrap();
        let graph = build(std::slice::from_ref(&entry));
        assert_eq!(graph.imported_names_for_file(&entry), None);

        // Same entry text, but its import now resolves.
        std::fs::write(&dep, "pub fn helper() { 1 }\n").unwrap();
        let graph = build(std::slice::from_ref(&entry));
        assert_eq!(
            graph.imported_names_for_file(&entry),
            Some(HashSet::from(["helper".to_string()]))
        );

        // A same-length edit inside one timestamp tick still reprojects.
        std::fs::write(&dep, "pub fn helpex() { 1 }\n").unwrap();
        let graph = build(std::slice::from_ref(&entry));
        assert_eq!(graph.exported_kind(&dep, "helper"), None);
        assert!(graph.exported_kind(&dep, "helpex").is_some());
    }
}
