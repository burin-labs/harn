//! Host-builtin handlers for the `code_index` module.
//!
//! Each handler shape mirrors the schema in
//! `schemas/code_index/<method>.{request,response}.json`. A single shared
//! [`SharedIndex`] cell is captured by the closure of every handler so all
//! five builtins observe the same in-memory state — `rebuild` writes,
//! everything else reads.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use harn_vm::VmValue;

use super::file_table::FileId;
use super::imports;
use super::state::IndexState;
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};

/// Shared, mutable cell carrying the (at most one) live workspace index.
/// `Mutex` rather than `RwLock` because rebuilds flip the slot wholesale —
/// fine-grained concurrency between rebuild + reads is intentionally not
/// supported (the Swift side serialised through a single actor too).
pub type SharedIndex = Arc<Mutex<Option<IndexState>>>;

pub(super) const BUILTIN_QUERY: &str = "hostlib_code_index_query";
pub(super) const BUILTIN_REBUILD: &str = "hostlib_code_index_rebuild";
pub(super) const BUILTIN_STATS: &str = "hostlib_code_index_stats";
pub(super) const BUILTIN_IMPORTS_FOR: &str = "hostlib_code_index_imports_for";
pub(super) const BUILTIN_IMPORTERS_OF: &str = "hostlib_code_index_importers_of";

pub(super) fn run_query(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_QUERY, args)?;
    let dict = raw.as_ref();
    let needle = require_string(BUILTIN_QUERY, dict, "needle")?;
    if needle.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_QUERY,
            param: "needle",
            message: "must not be empty".to_string(),
        });
    }
    let case_sensitive = optional_bool(BUILTIN_QUERY, dict, "case_sensitive", false)?;
    let max_results = optional_int(BUILTIN_QUERY, dict, "max_results", 100)?;
    if max_results < 1 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_QUERY,
            param: "max_results",
            message: "must be >= 1".to_string(),
        });
    }
    let scope = optional_string_list(dict, "scope")?;

    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(empty_query_response());
    };

    let candidate_ids = candidates_for(state, &needle);
    let mut hits: Vec<Hit> = Vec::new();
    for id in candidate_ids {
        let Some(file) = state.files.get(&id) else {
            continue;
        };
        if !scope_allows(&scope, &file.relative_path) {
            continue;
        }
        let Some(text) = read_file_text(&state.root, &file.relative_path) else {
            continue;
        };
        let count = count_matches(&text, &needle, case_sensitive);
        if count == 0 {
            continue;
        }
        hits.push(Hit {
            path: file.relative_path.clone(),
            score: count as f64,
            match_count: count,
        });
    }
    hits.sort_by(|a, b| {
        b.match_count
            .cmp(&a.match_count)
            .then_with(|| a.path.cmp(&b.path))
    });
    let max = max_results as usize;
    let truncated = hits.len() > max;
    if truncated {
        hits.truncate(max);
    }
    Ok(build_dict([
        (
            "results",
            VmValue::List(Rc::new(hits.into_iter().map(hit_to_value).collect())),
        ),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

pub(super) fn run_rebuild(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_REBUILD, args)?;
    let dict = raw.as_ref();
    let _force = optional_bool(BUILTIN_REBUILD, dict, "force", false)?;
    let root = optional_string(BUILTIN_REBUILD, dict, "root")?
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !root.exists() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_REBUILD,
            param: "root",
            message: format!("path `{}` does not exist", root.display()),
        });
    }
    if !root.is_dir() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_REBUILD,
            param: "root",
            message: format!("path `{}` is not a directory", root.display()),
        });
    }
    let started = Instant::now();
    let (state, outcome) = IndexState::build_from_root(&root);
    let elapsed_ms = started.elapsed().as_millis() as i64;
    {
        let mut guard = index.lock().expect("code_index mutex poisoned");
        *guard = Some(state);
    }
    Ok(build_dict([
        ("files_indexed", VmValue::Int(outcome.files_indexed as i64)),
        ("files_skipped", VmValue::Int(outcome.files_skipped as i64)),
        ("elapsed_ms", VmValue::Int(elapsed_ms)),
    ]))
}

pub(super) fn run_stats(index: &SharedIndex, _args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(empty_stats_response());
    };
    Ok(build_dict([
        ("indexed_files", VmValue::Int(state.files.len() as i64)),
        (
            "trigrams",
            VmValue::Int(state.trigrams.distinct_trigrams() as i64),
        ),
        ("words", VmValue::Int(state.words.distinct_words() as i64)),
        ("memory_bytes", VmValue::Int(state.estimated_bytes() as i64)),
        (
            "last_rebuild_unix_ms",
            VmValue::Int(state.last_built_unix_ms),
        ),
    ]))
}

pub(super) fn run_imports_for(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_IMPORTS_FOR, args)?;
    let dict = raw.as_ref();
    let path = require_string(BUILTIN_IMPORTS_FOR, dict, "path")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(empty_imports_response(&path));
    };
    let Some(file_id) = state.lookup_path(&path) else {
        return Ok(empty_imports_response(&path));
    };
    let Some(file) = state.files.get(&file_id) else {
        return Ok(empty_imports_response(&path));
    };
    let kind = imports::import_kind(&file.language).to_string();
    let base_dir = imports::parent_dir(&file.relative_path);
    let resolved_ids: HashSet<FileId> = state.deps.imports_of(file_id).into_iter().collect();
    let mut entries: Vec<VmValue> = Vec::with_capacity(file.imports.len());
    for raw_import in &file.imports {
        let resolved_path =
            imports::resolve_module(raw_import, &file.language, &base_dir, &state.path_to_id)
                .filter(|id| resolved_ids.contains(id))
                .and_then(|id| state.files.get(&id).map(|f| f.relative_path.clone()));
        entries.push(import_entry(raw_import, resolved_path.as_deref(), &kind));
    }
    Ok(build_dict([
        ("path", str_value(&file.relative_path)),
        ("imports", VmValue::List(Rc::new(entries))),
    ]))
}

pub(super) fn run_importers_of(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_IMPORTERS_OF, args)?;
    let dict = raw.as_ref();
    let module = require_string(BUILTIN_IMPORTERS_OF, dict, "module")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(empty_importers_response(&module));
    };

    let target_id = state.lookup_path(&module).or_else(|| {
        // Fallback: suffix-match on relative paths so callers can request
        // by basename (matching the `allowSuffixMatch` convention used by
        // the resolver itself).
        let needle = format!("/{module}");
        state
            .path_to_id
            .iter()
            .find(|(p, _)| p.ends_with(&needle) || *p == &module)
            .map(|(_, id)| *id)
    });

    let mut importers: Vec<String> = match target_id {
        Some(id) => state
            .deps
            .importers_of(id)
            .into_iter()
            .filter_map(|importer_id| {
                state
                    .files
                    .get(&importer_id)
                    .map(|f| f.relative_path.clone())
            })
            .collect(),
        None => Vec::new(),
    };
    importers.sort();
    Ok(build_dict([
        ("module", str_value(&module)),
        (
            "importers",
            VmValue::List(Rc::new(importers.into_iter().map(str_value).collect())),
        ),
    ]))
}

fn candidates_for(state: &IndexState, needle: &str) -> Vec<FileId> {
    if needle.len() >= 3 {
        let trigrams = super::trigram::query_trigrams(needle);
        return state.trigrams.query(&trigrams).into_iter().collect();
    }
    // Sub-3-byte needles are below the trigram floor — fall back to
    // scanning the whole file table (rare interactive case).
    state.files.keys().copied().collect()
}

fn read_file_text(root: &std::path::Path, relative: &str) -> Option<String> {
    std::fs::read_to_string(root.join(relative)).ok()
}

fn count_matches(haystack: &str, needle: &str, case_sensitive: bool) -> u64 {
    if case_sensitive {
        haystack.matches(needle).count() as u64
    } else {
        let lower_h = haystack.to_lowercase();
        let lower_n = needle.to_lowercase();
        lower_h.matches(&lower_n).count() as u64
    }
}

fn scope_allows(scope: &[String], relative: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    scope
        .iter()
        .any(|s| relative == s || relative.starts_with(&format!("{s}/")) || s.is_empty())
}

fn optional_string_list(
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Vec<String>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    VmValue::String(s) => out.push(s.to_string()),
                    other => {
                        return Err(HostlibError::InvalidParameter {
                            builtin: BUILTIN_QUERY,
                            param: key,
                            message: format!(
                                "expected list of strings, got element {}",
                                other.type_name()
                            ),
                        });
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_QUERY,
            param: key,
            message: format!("expected list of strings, got {}", other.type_name()),
        }),
    }
}

struct Hit {
    path: String,
    score: f64,
    match_count: u64,
}

fn hit_to_value(hit: Hit) -> VmValue {
    let Hit {
        path,
        score,
        match_count,
    } = hit;
    build_dict([
        ("path", str_value(&path)),
        ("score", VmValue::Float(score)),
        ("match_count", VmValue::Int(match_count as i64)),
    ])
}

fn import_entry(module: &str, resolved: Option<&str>, kind: &str) -> VmValue {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    map.insert("module".into(), str_value(module));
    map.insert(
        "resolved_path".into(),
        match resolved {
            Some(p) => str_value(p),
            None => VmValue::Nil,
        },
    );
    map.insert("kind".into(), str_value(kind));
    VmValue::Dict(Rc::new(map))
}

fn empty_query_response() -> VmValue {
    build_dict([
        ("results", VmValue::List(Rc::new(Vec::new()))),
        ("truncated", VmValue::Bool(false)),
    ])
}

fn empty_stats_response() -> VmValue {
    build_dict([
        ("indexed_files", VmValue::Int(0)),
        ("trigrams", VmValue::Int(0)),
        ("words", VmValue::Int(0)),
        ("memory_bytes", VmValue::Int(0)),
        ("last_rebuild_unix_ms", VmValue::Nil),
    ])
}

fn empty_imports_response(path: &str) -> VmValue {
    build_dict([
        ("path", str_value(path)),
        ("imports", VmValue::List(Rc::new(Vec::new()))),
    ])
}

fn empty_importers_response(module: &str) -> VmValue {
    build_dict([
        ("module", str_value(module)),
        ("importers", VmValue::List(Rc::new(Vec::new()))),
    ])
}
