//! Additive, read-only secondary roots for the code index (issue #2403
//! follow-up).
//!
//! The primary [`IndexState`] (built by `hostlib_code_index_rebuild`) owns
//! exactly one writable workspace root and flips its slot wholesale on
//! every rebuild. That is correct for the project under edit, but it means
//! a caller cannot also index *dependency* / SDK roots (e.g. the macOS
//! IOKit headers that declare `kIOPSTimeToFullChargeKey`) without
//! clobbering the project index.
//!
//! This module adds a parallel, **read-only** set of secondary
//! [`IndexState`]s that live beside the primary in the capability. They are
//! merged into path-based query results so library API symbols become
//! discoverable, while staying entirely out of the project's writable
//! scope:
//!
//! - They are never mutated by `version_record`, `reindex_file`,
//!   `agent_*`, `lock_*`, or `rename_symbol` — those builtins only ever
//!   touch the primary slot.
//! - `read_range` will resolve a path inside a read-only root (so a symbol
//!   discovered there can be read), but every *write* path (rename,
//!   reindex, version log, locks) still rejects out-of-project paths
//!   exactly as before, because those paths are not in the primary index.
//!
//! Concurrency mirrors the primary: a single `Arc<Mutex<Vec<IndexState>>>`
//! cell, so the same serialised view is shared by every Harn VM wired
//! against the capability.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::state::IndexState;
use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_bool, optional_string_list, str_value};

/// Shared cell holding the read-only secondary indexes. Each entry is a
/// fully-built [`IndexState`] anchored at one dependency root. Empty until
/// `hostlib_code_index_add_readonly_roots` is called.
pub type ReadonlyRoots = Arc<Mutex<Vec<IndexState>>>;

pub(super) const BUILTIN_ADD_READONLY_ROOTS: &str = "hostlib_code_index_add_readonly_roots";

/// Build and merge one or more dependency roots into the read-only set.
///
/// Idempotent per root: re-adding a root that is already present rebuilds
/// that entry in place rather than appending a duplicate. `replace: true`
/// clears the existing set first (so a caller can swap the whole dependency
/// fan-out without accumulating stale roots).
pub(super) fn run_add_readonly_roots(
    readonly: &ReadonlyRoots,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN_ADD_READONLY_ROOTS, args)?;
    let dict = raw.as_ref();
    let roots = optional_string_list(BUILTIN_ADD_READONLY_ROOTS, dict, "roots")?;
    let replace = optional_bool(BUILTIN_ADD_READONLY_ROOTS, dict, "replace", false)?;

    let mut guard = readonly.lock().expect("readonly roots mutex poisoned");
    if replace {
        guard.clear();
    }

    let mut added: Vec<VmValue> = Vec::with_capacity(roots.len());
    let mut total_files: usize = 0;
    for raw_root in roots {
        let root = PathBuf::from(&raw_root);
        if !root.exists() {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN_ADD_READONLY_ROOTS,
                param: "roots",
                message: format!("path `{}` does not exist", root.display()),
            });
        }
        if !root.is_dir() {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN_ADD_READONLY_ROOTS,
                param: "roots",
                message: format!("path `{}` is not a directory", root.display()),
            });
        }
        let (state, outcome) = IndexState::build_from_root(&root);
        let files_indexed = outcome.files_indexed as i64;
        total_files += state.files.len();
        // Idempotent: replace any existing entry for the same canonical
        // root rather than appending a duplicate.
        let canonical = state.root.clone();
        if let Some(slot) = guard.iter_mut().find(|s| s.root == canonical) {
            *slot = state;
        } else {
            guard.push(state);
        }
        added.push(build_dict([
            ("root", str_value(canonical.to_string_lossy().as_ref())),
            ("files_indexed", VmValue::Int(files_indexed)),
        ]));
    }

    Ok(build_dict([
        ("roots", VmValue::List(Arc::new(added))),
        ("readonly_root_count", VmValue::Int(guard.len() as i64)),
        ("readonly_files_indexed", VmValue::Int(total_files as i64)),
    ]))
}

/// Resolve `rel_or_abs` against the primary index first, then fall back to
/// every read-only secondary root. Returns the canonical absolute path of
/// the first index that contains the file. Used by `read_range` so a symbol
/// discovered in a dependency root can be read back.
pub(super) fn resolve_read_path(
    primary: &SharedIndex,
    readonly: &ReadonlyRoots,
    path: &str,
) -> Option<PathBuf> {
    // Resolution order, preserving the pre-#2403 contract:
    //   1. An *existing* file inside the primary workspace root.
    //   2. An *existing* file inside any read-only dependency root.
    //   3. As a fallback, the primary root's resolution even if the file
    //      does not exist — so a missing in-workspace path still fails at
    //      the read step with "file not found" exactly as before, rather
    //      than being rejected as out-of-scope.
    // `absolute_path` confines every candidate to a known root and accepts
    // not-yet-existing paths (it is shared with write paths), so the
    // existence filter is what lets a dependency-only path win over the
    // phantom project path it would otherwise resolve to.
    let primary_resolved = {
        let guard = primary.lock().expect("code_index mutex poisoned");
        guard.as_ref().and_then(|state| state.absolute_path(path))
    };
    if let Some(abs) = primary_resolved.as_ref().filter(|p| p.exists()) {
        return Some(abs.clone());
    }
    {
        let guard = readonly.lock().expect("readonly roots mutex poisoned");
        if let Some(abs) = guard
            .iter()
            .find_map(|state| state.absolute_path(path).filter(|p| p.exists()))
        {
            return Some(abs);
        }
    }
    primary_resolved
}

/// Run `query`-style scoring over every read-only secondary root, tagging
/// each hit with its `root` so callers can disambiguate a dependency hit
/// from a project hit. Returns hits already merged with whatever the
/// primary index produced. Hits keep the same `path`/`score`/`match_count`
/// shape as the primary query plus a `root` field; primary hits carry
/// `root: nil`.
pub(super) fn query_readonly_hits(
    readonly: &ReadonlyRoots,
    needle: &str,
    case_sensitive: bool,
) -> Vec<super::builtins::Hit> {
    let guard = readonly.lock().expect("readonly roots mutex poisoned");
    let mut hits: Vec<super::builtins::Hit> = Vec::new();
    for state in guard.iter() {
        super::builtins::collect_hits_into(state, needle, case_sensitive, &mut hits);
    }
    hits
}
