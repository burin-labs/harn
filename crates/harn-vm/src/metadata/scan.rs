use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::value::{VmDictExt, VmValue};

#[derive(Clone)]
pub(super) struct ScanOptions {
    pub(super) pattern: Option<String>,
    pub(super) max_depth: usize,
    pub(super) include_hidden: bool,
    pub(super) include_dirs: bool,
    pub(super) include_files: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            pattern: None,
            max_depth: 5,
            include_hidden: false,
            include_dirs: true,
            include_files: true,
        }
    }
}

fn bool_arg(map: &crate::value::DictMap, key: &str, default: bool) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(value)) => *value,
        _ => default,
    }
}

fn usize_arg(map: &crate::value::DictMap, key: &str, default: usize) -> usize {
    match map.get(key) {
        Some(VmValue::Int(value)) if *value >= 0 => *value as usize,
        _ => default,
    }
}

pub(super) fn parse_scan_options(
    pattern_or_options: Option<&VmValue>,
    explicit_options: Option<&VmValue>,
) -> ScanOptions {
    let mut options = ScanOptions::default();
    if let Some(VmValue::String(pattern)) = pattern_or_options {
        options.pattern = Some(pattern.to_string());
    } else if let Some(VmValue::Dict(dict)) = pattern_or_options {
        apply_scan_options_dict(&mut options, dict);
    }
    if let Some(VmValue::Dict(dict)) = explicit_options {
        apply_scan_options_dict(&mut options, dict);
    }
    options
}

fn apply_scan_options_dict(options: &mut ScanOptions, dict: &crate::value::DictMap) {
    if let Some(pattern) = dict.get("pattern").map(|value| value.display()) {
        if !pattern.is_empty() {
            options.pattern = Some(pattern);
        }
    }
    options.max_depth = usize_arg(dict, "max_depth", options.max_depth);
    options.include_hidden = bool_arg(dict, "include_hidden", options.include_hidden);
    options.include_dirs = bool_arg(dict, "include_dirs", options.include_dirs);
    options.include_files = bool_arg(dict, "include_files", options.include_files);
}

pub(super) fn resolve_scan_root(rel_dir: &str) -> PathBuf {
    let candidate = PathBuf::from(rel_dir);
    if candidate.is_absolute() {
        return candidate;
    }
    crate::stdlib::process::resolve_source_relative_path(rel_dir)
}

pub(super) fn scan_dir_recursive(
    dir: &Path,
    base: &Path,
    options: &ScanOptions,
    results: &mut Vec<VmValue>,
    depth: usize,
) {
    if depth > options.max_depth {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !options.include_hidden && name.starts_with('.') {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(entry.path().as_path())
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(pat) = &options.pattern {
            if !glob_match(pat, &rel_path) {
                if meta.is_dir() {
                    scan_dir_recursive(&entry.path(), base, options, results, depth + 1);
                }
                continue;
            }
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut m = BTreeMap::new();
        m.put_str("path", rel_path);
        m.put_int("size", meta.len() as i64);
        m.put_int("modified", mtime);
        m.put_bool("is_dir", meta.is_dir());
        if (meta.is_dir() && options.include_dirs) || (!meta.is_dir() && options.include_files) {
            results.push(VmValue::dict(m));
        }
        if meta.is_dir() {
            scan_dir_recursive(&entry.path(), base, options, results, depth + 1);
        }
    }
}

/// Wildcards cross directory separators; literal patterns match substrings.
pub(super) fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        harn_glob::match_name(pattern, path)
    } else {
        path.contains(pattern)
    }
}
