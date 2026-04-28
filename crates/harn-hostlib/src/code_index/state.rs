//! Per-workspace index state.
//!
//! Owns the file table, trigram index, word index, dep graph, version
//! log, and agent registry for one workspace root. Construction is via
//! [`IndexState::build_from_root`], which walks the workspace, reads
//! every indexable file, and populates every sub-index in a single pass
//! before resolving imports.
//!
//! Single-file mutations (`reindex_file`, `remove_file`) flow through
//! the same paths so the sub-indexes stay consistent across the
//! incremental host ops drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::agents::AgentRegistry;
use super::file_table::{fnv1a64, FileId, IndexedFile};
use super::graph::DepGraph;
use super::imports;
use super::trigram::TrigramIndex;
use super::versions::VersionLog;
use super::walker::{is_indexable_file, language_for_extension, walk_indexable, MAX_FILE_BYTES};
use super::words::WordIndex;

/// In-memory index for one workspace. Composed from the per-file table,
/// the trigram + word sub-indexes, the dep graph, the append-only version
/// log, and the agent registry.
pub struct IndexState {
    /// Canonicalised workspace root.
    pub root: PathBuf,
    /// File table keyed on stable id.
    pub files: HashMap<FileId, IndexedFile>,
    /// Workspace-relative path → stable id.
    pub path_to_id: HashMap<String, FileId>,
    /// Trigram posting list.
    pub trigrams: TrigramIndex,
    /// Identifier-token inverted index.
    pub words: WordIndex,
    /// Forward + reverse import graph.
    pub deps: DepGraph,
    /// Append-only log of file mutations.
    pub versions: VersionLog,
    /// Live agents + advisory locks.
    pub agents: AgentRegistry,
    /// Wall-clock timestamp (ms since epoch) of the most recent rebuild.
    pub last_built_unix_ms: i64,
    /// Best-effort `HEAD` SHA, or `None` if the workspace isn't a git repo.
    pub git_head: Option<String>,
    next_id: FileId,
}

/// Summary returned from `IndexState::build_from_root`.
#[derive(Debug, Default)]
pub struct BuildOutcome {
    /// Files that passed every filter and were ingested.
    pub files_indexed: u64,
    /// Files that matched the filename filter but couldn't be read or
    /// were too large.
    pub files_skipped: u64,
}

impl IndexState {
    /// Build a fresh index over `root`. Returns the populated state plus a
    /// summary of how many files were indexed vs skipped.
    pub fn build_from_root(root: &Path) -> (Self, BuildOutcome) {
        let canonical_root = canonicalize(root);
        let mut state = IndexState {
            root: canonical_root.clone(),
            files: HashMap::new(),
            path_to_id: HashMap::new(),
            trigrams: TrigramIndex::new(),
            words: WordIndex::new(),
            deps: DepGraph::new(),
            versions: VersionLog::new(),
            agents: AgentRegistry::new(),
            last_built_unix_ms: now_unix_ms(),
            git_head: read_git_head(&canonical_root),
            next_id: 1,
        };
        let mut outcome = BuildOutcome::default();
        let mut to_resolve: Vec<(FileId, String)> = Vec::new();
        walk_indexable(&canonical_root, |abs| match state.ingest(abs) {
            Some(file_id) => {
                outcome.files_indexed += 1;
                if let Some(file) = state.files.get(&file_id) {
                    to_resolve.push((file_id, file.relative_path.clone()));
                }
            }
            None => {
                outcome.files_skipped += 1;
            }
        });
        for (id, rel) in to_resolve {
            state.rebuild_deps(id, &rel);
        }
        (state, outcome)
    }

    /// Re-index a single file by its absolute path. Returns the id of the
    /// affected file (newly assigned or existing). If the file no longer
    /// exists or fails the indexability/sensitivity filter, any existing
    /// entry under that path is removed and `None` is returned.
    pub fn reindex_file(&mut self, abs: &Path) -> Option<FileId> {
        if !abs.exists() {
            self.remove_file_path(abs);
            return None;
        }
        if !is_indexable_file(abs) || super::walker::is_sensitive_path(abs) {
            self.remove_file_path(abs);
            return None;
        }
        let id = self.ingest(abs)?;
        let rel = self
            .files
            .get(&id)
            .map(|f| f.relative_path.clone())
            .unwrap_or_default();
        if !rel.is_empty() {
            self.rebuild_deps(id, &rel);
        }
        Some(id)
    }

    /// Remove an existing file from every sub-index. No-op when the file
    /// isn't tracked.
    pub fn remove_file_path(&mut self, abs: &Path) {
        let Some(rel) = relative_path(&self.root, abs) else {
            return;
        };
        let Some(id) = self.path_to_id.remove(&rel) else {
            return;
        };
        self.files.remove(&id);
        self.trigrams.remove_file(id);
        self.words.remove_file(id);
        self.deps.remove_file(id);
    }

    fn ingest(&mut self, abs: &Path) -> Option<FileId> {
        if !is_indexable_file(abs) {
            return None;
        }
        let metadata = std::fs::metadata(abs).ok()?;
        if metadata.len() > MAX_FILE_BYTES {
            return None;
        }
        let content = std::fs::read_to_string(abs).ok()?;
        if content.len() > MAX_FILE_BYTES as usize {
            return None;
        }
        let rel = relative_path(&self.root, abs)?;
        let hash = fnv1a64(content.as_bytes());
        let id = match self.path_to_id.get(&rel) {
            Some(existing_id) => {
                if let Some(file) = self.files.get(existing_id) {
                    if file.content_hash == hash {
                        return Some(*existing_id);
                    }
                }
                *existing_id
            }
            None => {
                let id = self.next_id;
                self.next_id = self.next_id.checked_add(1).expect("FileId overflow");
                self.path_to_id.insert(rel.clone(), id);
                id
            }
        };

        let ext = abs
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let language = language_for_extension(&ext).to_string();
        let imports = imports::extract_imports(&content, &language);
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let line_count = if content.is_empty() {
            0
        } else {
            content.split('\n').count() as u32
        };

        let file = IndexedFile {
            id,
            relative_path: rel,
            language,
            size_bytes: content.len() as u64,
            line_count,
            content_hash: hash,
            mtime_ms,
            symbols: Vec::new(),
            imports,
        };
        self.trigrams.index_file(id, &content);
        self.words.index_file(id, &content);
        self.files.insert(id, file);
        Some(id)
    }

    fn rebuild_deps(&mut self, id: FileId, relative_path: &str) {
        let Some(file) = self.files.get(&id).cloned() else {
            return;
        };
        let resolved = imports::resolve(
            &file.imports,
            relative_path,
            &file.language,
            &self.path_to_id,
        );
        self.deps
            .set_edges(id, resolved.resolved, resolved.unresolved);
    }

    /// Look up a file by either its workspace-relative path or its
    /// absolute path inside the workspace root.
    pub fn lookup_path(&self, raw: &str) -> Option<FileId> {
        if let Some(id) = self.path_to_id.get(raw) {
            return Some(*id);
        }
        let path = Path::new(raw);
        if path.is_absolute() {
            if let Some(rel) = relative_path(&self.root, path) {
                if let Some(id) = self.path_to_id.get(&rel) {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Estimate the resident memory footprint of every sub-index. Cheap
    /// order-of-magnitude figure surfaced by the `stats` builtin.
    pub fn estimated_bytes(&self) -> usize {
        let file_bytes: usize = self
            .files
            .values()
            .map(|f| f.relative_path.len() + f.imports.iter().map(|s| s.len()).sum::<usize>() + 64)
            .sum();
        self.trigrams.estimated_bytes() + self.words.estimated_bytes() + file_bytes
    }

    /// Resolve a workspace-relative path against the canonical root.
    /// Used by host builtins that take a `path` argument and need to
    /// open the underlying file (e.g. `read_range`, `file_hash`).
    pub fn absolute_path(&self, rel_or_abs: &str) -> PathBuf {
        let p = Path::new(rel_or_abs);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    /// Construct an empty [`IndexState`] anchored at `root`. Used by the
    /// snapshot path which fills in the sub-indexes itself.
    pub(crate) fn empty(root: PathBuf) -> Self {
        Self {
            root,
            files: HashMap::new(),
            path_to_id: HashMap::new(),
            trigrams: TrigramIndex::new(),
            words: WordIndex::new(),
            deps: DepGraph::new(),
            versions: VersionLog::new(),
            agents: AgentRegistry::new(),
            last_built_unix_ms: 0,
            git_head: None,
            next_id: 1,
        }
    }

    /// Borrow the `next_id` counter — exposed for snapshot serialisation.
    pub(crate) fn next_file_id_internal(&self) -> FileId {
        self.next_id
    }

    /// Restore the `next_id` counter from a serialised snapshot.
    pub(crate) fn set_next_file_id(&mut self, id: FileId) {
        self.next_id = id.max(1);
    }
}

/// Return the current wall-clock time in milliseconds since the Unix
/// epoch.
pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn canonicalize(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Compute `abs` relative to `root`, using `/` separators. Returns `None`
/// if `abs` is not inside `root`. Handles the missing-file case (where
/// `canonicalize` would fail) by canonicalising the longest existing
/// prefix and re-attaching the missing tail — so `remove_file_path` keeps
/// working when the underlying path has just been deleted.
pub(crate) fn relative_path(root: &Path, abs: &Path) -> Option<String> {
    let canonical_abs = canonicalize_existing(abs);
    let stripped = canonical_abs.strip_prefix(root).ok()?;
    Some(stripped.to_string_lossy().replace('\\', "/"))
}

fn canonicalize_existing(abs: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(abs) {
        return c;
    }
    // Walk upward until we find a parent that does exist; canonicalise
    // that and re-attach the missing tail.
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = abs;
    loop {
        if cursor.exists() {
            if let Ok(canonical) = std::fs::canonicalize(cursor) {
                let mut out = canonical;
                for piece in tail.iter().rev() {
                    out = out.join(piece);
                }
                return out;
            }
            break;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                tail.push(name);
                cursor = parent;
            }
            _ => break,
        }
    }
    abs.to_path_buf()
}

fn read_git_head(workspace_root: &Path) -> Option<String> {
    let head = workspace_root.join(".git").join("HEAD");
    let txt = std::fs::read_to_string(&head).ok()?;
    let line = txt.trim().to_string();
    if let Some(ref_target) = line.strip_prefix("ref: ") {
        let ref_path = workspace_root.join(".git").join(ref_target);
        if let Ok(sha) = std::fs::read_to_string(&ref_path) {
            return Some(sha.trim().to_string());
        }
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn build_indexes_files_and_resolves_imports() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "use crate::util::helper;\nfn main() {}\n",
        )
        .unwrap();
        fs::write(root.join("src/util.rs"), "pub fn helper() {}").unwrap();

        let (state, outcome) = IndexState::build_from_root(root);
        assert_eq!(outcome.files_indexed, 2);
        assert_eq!(state.files.len(), 2);
        let main_id = state.path_to_id["src/main.rs"];
        let util_id = state.path_to_id["src/util.rs"];
        // Rust uses `noop` resolution, so dep graph is empty.
        assert_eq!(state.deps.imports_of(main_id), Vec::<FileId>::new());
        let _ = util_id;
    }

    #[test]
    fn typescript_imports_get_resolved() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/index.ts"),
            "import { helper } from \"./util\";\n",
        )
        .unwrap();
        fs::write(root.join("src/util.ts"), "export function helper() {}").unwrap();

        let (state, _) = IndexState::build_from_root(root);
        let index_id = state.path_to_id["src/index.ts"];
        let util_id = state.path_to_id["src/util.ts"];
        assert_eq!(state.deps.imports_of(index_id), vec![util_id]);
        assert_eq!(state.deps.importers_of(util_id), vec![index_id]);
    }

    #[test]
    fn lookup_path_handles_absolute_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/c.py"), "x = 1\n").unwrap();
        let (state, _) = IndexState::build_from_root(root);
        let abs = root.join("a/b/c.py");
        let id = state.lookup_path(abs.to_str().unwrap()).unwrap();
        assert_eq!(state.path_to_id["a/b/c.py"], id);
    }

    #[test]
    fn reindex_file_picks_up_changes_in_place() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.ts"), "export const x = 1;\n").unwrap();
        let (mut state, _) = IndexState::build_from_root(root);
        let id = state.path_to_id["src/a.ts"];
        let before_hash = state.files[&id].content_hash;

        fs::write(root.join("src/a.ts"), "export const x = 2;\n").unwrap();
        let new_id = state.reindex_file(&root.join("src/a.ts")).unwrap();
        assert_eq!(new_id, id, "file id should be stable across reindex");
        let after_hash = state.files[&id].content_hash;
        assert_ne!(before_hash, after_hash);
    }

    #[test]
    fn reindex_file_removes_entry_when_path_disappears() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.ts"), "export const x = 1;\n").unwrap();
        let (mut state, _) = IndexState::build_from_root(root);
        assert!(state.path_to_id.contains_key("src/a.ts"));

        fs::remove_file(root.join("src/a.ts")).unwrap();
        let result = state.reindex_file(&root.join("src/a.ts"));
        assert!(result.is_none());
        assert!(!state.path_to_id.contains_key("src/a.ts"));
    }
}
