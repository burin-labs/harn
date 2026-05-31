//! Host-builtin handlers for the `code_index` module.
//!
//! Each handler shape mirrors the schema in
//! `schemas/code_index/<method>.{request,response}.json`. A single shared
//! [`SharedIndex`] cell is captured by the closure of every handler so
//! every builtin observes the same in-memory state. The `current_agent_id`
//! op also reads from the capability's `current_agent` slot, but for
//! every other op the index mutex is the source of truth.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use harn_vm::VmValue;

use super::agents::AgentId;
use super::file_table::{fnv1a64, FileId};
use super::imports;
use super::state::{now_unix_ms, IndexState};
use super::trigram;
use super::versions::EditOp;
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int_list, optional_string, optional_string_list,
    require_string, str_value,
};

/// Shared, mutable cell carrying the (at most one) live workspace index.
/// `Mutex` rather than `RwLock` because rebuilds flip the slot wholesale
/// and every mutating op (record_edit, agent_register, lock_try, etc.)
/// needs exclusive access. Single-threaded VM scripts pay no real cost
/// from the choice; embedders that fan out across threads are still
/// safe because the mutex serialises everyone.
pub type SharedIndex = Arc<Mutex<Option<IndexState>>>;

// === Builtin name constants ===
//
// Every handler routes through one of these. They double as the module's
// public surface area so cross-repo schema-drift tests can discover them
// without scraping source.

pub(super) const BUILTIN_QUERY: &str = "hostlib_code_index_query";
pub(super) const BUILTIN_REBUILD: &str = "hostlib_code_index_rebuild";
pub(super) const BUILTIN_STATS: &str = "hostlib_code_index_stats";
pub(super) const BUILTIN_IMPORTS_FOR: &str = "hostlib_code_index_imports_for";
pub(super) const BUILTIN_IMPORTERS_OF: &str = "hostlib_code_index_importers_of";

pub(super) const BUILTIN_PATH_TO_ID: &str = "hostlib_code_index_path_to_id";
pub(super) const BUILTIN_ID_TO_PATH: &str = "hostlib_code_index_id_to_path";
pub(super) const BUILTIN_FILE_IDS: &str = "hostlib_code_index_file_ids";
pub(super) const BUILTIN_FILE_META: &str = "hostlib_code_index_file_meta";
pub(super) const BUILTIN_FILE_HASH: &str = "hostlib_code_index_file_hash";

pub(super) const BUILTIN_READ_RANGE: &str = "hostlib_code_index_read_range";
pub(super) const BUILTIN_REINDEX_FILE: &str = "hostlib_code_index_reindex_file";
pub(super) const BUILTIN_TRIGRAM_QUERY: &str = "hostlib_code_index_trigram_query";
pub(super) const BUILTIN_EXTRACT_TRIGRAMS: &str = "hostlib_code_index_extract_trigrams";
pub(super) const BUILTIN_WORD_GET: &str = "hostlib_code_index_word_get";
pub(super) const BUILTIN_DEPS_GET: &str = "hostlib_code_index_deps_get";
pub(super) const BUILTIN_OUTLINE_GET: &str = "hostlib_code_index_outline_get";

pub(super) const BUILTIN_CURRENT_SEQ: &str = "hostlib_code_index_current_seq";
pub(super) const BUILTIN_CHANGES_SINCE: &str = "hostlib_code_index_changes_since";
pub(super) const BUILTIN_VERSION_RECORD: &str = "hostlib_code_index_version_record";

pub(super) const BUILTIN_AGENT_REGISTER: &str = "hostlib_code_index_agent_register";
pub(super) const BUILTIN_AGENT_HEARTBEAT: &str = "hostlib_code_index_agent_heartbeat";
pub(super) const BUILTIN_AGENT_UNREGISTER: &str = "hostlib_code_index_agent_unregister";
pub(super) const BUILTIN_LOCK_TRY: &str = "hostlib_code_index_lock_try";
pub(super) const BUILTIN_LOCK_RELEASE: &str = "hostlib_code_index_lock_release";
pub(super) const BUILTIN_STATUS: &str = "hostlib_code_index_status";
pub(super) const BUILTIN_CURRENT_AGENT_ID: &str = "hostlib_code_index_current_agent_id";

pub(super) const BUILTIN_CYPHER: &str = "hostlib_code_index_cypher";
pub(super) const BUILTIN_BRANCH_OVERLAY: &str = "hostlib_code_index_branch_overlay";
pub(super) const BUILTIN_FRESHNESS: &str = "hostlib_code_index_freshness";

// === Search / rebuild / stats ===

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
    let max_results = optional_positive_usize(BUILTIN_QUERY, dict, "max_results")?.unwrap_or(100);
    let scope = optional_string_list(BUILTIN_QUERY, dict, "scope")?;

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
    let truncated = hits.len() > max_results;
    if truncated {
        hits.truncate(max_results);
    }
    Ok(build_dict([
        (
            "results",
            VmValue::List(Arc::new(hits.into_iter().map(hit_to_value).collect())),
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
        ("imports", VmValue::List(Arc::new(entries))),
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
            VmValue::List(Arc::new(importers.into_iter().map(str_value).collect())),
        ),
    ]))
}

// === File table accessors ===

pub(super) fn run_path_to_id(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_PATH_TO_ID, args)?;
    let path = require_string(BUILTIN_PATH_TO_ID, raw.as_ref(), "path")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let id = guard.as_ref().and_then(|s| s.lookup_path(&path));
    Ok(match id {
        Some(id) => VmValue::Int(id as i64),
        None => VmValue::Nil,
    })
}

pub(super) fn run_id_to_path(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_ID_TO_PATH, args)?;
    let id = require_positive_file_id(BUILTIN_ID_TO_PATH, raw.as_ref(), "file_id")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let path = guard
        .as_ref()
        .and_then(|s| s.files.get(&id))
        .map(|f| f.relative_path.clone());
    Ok(match path {
        Some(p) => str_value(&p),
        None => VmValue::Nil,
    })
}

pub(super) fn run_file_ids(
    index: &SharedIndex,
    _args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let guard = index.lock().expect("code_index mutex poisoned");
    let mut ids: Vec<FileId> = guard
        .as_ref()
        .map(|s| s.files.keys().copied().collect())
        .unwrap_or_default();
    ids.sort_unstable();
    Ok(VmValue::List(Arc::new(
        ids.into_iter().map(|id| VmValue::Int(id as i64)).collect(),
    )))
}

pub(super) fn run_file_meta(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_FILE_META, args)?;
    let dict = raw.as_ref();
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(VmValue::Nil);
    };
    let id_opt: Option<FileId> = if dict.contains_key("file_id") {
        Some(require_positive_file_id(
            BUILTIN_FILE_META,
            dict,
            "file_id",
        )?)
    } else if let Some(VmValue::String(p)) = dict.get("path") {
        state.lookup_path(p)
    } else {
        return Err(HostlibError::MissingParameter {
            builtin: BUILTIN_FILE_META,
            param: "file_id|path",
        });
    };
    let Some(id) = id_opt else {
        return Ok(VmValue::Nil);
    };
    let Some(file) = state.files.get(&id) else {
        return Ok(VmValue::Nil);
    };
    let last_edit_seq = state
        .versions
        .last_entry(&file.relative_path)
        .map(|e| e.seq)
        .unwrap_or(0);
    Ok(build_dict([
        ("id", VmValue::Int(file.id as i64)),
        ("path", str_value(&file.relative_path)),
        ("language", str_value(&file.language)),
        ("size", VmValue::Int(file.size_bytes as i64)),
        ("line_count", VmValue::Int(file.line_count as i64)),
        ("hash", str_value(file.content_hash.to_string())),
        ("mtime_ms", VmValue::Int(file.mtime_ms)),
        ("last_edit_seq", VmValue::Int(last_edit_seq as i64)),
    ]))
}

pub(super) fn run_file_hash(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_FILE_HASH, args)?;
    let path = require_string(BUILTIN_FILE_HASH, raw.as_ref(), "path")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(VmValue::Nil);
    };
    let Some(abs) = state.absolute_path(&path) else {
        return Ok(VmValue::Nil);
    };
    let bytes = match crate::fs::read(&abs, None) {
        Some(result) => result,
        None => std::fs::read(&abs),
    };
    match bytes {
        Ok(bytes) => Ok(str_value(fnv1a64(&bytes).to_string())),
        Err(_) => Ok(VmValue::Nil),
    }
}

// === Cached reads ===

pub(super) fn run_read_range(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_READ_RANGE, args)?;
    let dict = raw.as_ref();
    let path = require_string(BUILTIN_READ_RANGE, dict, "path")?;
    let start = optional_positive_i64(BUILTIN_READ_RANGE, dict, "start")?;
    let end = optional_positive_i64(BUILTIN_READ_RANGE, dict, "end")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let abs = match guard.as_ref() {
        Some(state) => {
            state
                .absolute_path(&path)
                .ok_or_else(|| HostlibError::InvalidParameter {
                    builtin: BUILTIN_READ_RANGE,
                    param: "path",
                    message: "path must stay within the indexed workspace root".to_string(),
                })?
        }
        None => PathBuf::from(&path),
    };
    drop(guard);

    let content_result = match crate::fs::read_to_string(&abs, None) {
        Some(result) => result,
        None => std::fs::read_to_string(&abs),
    };
    let content = match content_result {
        Ok(s) => s,
        Err(_) => {
            return Err(HostlibError::Backend {
                builtin: BUILTIN_READ_RANGE,
                message: format!("file not found: {path}"),
            })
        }
    };

    if start.is_none() && end.is_none() {
        return Ok(build_dict([("content", str_value(&content))]));
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let total = lines.len() as i64;
    let lo = (start.unwrap_or(1) - 1).max(0) as usize;
    let hi = end.unwrap_or(total).min(total).max(0) as usize;
    if lo >= hi {
        return Ok(build_dict([
            ("content", str_value("")),
            ("start", VmValue::Int((lo as i64) + 1)),
            ("end", VmValue::Int(hi as i64)),
        ]));
    }
    let slice = lines[lo..hi].join("\n");
    Ok(build_dict([
        ("content", str_value(&slice)),
        ("start", VmValue::Int((lo as i64) + 1)),
        ("end", VmValue::Int(hi as i64)),
    ]))
}

pub(super) fn run_reindex_file(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_REINDEX_FILE, args)?;
    let path = require_string(BUILTIN_REINDEX_FILE, raw.as_ref(), "path")?;
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_mut() else {
        return Ok(build_dict([
            ("indexed", VmValue::Bool(false)),
            ("file_id", VmValue::Nil),
        ]));
    };
    let Some(abs) = state.absolute_path(&path) else {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN_REINDEX_FILE,
            param: "path",
            message: "path must stay within the indexed workspace root".to_string(),
        });
    };
    let id = state.reindex_file(&abs);
    Ok(build_dict([
        ("indexed", VmValue::Bool(id.is_some())),
        (
            "file_id",
            id.map(|i| VmValue::Int(i as i64)).unwrap_or(VmValue::Nil),
        ),
    ]))
}

pub(super) fn run_trigram_query(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_TRIGRAM_QUERY, args)?;
    let dict = raw.as_ref();
    let trigrams_raw = optional_int_list(BUILTIN_TRIGRAM_QUERY, dict, "trigrams")?;
    let max_files = optional_positive_usize(BUILTIN_TRIGRAM_QUERY, dict, "max_files")?;
    let mut trigrams = Vec::with_capacity(trigrams_raw.len());
    for n in trigrams_raw {
        if n < 0 {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN_TRIGRAM_QUERY,
                param: "trigrams",
                message: "entries must be >= 0".to_string(),
            });
        }
        trigrams.push(n as u32);
    }
    let guard = index.lock().expect("code_index mutex poisoned");
    let mut ids: Vec<FileId> = match guard.as_ref() {
        Some(state) => state.trigrams.query(&trigrams).into_iter().collect(),
        None => Vec::new(),
    };
    ids.sort_unstable();
    if let Some(limit) = max_files {
        ids.truncate(limit);
    }
    Ok(VmValue::List(Arc::new(
        ids.into_iter().map(|id| VmValue::Int(id as i64)).collect(),
    )))
}

pub(super) fn run_extract_trigrams(
    _index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_EXTRACT_TRIGRAMS, args)?;
    let query = require_string(BUILTIN_EXTRACT_TRIGRAMS, raw.as_ref(), "query")?;
    let mut tgs = trigram::query_trigrams(&query);
    tgs.sort_unstable();
    Ok(VmValue::List(Arc::new(
        tgs.into_iter().map(|n| VmValue::Int(n as i64)).collect(),
    )))
}

pub(super) fn run_word_get(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_WORD_GET, args)?;
    let word = require_string(BUILTIN_WORD_GET, raw.as_ref(), "word")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let hits: Vec<VmValue> = match guard.as_ref() {
        Some(state) => state
            .words
            .get(&word)
            .iter()
            .map(|h| {
                build_dict([
                    ("file_id", VmValue::Int(h.file as i64)),
                    ("line", VmValue::Int(h.line as i64)),
                ])
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(VmValue::List(Arc::new(hits)))
}

pub(super) fn run_deps_get(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_DEPS_GET, args)?;
    let dict = raw.as_ref();
    let id = require_positive_file_id(BUILTIN_DEPS_GET, dict, "file_id")?;
    let direction = optional_string(BUILTIN_DEPS_GET, dict, "direction")?
        .unwrap_or_else(|| "importers".to_string());
    let guard = index.lock().expect("code_index mutex poisoned");
    let mut neighbors: Vec<FileId> = match guard.as_ref() {
        Some(state) => match direction.as_str() {
            "importers" => state.deps.importers_of(id),
            "imports" => state.deps.imports_of(id),
            _ => {
                return Err(HostlibError::InvalidParameter {
                    builtin: BUILTIN_DEPS_GET,
                    param: "direction",
                    message: format!("expected \"importers\" or \"imports\", got {direction:?}"),
                })
            }
        },
        None => Vec::new(),
    };
    neighbors.sort_unstable();
    Ok(VmValue::List(Arc::new(
        neighbors
            .into_iter()
            .map(|id| VmValue::Int(id as i64))
            .collect(),
    )))
}

pub(super) fn run_outline_get(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_OUTLINE_GET, args)?;
    let id = require_positive_file_id(BUILTIN_OUTLINE_GET, raw.as_ref(), "file_id")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let symbols: Vec<VmValue> = match guard.as_ref().and_then(|s| s.files.get(&id)) {
        Some(file) => file
            .symbols
            .iter()
            .map(|sym| {
                build_dict([
                    ("name", str_value(&sym.name)),
                    ("kind", str_value(&sym.kind)),
                    ("start_line", VmValue::Int(sym.start_line as i64)),
                    ("end_line", VmValue::Int(sym.end_line as i64)),
                    ("signature", str_value(&sym.signature)),
                ])
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(VmValue::List(Arc::new(symbols)))
}

// === Change log ===

pub(super) fn run_current_seq(
    index: &SharedIndex,
    _args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let guard = index.lock().expect("code_index mutex poisoned");
    let seq = guard.as_ref().map(|s| s.versions.current_seq).unwrap_or(0);
    Ok(VmValue::Int(seq as i64))
}

pub(super) fn run_changes_since(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_CHANGES_SINCE, args)?;
    let dict = raw.as_ref();
    let seq = optional_non_negative_u64(BUILTIN_CHANGES_SINCE, dict, "seq", 0)?;
    let limit = optional_positive_usize(BUILTIN_CHANGES_SINCE, dict, "limit")?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let records = match guard.as_ref() {
        Some(state) => state.versions.changes_since(seq, limit),
        None => Vec::new(),
    };
    Ok(VmValue::List(Arc::new(
        records
            .into_iter()
            .map(|r| {
                build_dict([
                    ("path", str_value(&r.path)),
                    ("seq", VmValue::Int(r.seq as i64)),
                    ("agent_id", VmValue::Int(r.agent_id as i64)),
                    ("op", str_value(r.op.as_str())),
                    ("hash", str_value(r.hash.to_string())),
                    ("size", VmValue::Int(r.size as i64)),
                    ("timestamp_ms", VmValue::Int(r.timestamp_ms)),
                ])
            })
            .collect(),
    )))
}

pub(super) fn run_version_record(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_VERSION_RECORD, args)?;
    let dict = raw.as_ref();
    let agent_id = require_non_negative_u64(BUILTIN_VERSION_RECORD, dict, "agent_id")?;
    let path = require_string(BUILTIN_VERSION_RECORD, dict, "path")?;
    let op_str =
        optional_string(BUILTIN_VERSION_RECORD, dict, "op")?.unwrap_or_else(|| "write".to_string());
    let op = EditOp::parse(&op_str).unwrap_or(EditOp::Write);
    let hash = parse_hash(BUILTIN_VERSION_RECORD, dict, "hash")?;
    let size = optional_non_negative_u64(BUILTIN_VERSION_RECORD, dict, "size", 0)?;
    let now = now_unix_ms();
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_VERSION_RECORD, &mut guard)?;
    let normalized = normalize_relative_path(state, &path);
    let seq = state
        .versions
        .record(normalized, agent_id, op, hash, size, now);
    state.agents.note_edit(agent_id, now);
    Ok(VmValue::Int(seq as i64))
}

// === Agent registry + locks ===

pub(super) fn run_agent_register(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_AGENT_REGISTER, args)?;
    let dict = raw.as_ref();
    let name = optional_string(BUILTIN_AGENT_REGISTER, dict, "name")?
        .unwrap_or_else(|| "agent".to_string());
    let requested_id = optional_positive_u64(BUILTIN_AGENT_REGISTER, dict, "agent_id")?;
    let now = now_unix_ms();
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_AGENT_REGISTER, &mut guard)?;
    let id = match requested_id {
        Some(id) => state.agents.register_with_id(id, name, now),
        None => state.agents.register(name, now),
    };
    Ok(VmValue::Int(id as i64))
}

pub(super) fn run_agent_heartbeat(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_AGENT_HEARTBEAT, args)?;
    let id = require_positive_u64(BUILTIN_AGENT_HEARTBEAT, raw.as_ref(), "agent_id")?;
    let now = now_unix_ms();
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_AGENT_HEARTBEAT, &mut guard)?;
    state.agents.heartbeat(id, now);
    Ok(VmValue::Bool(true))
}

pub(super) fn run_agent_unregister(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_AGENT_UNREGISTER, args)?;
    let id = require_positive_u64(BUILTIN_AGENT_UNREGISTER, raw.as_ref(), "agent_id")?;
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_AGENT_UNREGISTER, &mut guard)?;
    state.agents.unregister(id);
    Ok(VmValue::Bool(true))
}

pub(super) fn run_lock_try(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_LOCK_TRY, args)?;
    let dict = raw.as_ref();
    let agent_id = require_positive_u64(BUILTIN_LOCK_TRY, dict, "agent_id")?;
    let path = require_string(BUILTIN_LOCK_TRY, dict, "path")?;
    let ttl = optional_positive_i64(BUILTIN_LOCK_TRY, dict, "ttl_ms")?;
    let now = now_unix_ms();
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_LOCK_TRY, &mut guard)?;
    let granted = state.agents.try_lock(agent_id, &path, ttl, now);
    if granted {
        return Ok(build_dict([
            ("locked", VmValue::Bool(true)),
            ("holder", VmValue::Int(agent_id as i64)),
        ]));
    }
    let holder = state.agents.lock_holder(&path, now);
    Ok(build_dict([
        ("locked", VmValue::Bool(false)),
        (
            "holder",
            holder
                .map(|id| VmValue::Int(id as i64))
                .unwrap_or(VmValue::Nil),
        ),
    ]))
}

pub(super) fn run_lock_release(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_LOCK_RELEASE, args)?;
    let dict = raw.as_ref();
    let agent_id = require_positive_u64(BUILTIN_LOCK_RELEASE, dict, "agent_id")?;
    let path = require_string(BUILTIN_LOCK_RELEASE, dict, "path")?;
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_LOCK_RELEASE, &mut guard)?;
    state.agents.release_lock(agent_id, &path);
    Ok(VmValue::Bool(true))
}

pub(super) fn run_status(index: &SharedIndex, _args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let guard = index.lock().expect("code_index mutex poisoned");
    match guard.as_ref() {
        Some(state) => Ok(build_dict([
            ("file_count", VmValue::Int(state.files.len() as i64)),
            (
                "current_seq",
                VmValue::Int(state.versions.current_seq as i64),
            ),
            ("last_indexed_at_ms", VmValue::Int(state.last_built_unix_ms)),
            (
                "git_head",
                state
                    .git_head
                    .as_deref()
                    .map(str_value)
                    .unwrap_or(VmValue::Nil),
            ),
            (
                "agents",
                VmValue::List(Arc::new(
                    state
                        .agents
                        .agents()
                        .map(|info| {
                            build_dict([
                                ("id", VmValue::Int(info.id as i64)),
                                ("name", str_value(&info.name)),
                                (
                                    "state",
                                    str_value(match info.state {
                                        super::agents::AgentState::Active => "active",
                                        super::agents::AgentState::Crashed => "crashed",
                                        super::agents::AgentState::Gone => "gone",
                                    }),
                                ),
                                ("last_seen_ms", VmValue::Int(info.last_seen_ms)),
                                ("edit_count", VmValue::Int(info.edit_count as i64)),
                                ("lock_count", VmValue::Int(info.locked_paths.len() as i64)),
                            ])
                        })
                        .collect(),
                )),
            ),
        ])),
        None => Ok(build_dict([
            ("file_count", VmValue::Int(0)),
            ("current_seq", VmValue::Int(0)),
            ("last_indexed_at_ms", VmValue::Int(0)),
            ("git_head", VmValue::Nil),
            ("agents", VmValue::List(Arc::new(Vec::new()))),
        ])),
    }
}

pub(super) fn run_current_agent_id(
    slot: &Arc<Mutex<Option<AgentId>>>,
    _args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let guard = slot.lock().expect("current_agent slot poisoned");
    Ok(match *guard {
        Some(id) => VmValue::Int(id as i64),
        None => VmValue::Nil,
    })
}

// === Symbol graph: cypher, branch_overlay, freshness (issue #2434) ===

pub(super) fn run_cypher(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_CYPHER, args)?;
    let dict = raw.as_ref();
    let query = require_string(BUILTIN_CYPHER, dict, "query")?;

    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Ok(build_dict([
            ("rows", VmValue::List(Arc::new(Vec::new()))),
            ("overlay", VmValue::Nil),
        ]));
    };

    let graph = state.overlays.graph(&state.symbols);
    let rows = super::cypher::execute(&query, graph).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN_CYPHER,
        message: err.to_string(),
    })?;

    let rows_vm: Vec<VmValue> = rows
        .into_iter()
        .map(|row| {
            let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
            for (k, v) in row {
                map.insert(k, v.to_vm());
            }
            VmValue::Dict(Arc::new(map))
        })
        .collect();

    Ok(build_dict([
        ("rows", VmValue::List(Arc::new(rows_vm))),
        (
            "overlay",
            match state.overlays.active() {
                Some(name) => str_value(name),
                None => VmValue::Nil,
            },
        ),
    ]))
}

pub(super) fn run_branch_overlay(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_BRANCH_OVERLAY, args)?;
    let dict = raw.as_ref();
    let branch = optional_string(BUILTIN_BRANCH_OVERLAY, dict, "branch")?;
    let activate = optional_bool(BUILTIN_BRANCH_OVERLAY, dict, "activate", true)?;
    let action = optional_string(BUILTIN_BRANCH_OVERLAY, dict, "action")?;

    let mut guard = index.lock().expect("code_index mutex poisoned");
    let state = ensure_state(BUILTIN_BRANCH_OVERLAY, &mut guard)?;

    let mut reuse: f64 = 1.0;
    match action.as_deref().unwrap_or("activate") {
        "deactivate" => {
            state.overlays.activate(None);
        }
        "create" => {
            let branch_name = branch.ok_or(HostlibError::MissingParameter {
                builtin: BUILTIN_BRANCH_OVERLAY,
                param: "branch",
            })?;
            let mut overlay = super::overlay::BranchOverlay::new(&branch_name);
            overlay.materialize(&state.symbols);
            state.overlays.set(overlay);
            if activate {
                state.overlays.activate(Some(branch_name));
            }
            reuse = state.overlays.reuse_fraction(&state.symbols);
        }
        "activate" => {
            let branch_name = branch.ok_or(HostlibError::MissingParameter {
                builtin: BUILTIN_BRANCH_OVERLAY,
                param: "branch",
            })?;
            // If the overlay doesn't exist, create an empty pass-through
            // one — the base graph then serves it untouched, giving the
            // 100% reuse / 0-change baseline.
            if state.overlays.get(&branch_name).is_none() {
                let mut overlay = super::overlay::BranchOverlay::new(&branch_name);
                overlay.materialize(&state.symbols);
                state.overlays.set(overlay);
            }
            state.overlays.activate(Some(branch_name));
            reuse = state.overlays.reuse_fraction(&state.symbols);
        }
        other => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN_BRANCH_OVERLAY,
                param: "action",
                message: format!("expected one of activate|deactivate|create, got `{other}`"),
            })
        }
    }

    Ok(build_dict([
        (
            "active",
            match state.overlays.active() {
                Some(name) => str_value(name),
                None => VmValue::Nil,
            },
        ),
        ("reuse_fraction", VmValue::Float(reuse)),
    ]))
}

pub(super) fn run_freshness(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_FRESHNESS, args)?;
    let dict = raw.as_ref();
    let path = require_string(BUILTIN_FRESHNESS, dict, "path")?;

    let guard = index.lock().expect("code_index mutex poisoned");
    let state = guard.as_ref().ok_or_else(|| HostlibError::Backend {
        builtin: BUILTIN_FRESHNESS,
        message: "code index has not been initialised — call \
            `hostlib_code_index_rebuild` first"
            .to_string(),
    })?;

    let normalized = normalize_relative_path(state, &path);
    let file = state
        .lookup_path(&normalized)
        .and_then(|id| state.files.get(&id));
    let Some(file) = file else {
        return Ok(unknown_freshness_response(&path));
    };

    let abs = state.root.join(&file.relative_path);
    let (disk_mtime, disk_hash) = match std::fs::read(&abs) {
        Ok(bytes) => {
            let hash = fnv1a64(&bytes);
            let mtime = std::fs::metadata(&abs)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            (mtime, Some(hash))
        }
        Err(_) => (0, None),
    };
    let stale = disk_hash != Some(file.content_hash);
    Ok(build_dict([
        ("path", str_value(&file.relative_path)),
        ("known", VmValue::Bool(true)),
        ("stale", VmValue::Bool(stale)),
        (
            "indexed_hash",
            VmValue::String(Arc::from(format!("{:016x}", file.content_hash).as_str())),
        ),
        ("indexed_mtime_ms", VmValue::Int(file.mtime_ms)),
        (
            "disk_hash",
            match disk_hash {
                Some(h) => VmValue::String(Arc::from(format!("{h:016x}").as_str())),
                None => VmValue::Nil,
            },
        ),
        ("disk_mtime_ms", VmValue::Int(disk_mtime)),
    ]))
}

// === Helpers ===

fn ensure_state<'a>(
    builtin: &'static str,
    guard: &'a mut std::sync::MutexGuard<'_, Option<IndexState>>,
) -> Result<&'a mut IndexState, HostlibError> {
    if guard.is_none() {
        return Err(HostlibError::Backend {
            builtin,
            message: "code index has not been initialised — call \
                 `hostlib_code_index_rebuild` or restore from a snapshot first"
                .to_string(),
        });
    }
    Ok(guard.as_mut().unwrap())
}

fn parse_hash(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<u64, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(0),
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(VmValue::Int(n)) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 0, got {n}"),
        }),
        Some(VmValue::String(s)) => s
            .parse::<u64>()
            .map_err(|_| HostlibError::InvalidParameter {
                builtin,
                param: key,
                message: format!("expected u64-parseable string, got {s:?}"),
            }),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!(
                "expected integer or numeric string, got {}",
                other.type_name()
            ),
        }),
    }
}

fn require_positive_u64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<u64, HostlibError> {
    let raw = require_non_negative_u64(builtin, dict, key)?;
    if raw == 0 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: "must be >= 1".to_string(),
        });
    }
    Ok(raw)
}

fn require_positive_file_id(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<FileId, HostlibError> {
    let raw = require_positive_u64(builtin, dict, key)?;
    FileId::try_from(raw).map_err(|_| HostlibError::InvalidParameter {
        builtin,
        param: key,
        message: "does not fit in file id".to_string(),
    })
}

fn require_non_negative_u64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<u64, HostlibError> {
    match dict.get(key) {
        Some(VmValue::Int(n)) if *n >= 0 => Ok(*n as u64),
        Some(VmValue::Float(f)) if f.fract() == 0.0 && *f >= 0.0 => Ok(*f as u64),
        Some(VmValue::Int(n)) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 0, got {n}"),
        }),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", other.type_name()),
        }),
        None => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

fn optional_positive_u64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<u64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(_) => require_positive_u64(builtin, dict, key).map(Some),
    }
}

fn optional_non_negative_u64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
    default: u64,
) -> Result<u64, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(_) => require_non_negative_u64(builtin, dict, key),
    }
}

fn optional_positive_i64(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<i64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(n)) if *n >= 1 => Ok(Some(*n)),
        Some(VmValue::Float(f)) if f.fract() == 0.0 && *f >= 1.0 => Ok(Some(*f as i64)),
        Some(VmValue::Int(n)) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("must be >= 1, got {n}"),
        }),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected integer, got {}", other.type_name()),
        }),
    }
}

fn optional_positive_usize(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<usize>, HostlibError> {
    match optional_positive_u64(builtin, dict, key)? {
        Some(value) => {
            usize::try_from(value)
                .map(Some)
                .map_err(|_| HostlibError::InvalidParameter {
                    builtin,
                    param: key,
                    message: "does not fit in usize".to_string(),
                })
        }
        None => Ok(None),
    }
}

/// Re-export of [`normalize_relative_path`] for sibling modules
/// (e.g. [`super::rename`]). Inputs may be a workspace-relative path,
/// an absolute path inside the workspace, or an unknown path; the
/// returned string is always workspace-relative when resolvable and
/// falls back to the raw input otherwise.
pub(super) fn normalize_relative_path_for(state: &IndexState, path: &str) -> String {
    normalize_relative_path(state, path)
}

fn normalize_relative_path(state: &IndexState, path: &str) -> String {
    if let Some(rel) = state
        .lookup_path(path)
        .and_then(|id| state.files.get(&id))
        .map(|f| f.relative_path.clone())
    {
        return rel;
    }
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        if let Ok(rel) = p.strip_prefix(&state.root) {
            return rel.to_string_lossy().replace('\\', "/");
        }
    }
    path.to_string()
}

fn candidates_for(state: &IndexState, needle: &str) -> Vec<FileId> {
    if needle.len() >= 3 {
        let trigrams = trigram::query_trigrams(needle);
        return state.trigrams.query(&trigrams).into_iter().collect();
    }
    state.files.keys().copied().collect()
}

fn read_file_text(root: &std::path::Path, relative: &str) -> Option<String> {
    let path = root.join(relative);
    match crate::fs::read_to_string(&path, None) {
        Some(result) => result.ok(),
        None => std::fs::read_to_string(path).ok(),
    }
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
    VmValue::Dict(Arc::new(map))
}

fn empty_query_response() -> VmValue {
    build_dict([
        ("results", VmValue::List(Arc::new(Vec::new()))),
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
        ("imports", VmValue::List(Arc::new(Vec::new()))),
    ])
}

fn empty_importers_response(module: &str) -> VmValue {
    build_dict([
        ("module", str_value(module)),
        ("importers", VmValue::List(Arc::new(Vec::new()))),
    ])
}

fn unknown_freshness_response(path: &str) -> VmValue {
    build_dict([
        ("path", str_value(path)),
        ("known", VmValue::Bool(false)),
        ("stale", VmValue::Bool(true)),
        ("indexed_hash", VmValue::Nil),
        ("indexed_mtime_ms", VmValue::Nil),
        ("disk_hash", VmValue::Nil),
        ("disk_mtime_ms", VmValue::Nil),
    ])
}
