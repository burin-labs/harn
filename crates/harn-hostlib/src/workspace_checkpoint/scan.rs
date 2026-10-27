//! Workspace scan and the runtime-owned paths it must never attribute.

use super::{stat_entry, to_agent_path, Scan, StatIndex};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) fn excluded_paths(root: &Path) -> Vec<PathBuf> {
    crate::workspace_runtime_paths::owned_paths(root)
}

pub(super) fn is_excluded(root: &Path, path: &Path, excluded: &[PathBuf]) -> bool {
    if excluded.iter().any(|entry| path.starts_with(entry)) {
        return true;
    }
    path.ancestors()
        .take_while(|ancestor| ancestor.starts_with(root))
        .filter(|ancestor| ancestor.file_name().is_some_and(|name| name == ".harn"))
        .filter_map(Path::parent)
        .filter(|parent| *parent != root)
        .any(|parent| {
            crate::workspace_runtime_paths::owned_paths(parent)
                .iter()
                .any(|entry| path.starts_with(entry))
        })
}

/// Stat non-ignored regular files, excluding nested repositories and runtime state.
pub(super) fn scan_tree(root: &Path, read_parent_ignores: bool) -> Scan {
    scan_tree_with_exclusions(root, read_parent_ignores, excluded_paths(root))
}

pub(super) fn scan_tree_with_exclusions(
    root: &Path,
    read_parent_ignores: bool,
    excluded: Vec<PathBuf>,
) -> Scan {
    let started = Instant::now();
    let walk_root = root.to_path_buf();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .parents(read_parent_ignores)
        .follow_links(false)
        .filter_entry(move |entry| {
            if entry.file_name() == ".git" || is_excluded(&walk_root, entry.path(), &excluded) {
                return false;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && entry.path() != walk_root && entry.path().join(".git").exists())
        })
        .build();
    let mut index = StatIndex::new();
    let mut errors = 0;
    let mut error_paths = Vec::new();
    let mut unlocated_error = false;
    let mut note_error = |path: Option<&Path>| {
        errors += 1;
        match path.and_then(|p| p.strip_prefix(root).ok()) {
            Some(rel) => error_paths.push(to_agent_path(rel)),
            None => unlocated_error = true,
        }
    };
    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                note_error(error_path(&error).as_deref());
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            note_error(Some(entry.path()));
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        index.insert(to_agent_path(rel), stat_entry(&meta));
    }
    Scan {
        index,
        errors,
        error_paths,
        unlocated_error,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn error_path(error: &ignore::Error) -> Option<PathBuf> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path.clone()),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        _ => None,
    }
}
