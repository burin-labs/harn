pub(crate) mod agents_conformance;
pub(crate) mod bench;
pub(crate) mod check;
pub(crate) mod connect;
pub(crate) mod connector;
pub(crate) mod contracts;
pub(crate) mod doctor;
pub(crate) mod dump_highlight_keywords;
pub(crate) mod dump_trigger_quickref;
pub(crate) mod explain;
pub(crate) mod init;
pub(crate) mod mcp;
pub(crate) mod orchestrator;
pub(crate) mod persona;
pub(crate) mod playground;
pub(crate) mod portal;
pub(crate) mod repl;
pub(crate) mod run;
pub(crate) mod serve;
pub(crate) mod skill;
pub(crate) mod skills;
pub(crate) mod test;
pub(crate) mod trace;
pub(crate) mod trigger;
pub(crate) mod trust;
pub(crate) mod viz;

use std::path::{Path, PathBuf};

/// Recursively collect `.harn` files under `dir`, sorted by path. Files with a
/// sibling `<name>.conformance-skip` marker are excluded — used to temporarily
/// park tests that are tracking a known regression in an issue so `make test`
/// + `harn test conformance` can stay green while the fix is in flight.
pub(crate) fn collect_harn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_harn_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                let skip_marker = path.with_extension("conformance-skip");
                if skip_marker.exists() {
                    continue;
                }
                out.push(path);
            }
        }
    }
}
