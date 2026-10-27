//! Runtime-owned workspace paths composed from their producers.

use std::path::{Path, PathBuf};

/// Paths whose contents belong to Harn rather than to a workspace edit.
pub fn owned_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = harn_vm::runtime_paths::owned_state_paths(&root.join(".harn"));
    let configured = harn_vm::runtime_paths::state_root(root);
    // Match scanner paths against the physical root when it exists. A
    // launcher may spell it with `..` or through a symlink.
    let configured = configured
        .canonicalize()
        .unwrap_or_else(|_| crate::fs::paths::normalize_against(root, &configured));
    if configured != root.join(".harn") {
        paths.extend(harn_vm::runtime_paths::owned_state_paths(&configured));
    }
    paths.push(crate::scanner::checkpoint_snapshot_path(root));
    paths.extend(harn_vm::runtime_paths::owned_project_paths(root));
    paths
}
