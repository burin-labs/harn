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
    // The walker emits paths using the workspace's spelling, which may be a
    // symlink or an alias such as macOS /var. Project a physical state root
    // back onto that spelling before comparing it with walker entries.
    let configured = root
        .canonicalize()
        .ok()
        .and_then(|physical_root| {
            configured
                .strip_prefix(physical_root)
                .ok()
                .map(|relative| root.join(relative))
        })
        .unwrap_or(configured);
    if configured != root.join(".harn") {
        paths.extend(harn_vm::runtime_paths::owned_state_paths(&configured));
    }
    paths.push(crate::scanner::checkpoint_snapshot_path(root));
    paths.extend(harn_vm::runtime_paths::owned_project_paths(root));
    paths
}
