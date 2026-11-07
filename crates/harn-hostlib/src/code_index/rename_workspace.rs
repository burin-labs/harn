//! Bounded source visibility shared by binding-aware workspace renames.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::host_call::{HostCallContext, Phase};

/// Read beyond the index's pruning policy, including staged additions. A
/// caller retains the complete snapshot and repeats this census at commit.
pub(super) fn read_sources(
    root: &Path,
    session_id: Option<&str>,
    extension: &str,
    language: &str,
    max_entries: usize,
    max_source_bytes: usize,
    call: Option<&HostCallContext>,
) -> Result<Vec<(String, String)>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = BTreeSet::<PathBuf>::new();
    let mut entries_seen = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|err| err.to_string())? {
            heartbeat(call, language)?;
            entries_seen += 1;
            if entries_seen > max_entries {
                return Err(format!(
                    "{language} workspace census exceeded its entry limit"
                ));
            }
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|err| err.to_string())?;
            let source_path = path.extension().is_some_and(|value| value == extension);
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    if super::walker::is_sensitive(&path) {
                        return Err(format!(
                            "a sensitive workspace directory prevents {language} coverage proof"
                        ));
                    }
                    pending.push(path);
                }
            } else if kind.is_symlink() {
                if source_path
                    || (!super::walker::is_sensitive(&path)
                        && std::fs::metadata(&path)
                            .map_err(|err| err.to_string())?
                            .is_dir())
                {
                    return Err(format!(
                        "a workspace symbolic link prevents {language} coverage proof"
                    ));
                }
            } else if kind.is_file() && source_path {
                if entry.metadata().map_err(|err| err.to_string())?.len()
                    > super::walker::MAX_FILE_BYTES
                {
                    return Err(format!(
                        "a {language} workspace source exceeds the inspection limit"
                    ));
                }
                paths.insert(path);
            } else if source_path {
                return Err(format!(
                    "a {language} workspace source is not a regular file"
                ));
            }
        }
    }
    if let Some(session_id) = session_id {
        for path in crate::fs::staged_pending_paths(session_id).map_err(|err| err.to_string())? {
            entries_seen += 1;
            if entries_seen > max_entries {
                return Err(format!(
                    "{language} workspace census exceeded its entry limit"
                ));
            }
            heartbeat(call, language)?;
            if path.extension().is_some_and(|value| value == extension) {
                paths.insert(path);
            }
        }
    }
    let mut sources = Vec::new();
    let mut bytes = 0usize;
    for path in paths {
        heartbeat(call, language)?;
        if super::walker::is_sensitive(&path) {
            return Err(format!(
                "a {language} workspace source cannot be safely inspected"
            ));
        }
        let relative = path.strip_prefix(root).map_err(|err| err.to_string())?;
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("a {language} workspace path is not UTF-8"))?;
        let source =
            super::refactor_core::read_source(super::rename::BUILTIN, root, relative, session_id)
                .map_err(|err| err.to_string())?;
        if source.len() as u64 > super::walker::MAX_FILE_BYTES {
            return Err(format!(
                "a {language} workspace source exceeds the inspection limit"
            ));
        }
        bytes = bytes.saturating_add(source.len());
        if bytes > max_source_bytes {
            return Err(format!(
                "{language} workspace census exceeded its source-size limit"
            ));
        }
        sources.push((relative.replace('\\', "/"), source));
    }
    Ok(sources)
}

fn heartbeat(call: Option<&HostCallContext>, language: &str) -> Result<(), String> {
    if let Some(call) = call {
        if call.interrupted().is_some() {
            return Err(format!("{language} workspace coverage was interrupted"));
        }
        call.heartbeat(Phase::Resolving, "checking workspace rename sources");
    }
    Ok(())
}
