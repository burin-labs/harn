//! Turning a requested conformance target and filter into a file list.
//!
//! The selection is resolved once, here, and every consumer takes the result:
//! the sequential runner, the parallel parent that shards it across workers,
//! and the emptiness diagnosis that reports on what was asked for.

use std::path::{Path, PathBuf};

use regex::Regex;

use super::{canonicalize_or_err, collect_harn_files_sorted};

pub(super) fn resolve_conformance_selection(
    suite_root: &Path,
    selection: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let suite_root = canonicalize_or_err(suite_root)?;

    let Some(selection) = selection else {
        return Ok(collect_harn_files_sorted(&suite_root));
    };

    let raw = PathBuf::from(selection);
    let mut candidates = vec![raw.clone()];
    if !raw.is_absolute() && !raw.starts_with(&suite_root) {
        candidates.push(suite_root.join(&raw));
    }

    let Some(candidate) = candidates.into_iter().find(|path| path.exists()) else {
        return Err(format!(
            "Conformance target not found: {selection}. Expected a file or directory under {}",
            suite_root.display()
        ));
    };

    let canonical = canonicalize_or_err(&candidate)?;
    if !canonical.starts_with(&suite_root) {
        return Err(format!(
            "Conformance target must be inside {}: {}",
            suite_root.display(),
            candidate.display()
        ));
    }

    if canonical.is_file() {
        if canonical.extension().is_some_and(|ext| ext == "harn") {
            return Ok(vec![canonical]);
        }
        return Err(format!(
            "Conformance target must be a .harn file or directory: {}",
            candidate.display()
        ));
    }

    let files = collect_harn_files_sorted(&canonical);
    if files.is_empty() {
        return Err(format!(
            "No .harn conformance tests found under {}",
            candidate.display()
        ));
    }
    Ok(files)
}

pub(super) fn conformance_filter_matches(rel_path: &str, filter: Option<&str>) -> bool {
    let Some(pattern) = filter else {
        return true;
    };
    if let Some(re_pat) = pattern.strip_prefix("re:") {
        Regex::new(re_pat).is_ok_and(|re| re.is_match(rel_path))
    } else if pattern.contains('|') {
        pattern.split('|').any(|p| rel_path.contains(p.trim()))
    } else if pattern.contains('*') || pattern.contains('?') {
        let escaped = regex::escape(pattern)
            .replace(r"\*", ".*")
            .replace(r"\?", ".");
        Regex::new(&escaped).is_ok_and(|re| re.is_match(rel_path))
    } else {
        rel_path.contains(pattern)
    }
}
