pub(crate) mod check;
pub(crate) mod dump_highlight_keywords;
pub(crate) mod init;
pub(crate) mod mcp;
pub(crate) mod portal;
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod test;

use std::path::{Path, PathBuf};

/// Recursively collect `.harn` files under `dir`, sorted by path.
pub(crate) fn collect_harn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_harn_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                out.push(path);
            }
        }
    }
}
