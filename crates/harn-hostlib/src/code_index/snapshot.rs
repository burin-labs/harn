//! Persistent on-disk snapshot of the workspace index.
//!
//! Mirrors the Swift `CodeIndexSnapshot` struct. v1 uses a single JSON
//! file at `.burin/index/snapshot.json`. The shape is intentionally
//! tolerant of missing sections so we can extend it in place without a
//! version bump (e.g. add a new sub-index without invalidating earlier
//! snapshots).
//!
//! The snapshot is the recovery primitive. On daemon startup, the
//! embedder restores from the snapshot if one exists, then calls
//! [`super::IndexState::reap_after_recovery`] to drop stale agent
//! records and locks before serving any traffic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::agents::{AgentRegistry, RegistryConfig, SerializedRegistry};
use super::file_table::{FileId, IndexedFile, IndexedSymbol};
use super::graph::DepGraph;
use super::trigram::TrigramIndex;
use super::versions::VersionLog;
use super::words::WordIndex;
use super::IndexState;

/// Current format version. Bumped whenever the snapshot layout changes
/// in a non-additive way.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// On-disk metadata header. Small and cheap to read so embedders can
/// peek at a snapshot without parsing the whole thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// Format version. Must equal [`SNAPSHOT_FORMAT_VERSION`] for now;
    /// older snapshots are dropped.
    pub format_version: u32,
    /// Workspace root the snapshot was captured against.
    pub workspace_root: String,
    /// `HEAD` SHA of the workspace at snapshot time, when known.
    pub git_head: Option<String>,
    /// Wall-clock ms since the Unix epoch when the snapshot was written.
    pub indexed_at_ms: i64,
    /// Total number of files captured.
    pub file_count: usize,
}

/// Serialised form of one outline symbol. Mirrors [`IndexedSymbol`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSymbol {
    /// Symbol name.
    pub name: String,
    /// Language-specific kind tag.
    pub kind: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based inclusive end line.
    pub end_line: u32,
    /// Single-line preview of the declaration.
    pub signature: String,
}

/// Serialised form of one file row. Mirrors [`IndexedFile`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// Stable file identifier.
    pub id: FileId,
    /// Workspace-relative path with `/` separators.
    pub relative_path: String,
    /// Best-effort language tag.
    pub language: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Newline-delimited line count.
    pub line_count: u32,
    /// FNV-1a 64-bit content hash.
    pub content_hash: u64,
    /// Last-modified time (ms since epoch).
    pub mtime_ms: i64,
    /// Outline symbols.
    pub symbols: Vec<SnapshotSymbol>,
    /// Raw import statement strings.
    pub imports: Vec<String>,
}

/// One trigram posting entry: `trigram → list of file ids`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrigramPosting {
    /// Packed trigram key.
    pub trigram: u32,
    /// Files containing this trigram.
    pub files: Vec<FileId>,
}

/// One word posting entry: `word → list of (file, line) pairs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordPosting {
    /// Identifier-shaped token.
    pub word: String,
    /// All occurrences as `(file_id, line)` pairs.
    pub hits: Vec<(FileId, u32)>,
}

/// One dep-graph row: `file → resolved imports + unresolved raw strings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepRow {
    /// Source file id.
    pub from: FileId,
    /// Resolved target file ids.
    pub to: Vec<FileId>,
    /// Raw import strings the resolver couldn't map back to a file.
    #[serde(default)]
    pub unresolved: Vec<String>,
}

/// Persistent on-disk form of the entire workspace index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeIndexSnapshot {
    /// Snapshot header.
    pub meta: SnapshotMeta,
    /// Next file id to hand out — preserved so reused ids don't collide
    /// with historical version-log entries.
    pub next_file_id: FileId,
    /// File table.
    pub files: Vec<SnapshotFile>,
    /// Trigram postings.
    pub trigrams: Vec<TrigramPosting>,
    /// Word postings.
    pub words: Vec<WordPosting>,
    /// Dep graph rows.
    pub deps: Vec<DepRow>,
    /// Append-only version log.
    pub versions: VersionLog,
    /// Live agents at snapshot time.
    pub agents: SerializedRegistry,
}

impl CodeIndexSnapshot {
    /// Path the snapshot lives at, relative to the workspace root.
    pub fn path_for(workspace_root: &Path) -> PathBuf {
        workspace_root
            .join(".burin")
            .join("index")
            .join("snapshot.json")
    }

    /// Save the snapshot atomically (`tmp` file + rename) so partial
    /// writes never leave a half-encoded JSON blob on disk.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        let path = Self::path_for(workspace_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Try to load the snapshot from `workspace_root/.burin/index/snapshot.json`.
    /// Returns `Ok(None)` when no snapshot exists yet; returns `Err` when
    /// one exists but couldn't be parsed (caller is expected to fall back
    /// to `build_from_root`).
    pub fn load(workspace_root: &Path) -> std::io::Result<Option<Self>> {
        let path = Self::path_for(workspace_root);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        let snap: CodeIndexSnapshot =
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if snap.meta.format_version != SNAPSHOT_FORMAT_VERSION {
            return Ok(None);
        }
        Ok(Some(snap))
    }
}

impl IndexState {
    /// Capture the current state as a [`CodeIndexSnapshot`].
    pub fn snapshot(&self) -> CodeIndexSnapshot {
        let files: Vec<SnapshotFile> = self
            .files
            .values()
            .map(|f| SnapshotFile {
                id: f.id,
                relative_path: f.relative_path.clone(),
                language: f.language.clone(),
                size_bytes: f.size_bytes,
                line_count: f.line_count,
                content_hash: f.content_hash,
                mtime_ms: f.mtime_ms,
                symbols: f
                    .symbols
                    .iter()
                    .map(|s| SnapshotSymbol {
                        name: s.name.clone(),
                        kind: s.kind.clone(),
                        start_line: s.start_line,
                        end_line: s.end_line,
                        signature: s.signature.clone(),
                    })
                    .collect(),
                imports: f.imports.clone(),
            })
            .collect();

        let trigrams = self.trigrams.snapshot_postings();
        let words = self.words.snapshot_postings();
        let deps = self.deps.snapshot_rows();

        CodeIndexSnapshot {
            meta: SnapshotMeta {
                format_version: SNAPSHOT_FORMAT_VERSION,
                workspace_root: self.root.to_string_lossy().into_owned(),
                git_head: self.git_head.clone(),
                indexed_at_ms: self.last_built_unix_ms,
                file_count: self.files.len(),
            },
            next_file_id: self.next_file_id_internal(),
            files,
            trigrams,
            words,
            deps,
            versions: self.versions.clone(),
            agents: self.agents.snapshot(),
        }
    }

    /// Restore an [`IndexState`] from a snapshot. The workspace root is
    /// taken from the snapshot meta; callers can then call
    /// [`Self::reap_after_recovery`] to drop stale agent records.
    pub fn from_snapshot(snap: CodeIndexSnapshot) -> Self {
        let root = PathBuf::from(snap.meta.workspace_root);
        let mut files: HashMap<FileId, IndexedFile> = HashMap::with_capacity(snap.files.len());
        let mut path_to_id: HashMap<String, FileId> = HashMap::with_capacity(snap.files.len());
        for f in snap.files {
            let indexed = IndexedFile {
                id: f.id,
                relative_path: f.relative_path.clone(),
                language: f.language,
                size_bytes: f.size_bytes,
                line_count: f.line_count,
                content_hash: f.content_hash,
                mtime_ms: f.mtime_ms,
                symbols: f
                    .symbols
                    .into_iter()
                    .map(|s| IndexedSymbol {
                        name: s.name,
                        kind: s.kind,
                        start_line: s.start_line,
                        end_line: s.end_line,
                        signature: s.signature,
                    })
                    .collect(),
                imports: f.imports,
            };
            path_to_id.insert(f.relative_path, f.id);
            files.insert(f.id, indexed);
        }
        let trigrams = TrigramIndex::from_postings(snap.trigrams);
        let words = WordIndex::from_postings(snap.words);
        let deps = DepGraph::from_rows(snap.deps);
        let agents = AgentRegistry::from_snapshot(RegistryConfig::default(), snap.agents);

        let mut state = Self::empty(root);
        state.files = files;
        state.path_to_id = path_to_id;
        state.trigrams = trigrams;
        state.words = words;
        state.deps = deps;
        state.versions = snap.versions;
        state.agents = agents;
        state.last_built_unix_ms = snap.meta.indexed_at_ms;
        state.git_head = snap.meta.git_head;
        state.set_next_file_id(snap.next_file_id);
        state
    }

    /// Drop stale agent records and release any locks held by agents
    /// whose `last_seen_ms` is older than the configured timeout. Called
    /// at startup after restoring from a snapshot.
    pub fn reap_after_recovery(&mut self, now_ms: i64) {
        self.agents.reap(now_ms);
    }
}
