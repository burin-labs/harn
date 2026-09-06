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
//!
//! [`IndexState::refresh_from_root`] is the incremental counterpart of
//! `build_from_root`: it re-walks the workspace but re-reads and
//! re-parses only the files whose mtime or size moved, so an unchanged
//! tree costs one stat per file instead of a full read-and-parse pass.
//! Every index read goes through it, which is why the cost of holding a
//! warm index no longer scales with the size of the workspace.

use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::agents::AgentRegistry;
use super::file_table::{fnv1a64, FileId, IndexedFile, IndexedSymbol};
use super::graph::DepGraph;
use super::imports;
use super::imports_go::GoModules;
use super::overlay::OverlayState;
use super::symbol_graph::SymbolGraph;
use super::trigram::TrigramIndex;
use super::versions::VersionLog;
use super::walker::{is_indexable_file, language_for_extension, walk_indexable, MAX_FILE_BYTES};
use super::words::WordIndex;
use crate::{HarnReferenceInput, HarnReferenceResolver};

use crate::ast::{Language as AstLanguage, Symbol as AstSymbol};

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
    /// Forward + reverse import graph. Explicit import statements only;
    /// ask [`IndexState::imports_of`] for the full dependency answer,
    /// which folds in implicit same-module visibility.
    pub deps: DepGraph,
    /// Module membership for languages where a file's module is implied
    /// by its location. Rebuilt whenever the path set changes.
    pub(crate) modules: imports::ModuleIndex,
    /// Append-only log of file mutations.
    pub versions: VersionLog,
    /// Live agents + advisory locks.
    pub agents: AgentRegistry,
    /// Typed symbol graph (issue #2434). Populated lazily on rebuild.
    pub symbols: SymbolGraph,
    /// Per-branch overlay registry (issue #2434).
    pub overlays: OverlayState,
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

/// Summary returned from [`IndexState::refresh_from_root`].
///
/// `files_reindexed` is the falsifiable number: on an unchanged tree it
/// must be zero, and after one edit it must be one. A refresh that
/// reports work it did not do, or hides work it did, is the failure this
/// struct exists to make visible.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// Files the walk visited (i.e. that passed the indexer's filters).
    pub files_scanned: u64,
    /// Files whose mtime and size were unchanged, so nothing was read.
    pub files_unchanged: u64,
    /// Files whose contents were re-read, re-hashed, and re-parsed.
    pub files_reindexed: u64,
    /// Files present in the walk but absent from the previous index.
    pub files_added: u64,
    /// Files dropped from the index because they no longer exist or no
    /// longer pass the filters.
    pub files_removed: u64,
    /// Files that were re-read but turned out to have identical content
    /// (mtime moved, bytes did not), so no re-parse was needed.
    pub files_touched_only: u64,
    /// Files the walk offered but `ingest` refused (unreadable, oversize).
    pub files_skipped: u64,
}

impl RefreshOutcome {
    /// True when the refresh changed nothing about the index contents.
    pub fn is_noop(&self) -> bool {
        self.files_reindexed == 0 && self.files_added == 0 && self.files_removed == 0
    }
}

/// One language's row in [`IndexState::import_census`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCensusRow {
    /// Language tag as the file table records it.
    pub language: String,
    /// The resolution strategy this language declares.
    pub strategy: imports::ResolutionStrategy,
    /// Indexed files of this language.
    pub files: u64,
    /// Files from which at least one import string was extracted.
    pub files_with_imports: u64,
    /// Files whose own import statements resolved to at least one
    /// workspace file. Deliberately separate from
    /// [`Self::files_with_module_peers`]: a language with implicit
    /// module membership would otherwise report every file as resolved
    /// while its import-path resolver did nothing at all, which is
    /// exactly the failure this census exists to catch.
    pub files_with_resolved_imports: u64,
    /// Files that see at least one other file through module membership
    /// rather than through an import statement.
    pub files_with_module_peers: u64,
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
            modules: imports::ModuleIndex::default(),
            versions: VersionLog::new(),
            agents: AgentRegistry::new(),
            symbols: SymbolGraph::new(),
            overlays: OverlayState::new(),
            last_built_unix_ms: now_unix_ms(),
            git_head: super::git_head::read_git_head(&canonical_root),
            next_id: 1,
        };
        let mut outcome = BuildOutcome::default();
        let mut to_resolve: Vec<(FileId, String)> = Vec::new();
        walk_indexable(&canonical_root, |abs, meta| {
            match state.ingest(abs, Some(meta)) {
                Some((file_id, _)) => {
                    outcome.files_indexed += 1;
                    if let Some(file) = state.files.get(&file_id) {
                        to_resolve.push((file_id, file.relative_path.clone()));
                    }
                }
                None => {
                    outcome.files_skipped += 1;
                }
            }
        });
        state.refresh_module_index();
        for (id, rel) in to_resolve {
            state.rebuild_deps(id, &rel);
            state.rebuild_symbol_graph_for(id);
        }
        // Second pass: every Module node exists now, so resolve IMPORTS.
        state.link_symbol_imports();
        (state, outcome)
    }

    /// Bring an already-built index up to date with the workspace on
    /// disk, re-reading only the files that actually moved.
    ///
    /// The walk is the same one `build_from_root` uses, and it already
    /// has to `stat` every candidate to apply the size and symlink
    /// filters. That metadata is the freshness oracle: a tracked file
    /// whose mtime **and** size both match the indexed row is skipped
    /// without opening it. Everything else is re-read, and only the
    /// files whose content hash actually changed pay the tree-sitter
    /// re-parse. Paths the walk no longer yields are dropped.
    ///
    /// Two whole-index passes survive, both of which are pure in-memory
    /// map work with no disk or parser cost: import resolution is redone
    /// for every file when the set of paths changed (a new file can
    /// resolve an import that was dangling), and `link_symbol_imports`
    /// re-links Module→Module edges. The Harn reference resolver runs
    /// only when a `.harn` file moved, because it re-reads the whole
    /// Harn source set.
    ///
    /// Returns a [`RefreshOutcome`] whose `files_reindexed` is the
    /// honest count of files re-read from disk — zero on an unchanged
    /// tree.
    pub fn refresh_from_root(
        &mut self,
        resolver: Option<&HarnReferenceResolver>,
    ) -> RefreshOutcome {
        let root = self.root.clone();
        let mut outcome = RefreshOutcome::default();
        let mut seen: HashSet<String> = HashSet::with_capacity(self.files.len());
        let mut reparse: Vec<(FileId, String)> = Vec::new();
        let mut harn_touched = false;
        let mut path_set_changed = false;

        walk_indexable(&root, |abs, meta| {
            outcome.files_scanned += 1;
            let Some(rel) = relative_path_under_root(&root, abs) else {
                return;
            };
            let known = self.path_to_id.get(&rel).copied();
            if let Some(id) = known {
                if let Some(file) = self.files.get(&id) {
                    if file.size_bytes == meta.len() && file.mtime_ms == mtime_ms_of(meta) {
                        outcome.files_unchanged += 1;
                        seen.insert(rel);
                        return;
                    }
                }
            }
            match self.ingest(abs, Some(meta)) {
                Some((id, changed)) => {
                    seen.insert(rel.clone());
                    if known.is_none() {
                        outcome.files_added += 1;
                        path_set_changed = true;
                    }
                    if changed {
                        outcome.files_reindexed += 1;
                        if self.files.get(&id).is_some_and(|f| f.language == "harn") {
                            harn_touched = true;
                        }
                        reparse.push((id, rel));
                    } else {
                        outcome.files_touched_only += 1;
                    }
                }
                None => {
                    outcome.files_skipped += 1;
                }
            }
        });

        // Anything tracked that the walk did not yield is gone (deleted,
        // moved, or newly filtered out).
        let stale: Vec<String> = self
            .path_to_id
            .keys()
            .filter(|rel| !seen.contains(*rel))
            .cloned()
            .collect();
        for rel in stale {
            if self
                .path_to_id
                .get(&rel)
                .and_then(|id| self.files.get(id))
                .is_some_and(|f| f.language == "harn")
            {
                harn_touched = true;
            }
            self.remove_relative_path(&rel);
            outcome.files_removed += 1;
            path_set_changed = true;
        }

        if outcome.is_noop() {
            return outcome;
        }

        if path_set_changed {
            // A path appearing or disappearing can resolve or dangle an
            // import in a file that did not itself change, so redo the
            // whole resolution table. Pure map lookups, no disk reads.
            self.refresh_module_index();
            let all: Vec<(FileId, String)> = self
                .files
                .values()
                .map(|f| (f.id, f.relative_path.clone()))
                .collect();
            for (id, rel) in all {
                self.rebuild_deps(id, &rel);
            }
        } else {
            for (id, rel) in &reparse {
                self.rebuild_deps(*id, rel);
            }
        }
        for (id, _) in &reparse {
            self.rebuild_symbol_graph_for(*id);
        }
        self.link_symbol_imports();
        if harn_touched {
            self.relink_harn_references(resolver);
        }
        self.last_built_unix_ms = now_unix_ms();
        outcome
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
        let (id, _changed) = self.ingest(abs, None)?;
        let rel = self
            .files
            .get(&id)
            .map(|f| f.relative_path.clone())
            .unwrap_or_default();
        if !rel.is_empty() {
            self.refresh_module_index();
            self.rebuild_deps(id, &rel);
            self.rebuild_symbol_graph_for(id);
            self.link_symbol_imports();
        }
        Some(id)
    }

    /// Remove an existing file from every sub-index. No-op when the file
    /// isn't tracked.
    pub fn remove_file_path(&mut self, abs: &Path) {
        let Some(rel) = relative_path(&self.root, abs) else {
            return;
        };
        self.remove_relative_path(&rel);
    }

    /// Remove a workspace-relative path from every sub-index. The
    /// path-keyed entry point, used by the incremental refresh where the
    /// file is already known to be gone and re-canonicalising it would
    /// be a wasted syscall. No-op when the path isn't tracked.
    pub(super) fn remove_relative_path(&mut self, rel: &str) {
        let Some(id) = self.path_to_id.remove(rel) else {
            return;
        };
        self.files.remove(&id);
        self.trigrams.remove_file(id);
        self.words.remove_file(id);
        self.deps.remove_file(id);
        self.symbols.remove_file(id);
        // Module membership is derived from the path table, so a
        // removal has to be reflected here or every sibling keeps
        // answering with a file that is gone. Guarded on the file
        // actually belonging to a module: the rebuild is O(paths), and
        // the common removal is a file no module ever contained.
        if self.modules.home_of(id).is_some() {
            self.refresh_module_index();
        }
    }

    /// Read `abs` and install (or update) its row in every flat
    /// sub-index. Returns the file id and whether the **contents**
    /// changed; a file whose bytes hash the same as the indexed row
    /// reports `false` so the caller can skip the tree-sitter re-parse,
    /// and its recorded mtime is refreshed so the cheap stat gate stops
    /// re-reading it on every later refresh.
    ///
    /// `known_metadata` lets the directory walk hand over the `stat` it
    /// already performed instead of paying a second one per file.
    fn ingest(&mut self, abs: &Path, known_metadata: Option<&Metadata>) -> Option<(FileId, bool)> {
        if !is_indexable_file(abs) {
            return None;
        }
        let owned;
        let metadata = match known_metadata {
            Some(meta) => meta,
            None => {
                owned = std::fs::metadata(abs).ok()?;
                &owned
            }
        };
        if metadata.len() > MAX_FILE_BYTES {
            return None;
        }
        let content = std::fs::read_to_string(abs).ok()?;
        if content.len() > MAX_FILE_BYTES as usize {
            return None;
        }
        let rel = relative_path(&self.root, abs)?;
        let hash = fnv1a64(content.as_bytes());
        let mtime_ms = mtime_ms_of(metadata);
        let id = match self.path_to_id.get(&rel) {
            Some(existing_id) => {
                let existing_id = *existing_id;
                if let Some(file) = self.files.get_mut(&existing_id) {
                    if file.content_hash == hash {
                        // Same bytes under a newer timestamp. Record the
                        // timestamp so the stat gate recognises it next
                        // time, and tell the caller nothing changed.
                        file.mtime_ms = mtime_ms;
                        file.size_bytes = metadata.len();
                        return Some((existing_id, false));
                    }
                }
                existing_id
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
        let line_count = crate::text::count_lines(content.as_bytes()) as u32;

        let file = IndexedFile {
            id,
            relative_path: rel,
            language,
            size_bytes: metadata.len(),
            line_count,
            content_hash: hash,
            mtime_ms,
            symbols: Vec::new(),
            imports,
        };
        self.trigrams.index_file(id, &content);
        self.words.index_file(id, &content);
        self.files.insert(id, file);
        Some((id, true))
    }

    /// Recompute module membership from the current path table. O(paths),
    /// so it runs once per path-set change rather than once per file.
    fn refresh_module_index(&mut self) {
        // `go.mod` is the one input that is not derivable from the path
        // table: an import path only maps onto a directory through the
        // module path a manifest declares. It is also not in the path
        // table, because the index does not track manifests, so the
        // ancestors of every Go file are probed on disk instead. That is
        // a handful of reads, once per path-set change.
        let mut manifest_dirs: HashSet<String> = HashSet::new();
        for rel in self.path_to_id.keys() {
            if !rel.ends_with(".go") {
                continue;
            }
            let mut cursor = rel.as_str();
            while let Some((head, _)) = cursor.rsplit_once('/') {
                manifest_dirs.insert(head.to_string());
                cursor = head;
            }
            manifest_dirs.insert(String::new());
        }
        let mut manifests: Vec<(String, String)> = manifest_dirs
            .into_iter()
            .filter_map(|dir| {
                let rel = if dir.is_empty() {
                    "go.mod".to_string()
                } else {
                    format!("{dir}/go.mod")
                };
                std::fs::read_to_string(self.root.join(&rel))
                    .ok()
                    .map(|contents| (rel, contents))
            })
            .collect();
        manifests.sort();
        let go_modules = GoModules::from_manifests(
            manifests
                .iter()
                .map(|(path, contents)| (path.as_str(), contents.as_str())),
        );
        self.modules = imports::ModuleIndex::build(&self.path_to_id).with_go_modules(go_modules);
    }

    /// [`Self::refresh_module_index`] for the snapshot-restore path,
    /// which fills the path table in from disk rather than by walking.
    pub(crate) fn rebuild_module_index(&mut self) {
        self.refresh_module_index();
    }

    /// Every file `id` depends on: the targets of its own import
    /// statements, plus every other file in its module for languages
    /// where same-module visibility needs no import statement.
    ///
    /// The second half is not stored as edges. A module of N files would
    /// need N squared of them, which is the shape that made the symbol
    /// graph unusable in issue #8081. Membership is O(N) in
    /// [`Self::modules`] and expanded per query instead.
    pub fn imports_of(&self, id: FileId) -> Vec<FileId> {
        let mut out = self.deps.imports_of(id);
        out.extend(self.modules.files_in_roots(self.deps.module_imports_of(id)));
        out.extend(self.modules.siblings_of(id));
        out.sort_unstable();
        out.dedup();
        out.retain(|other| *other != id);
        out
    }

    /// Reverse of [`Self::imports_of`]. Same-module visibility is
    /// symmetric, so the sibling half is identical in both directions.
    pub fn importers_of(&self, id: FileId) -> Vec<FileId> {
        let mut out = self.deps.importers_of(id);
        if let Some(home) = self.modules.home_of(id) {
            out.extend(self.deps.module_importers_of(home));
        }
        out.extend(self.modules.siblings_of(id));
        out.sort_unstable();
        out.dedup();
        out.retain(|other| *other != id);
        out
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
            &self.modules,
        );
        self.deps.set_module_imports(id, resolved.modules);
        self.deps
            .set_edges(id, resolved.resolved, resolved.unresolved);
    }

    /// Re-parse `id`'s source and replace its slice of the typed symbol
    /// graph in [`Self::symbols`]. Cheap to call after a single-file
    /// reindex; the full-rebuild loop calls this once per file. Files
    /// with no recognised tree-sitter grammar (the index also handles
    /// `.md`, `.json`, …) are skipped silently — `IndexedFile::symbols`
    /// stays empty for those files. For grammar-recognised files the
    /// same parse populates `IndexedFile::symbols` so the
    /// `outline_get` builtin doesn't have to re-parse on every call
    /// (issue #2456).
    pub(super) fn rebuild_symbol_graph_for(&mut self, id: FileId) {
        let Some(file) = self.files.get(&id).cloned() else {
            return;
        };
        let abs = self.root.join(&file.relative_path);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            return;
        };
        let Some(language) = AstLanguage::detect(std::path::Path::new(&file.relative_path), None)
        else {
            return;
        };
        // Call resolution is scoped by what this file can actually see,
        // so the dep graph for `id` must already be current. Every
        // caller runs `rebuild_deps` for the same id immediately before
        // this, and `refresh_from_root` re-resolves the whole table
        // whenever the set of paths changed, which is when a dangling
        // import can start resolving.
        //
        // The full answer, not just the explicit import statements: a
        // Swift or Go file calls into its own module's other files
        // without importing them, and scoping the resolver to written
        // imports alone would put those calls out of reach.
        let imported_files = self.imports_of(id);
        let outcome = self.symbols.rebuild_file(
            id,
            &file.relative_path,
            language,
            &source,
            &file.imports,
            &imported_files,
        );
        if let Some(file_mut) = self.files.get_mut(&id) {
            file_mut.symbols = outcome
                .symbols
                .iter()
                .map(indexed_symbol_from_ast)
                .collect();
        }
    }

    /// Walk every file's import-resolution table and add the
    /// corresponding Module→Module IMPORTS edges in the typed graph.
    /// Idempotent; called once at end-of-rebuild and after every
    /// per-file reindex.
    pub(super) fn link_symbol_imports(&mut self) {
        let mut resolved: HashMap<FileId, Vec<FileId>> = HashMap::new();
        for id in self.files.keys() {
            // Explicit import statements only. Module-wide visibility
            // would put one Module→Module edge in the symbol graph per
            // pair of files in a target, which is the cross-product
            // issue #8081 removed. The call resolver consults module
            // membership directly instead.
            resolved.insert(*id, self.deps.imports_of(*id));
        }
        self.symbols.link_imports(&resolved);
    }

    /// Replace Harn reference edges from the integration-owned resolver.
    pub(super) fn relink_harn_references(&mut self, resolver: Option<&HarnReferenceResolver>) {
        let Some(resolver) = resolver else {
            return;
        };
        let mut files: Vec<PathBuf> = self
            .files
            .values()
            .filter(|file| file.language == "harn")
            .map(|file| self.root.join(&file.relative_path))
            .collect();
        files.sort();
        let input = HarnReferenceInput {
            root: self.root.clone(),
            files,
            source_overrides: HashMap::new(),
        };
        match resolver(&input) {
            Ok(references) => self.symbols.replace_harn_references(&references),
            Err(error) => {
                self.symbols.replace_harn_references(&[]);
                tracing::warn!(%error, "Harn reference relink failed; cleared stale REFS edges");
            }
        }
    }

    /// Per-language import-resolution census.
    ///
    /// This exists because the aggregate number is the only one that
    /// catches the failure it was written for. Every per-language unit
    /// test passed while Rust, Swift, Go and Harn resolved nothing at
    /// all across a real workspace, because each language's resolver was
    /// declared `noop` and a zero from "no resolver" looked exactly like
    /// a zero from "nothing to resolve".
    ///
    /// [`ImportCensusRow::strategy`] is therefore part of the answer, not
    /// decoration: a row reading `unresolved-by-design` is reporting that
    /// nothing was attempted, which is a different claim from a row that
    /// tried and found no target.
    pub fn import_census(&self) -> Vec<ImportCensusRow> {
        let mut by_language: HashMap<&str, ImportCensusRow> = HashMap::new();
        for file in self.files.values() {
            let row = by_language
                .entry(file.language.as_str())
                .or_insert_with(|| ImportCensusRow {
                    language: file.language.clone(),
                    strategy: imports::strategy_for(&file.language),
                    files: 0,
                    files_with_imports: 0,
                    files_with_resolved_imports: 0,
                    files_with_module_peers: 0,
                });
            row.files += 1;
            if !file.imports.is_empty() {
                row.files_with_imports += 1;
            }
            // Explicit import statements only. Folding membership in
            // here would let a broken import-path resolver read as a
            // working one.
            if !self.deps.imports_of(file.id).is_empty()
                || !self.deps.module_imports_of(file.id).is_empty()
            {
                row.files_with_resolved_imports += 1;
            }
            if !self.modules.siblings_of(file.id).is_empty() {
                row.files_with_module_peers += 1;
            }
        }
        let mut rows: Vec<ImportCensusRow> = by_language.into_values().collect();
        rows.sort_by(|a, b| {
            b.files
                .cmp(&a.files)
                .then_with(|| a.language.cmp(&b.language))
        });
        rows
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
    pub fn absolute_path(&self, rel_or_abs: &str) -> Option<PathBuf> {
        let p = Path::new(rel_or_abs);
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let canonical = canonicalize_existing(&candidate);
        if canonical.strip_prefix(&self.root).is_ok() {
            Some(canonical)
        } else {
            None
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
            modules: imports::ModuleIndex::default(),
            versions: VersionLog::new(),
            agents: AgentRegistry::new(),
            symbols: SymbolGraph::new(),
            overlays: OverlayState::new(),
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

/// Map an AST-level [`AstSymbol`] (0-based tree-sitter coordinates) into
/// the flat [`IndexedSymbol`] (1-based outline coordinates) that the
/// `outline_get` builtin returns. Pure, used by
/// [`IndexState::rebuild_symbol_graph_for`].
fn indexed_symbol_from_ast(sym: &AstSymbol) -> IndexedSymbol {
    IndexedSymbol {
        name: sym.name.clone(),
        kind: sym.kind.as_str().to_string(),
        access_level: sym.access_level.clone(),
        start_line: sym.start_row.saturating_add(1),
        end_line: sym.end_row.saturating_add(1),
        signature: sym.signature.clone(),
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

/// Last-modified time of `metadata` in milliseconds since the Unix
/// epoch, or `0` when the platform does not report one. Paired with the
/// file size, this is the freshness oracle the incremental refresh uses
/// to decide it can skip opening a file.
pub(crate) fn mtime_ms_of(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Cheap relative path for a walk result. The walker descends from the
/// already-canonical root and never follows a symlink, so every path it
/// yields is canonical-prefixed and a plain `strip_prefix` is correct.
/// [`relative_path`] stays the entry point for caller-supplied paths,
/// which may be symlinked, relative, or already deleted.
fn relative_path_under_root(root: &Path, abs: &Path) -> Option<String> {
    let stripped = abs.strip_prefix(root).ok()?;
    Some(crate::tools::args::to_agent_path(stripped))
}

pub(super) fn canonicalize(root: &Path) -> PathBuf {
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
    Some(crate::tools::args::to_agent_path(stripped))
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
        // This assertion used to read "Rust uses `noop` resolution, so
        // dep graph is empty", which encoded the defect rather than a
        // requirement: `use crate::util::helper` names `src/util.rs` and
        // always did. Rust module resolution now anchors on the crate
        // root and says so.
        assert_eq!(state.deps.imports_of(main_id), vec![util_id]);
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

    #[test]
    fn swift_target_membership_is_stored_per_module_not_per_pair() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Sources/Core")).unwrap();
        fs::create_dir_all(dir.path().join("Sources/App")).unwrap();
        for name in ["A", "B", "C"] {
            fs::write(
                dir.path().join(format!("Sources/Core/{name}.swift")),
                format!("struct {name} {{}}\n"),
            )
            .unwrap();
        }
        fs::write(
            dir.path().join("Sources/App/Main.swift"),
            "import Core\nlet a = A()\n",
        )
        .unwrap();

        let (state, _) = IndexState::build_from_root(dir.path());
        let main_id = state.path_to_id["Sources/App/Main.swift"];
        let a_id = state.path_to_id["Sources/Core/A.swift"];
        let b_id = state.path_to_id["Sources/Core/B.swift"];
        let c_id = state.path_to_id["Sources/Core/C.swift"];

        // The answer names every file in the imported target.
        assert_eq!(state.imports_of(main_id), vec![a_id, b_id, c_id]);

        // Same-target files see each other with no import statement at
        // all, and never themselves.
        assert_eq!(state.imports_of(a_id), vec![b_id, c_id]);

        // The falsifier for the storage claim: none of that is written
        // out as one dependency edge per file. `Main.swift` holds a
        // single module row, and `A.swift` holds none.
        assert!(state.deps.imports_of(main_id).is_empty());
        assert_eq!(state.deps.module_imports_of(main_id), ["Sources/Core"]);
        assert!(state.deps.module_imports_of(a_id).is_empty());

        // The reverse direction is answered from the same rows.
        assert!(state.importers_of(a_id).contains(&main_id));
    }

    #[test]
    fn deleting_a_swift_file_shrinks_its_targets_membership() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Sources/Core")).unwrap();
        fs::create_dir_all(dir.path().join("Sources/App")).unwrap();
        fs::write(dir.path().join("Sources/Core/A.swift"), "struct A {}\n").unwrap();
        fs::write(dir.path().join("Sources/Core/B.swift"), "struct B {}\n").unwrap();
        fs::write(
            dir.path().join("Sources/App/Main.swift"),
            "import Core\nlet a = A()\n",
        )
        .unwrap();

        let (mut state, _) = IndexState::build_from_root(dir.path());
        let main_id = state.path_to_id["Sources/App/Main.swift"];
        assert_eq!(state.imports_of(main_id).len(), 2);

        fs::remove_file(dir.path().join("Sources/Core/B.swift")).unwrap();
        state.reindex_file(&dir.path().join("Sources/Core/B.swift"));

        // Membership is derived, so removing a file must remove it from
        // every answer without anyone re-resolving an import statement.
        assert_eq!(state.imports_of(main_id).len(), 1);
    }

    #[test]
    fn go_imports_resolve_through_the_manifests_module_path() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("internal/store")).unwrap();
        fs::create_dir_all(dir.path().join("internal/runtime")).unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/acme/tool\n\ngo 1.26.1\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("internal/store/state.go"),
            "package store\n\ntype State struct{}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("internal/store/keys.go"),
            "package store\n\nfunc Key() string { return \"\" }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("internal/runtime/net.go"),
            "package runtime\n\nimport (\n\t\"fmt\"\n\n\t\"github.com/acme/tool/internal/store\"\n)\n\nvar _ = fmt.Sprintf\n",
        )
        .unwrap();

        let (state, _) = IndexState::build_from_root(dir.path());
        let net = state.path_to_id["internal/runtime/net.go"];
        let keys = state.path_to_id["internal/store/keys.go"];
        let stateful = state.path_to_id["internal/store/state.go"];

        // The block form is what Go actually writes, and every path
        // inside it has to be extracted, not just the `import (` line.
        assert_eq!(
            state.files[&net].imports,
            vec![
                "fmt".to_string(),
                "github.com/acme/tool/internal/store".to_string()
            ]
        );

        // The import path only maps onto a directory through the module
        // path the manifest declares, so this is the falsifier for the
        // manifest being read at all.
        assert_eq!(state.deps.module_imports_of(net), ["internal/store"]);
        assert_eq!(state.imports_of(net), {
            let mut want = vec![keys, stateful];
            want.sort_unstable();
            want
        });

        // `fmt` is the standard library. A real strategy ran and matched
        // nothing, which is not the same as never having tried.
        assert!(state
            .deps
            .unresolved_imports(net)
            .contains(&"fmt".to_string()));

        // Same package, no import statement between them.
        assert_eq!(state.imports_of(keys), vec![stateful]);
    }

    #[test]
    fn go_imports_resolve_to_nothing_without_a_manifest() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("internal/store")).unwrap();
        fs::create_dir_all(dir.path().join("internal/runtime")).unwrap();
        fs::write(
            dir.path().join("internal/store/state.go"),
            "package store\n\ntype State struct{}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("internal/runtime/net.go"),
            "package runtime\n\nimport (\n\t\"github.com/acme/tool/internal/store\"\n)\n",
        )
        .unwrap();

        // The negative control for the test above. With no `go.mod`
        // there is no module path, so the same import names nothing,
        // and the census must not report it as resolved just because
        // the file has package peers elsewhere.
        let (state, _) = IndexState::build_from_root(dir.path());
        let net = state.path_to_id["internal/runtime/net.go"];
        assert!(state.deps.module_imports_of(net).is_empty());
        assert!(state.imports_of(net).is_empty());
        let go_row = state
            .import_census()
            .into_iter()
            .find(|row| row.language == "go")
            .expect("go row");
        assert_eq!(go_row.files_with_imports, 1);
        assert_eq!(go_row.files_with_resolved_imports, 0);
    }
}
