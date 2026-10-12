//! Persistent on-disk snapshot of the workspace index.
//!
//! One file at `.burin/index/snapshot.bin`: an eight-byte magic, the
//! little-endian format version, then four postcard-encoded sections, each
//! preceded by its little-endian `u64` length: file facts and bookkeeping,
//! the symbol graph, trigram postings, and word postings. The header lets a
//! reader reject another layout before decoding it, and the sections decode on
//! separate threads because they share nothing. Restart time is dominated by
//! decoding and restoring this file; the compact binary body decodes several
//! times faster than the JSON it replaced while taking about a third of the
//! space. Earlier versions, including every JSON snapshot, are rebuilt rather
//! than restored.
//!
//! The snapshot is the recovery primitive. On daemon startup, the
//! embedder restores from the snapshot if one exists, then calls
//! [`super::IndexState::reap_after_recovery`] to drop stale agent
//! records and locks before serving any traffic.

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::agents::{AgentRegistry, RegistryConfig, SerializedRegistry};
use super::file_table::{FileId, IndexedFile, IndexedSymbol};
use super::graph::DepGraph;
use super::imports_go::GoModules;
use super::symbol_graph::{GraphSnapshot, SymbolGraph};
use super::trigram::TrigramIndex;
use super::versions::VersionLog;
use super::words::WordIndex;
use super::IndexState;

/// Current format version. Bumped whenever the snapshot layout changes
/// in a non-additive way, or when graph edges a snapshot stores would be
/// computed differently, since a restore keeps the stored edges for every
/// file that has not changed. v4 adds partial HEAD-verification progress;
/// older readers must not treat a checkpoint as a fully verified HEAD when
/// the checkout reverts to the previously recorded commit. v5 replaces JSON
/// with the binary layout described in the module documentation.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 5;

/// Leading bytes of every binary snapshot.
const SNAPSHOT_MAGIC: &[u8; 8] = b"HARNIDX\0";
/// Magic plus the little-endian `u32` format version.
const SNAPSHOT_HEADER_LEN: usize = SNAPSHOT_MAGIC.len() + 4;
/// Each section's length prefix.
const SECTION_LEN_BYTES: usize = 8;
/// The pre-v5 JSON file. Removed when a binary snapshot replaces it.
const LEGACY_JSON_SNAPSHOT: &str = "snapshot.json";

/// On-disk metadata header. Small and cheap to read so embedders can
/// peek at a snapshot without parsing the whole thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    /// Format version. Written to the file header, which is what a load
    /// checks; must equal [`SNAPSHOT_FORMAT_VERSION`] to restore.
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
    /// Normalized declaration access level when known.
    #[serde(default)]
    pub access_level: Option<String>,
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
    /// The typed graph. Missing in v1, which must be rebuilt before graph
    /// queries can be served.
    #[serde(default)]
    pub(super) symbols: Option<GraphSnapshot>,
    /// Completed content checks at a newer HEAD, before the full walk finished.
    #[serde(default)]
    pub(super) reconcile_progress: Option<super::reconcile::ReconcileProgress>,
    /// The module manifests the saved graph was resolved under. A snapshot
    /// written before this field restores with none, and its first
    /// reconcile re-resolves the files the workspace's manifests affect.
    #[serde(default)]
    pub(super) go_modules: GoModules,
}

impl CodeIndexSnapshot {
    /// Path the snapshot lives at, relative to the workspace root.
    pub fn path_for(workspace_root: &Path) -> PathBuf {
        workspace_root
            .join(".burin")
            .join("index")
            .join("snapshot.bin")
    }

    /// Stream to a unique sibling temporary file and atomically replace the
    /// snapshot. Large symbol graphs must not be buffered as another full
    /// serialized copy in process memory, and concurrent writers must not
    /// share one fixed `.tmp` path.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        let _phase = super::timing::Phase::start("snapshot.write");
        let path = Self::path_for(workspace_root);
        let parent = path.parent().expect("snapshot path has a parent");
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(tmp.as_file_mut());
            writer.write_all(SNAPSHOT_MAGIC)?;
            writer.write_all(&self.meta.format_version.to_le_bytes())?;
            let core: CoreSectionRef<'_> = (
                &self.meta,
                self.next_file_id,
                &self.files,
                &self.deps,
                &self.versions,
                &self.agents,
                &self.reconcile_progress,
                &self.go_modules,
            );
            write_section(&mut writer, &core)?;
            write_section(&mut writer, &self.symbols)?;
            write_section(&mut writer, &self.trigrams)?;
            write_section(&mut writer, &self.words)?;
            writer.flush()?;
        }
        tmp.persist(&path).map_err(|error| error.error)?;
        // A superseded JSON snapshot is hundreds of megabytes nothing reads.
        let _ = std::fs::remove_file(parent.join(LEGACY_JSON_SNAPSHOT));
        Ok(())
    }

    /// Try to load the snapshot from [`Self::path_for`].
    /// Returns `Ok(None)` when no snapshot exists yet, the format is
    /// unrecognised, or the snapshot does not describe `workspace_root`
    /// (different checkout or missing graph). Returns `Err` when one
    /// exists but couldn't be parsed (caller is expected to fall back
    /// to `build_from_root`).
    pub fn load(workspace_root: &Path) -> std::io::Result<Option<Self>> {
        let _phase = super::timing::Phase::start("snapshot.load");
        let path = Self::path_for(workspace_root);
        if !path.exists() {
            return Ok(None);
        }
        let read_phase = super::timing::Phase::start("snapshot.read");
        let bytes = std::fs::read(&path)?;
        drop(read_phase);
        let Some(body) = bytes
            .strip_prefix(SNAPSHOT_MAGIC.as_slice())
            .filter(|_| bytes.len() >= SNAPSHOT_HEADER_LEN)
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "code-index snapshot has no binary header",
            ));
        };
        let (version, body) = body.split_at(4);
        let format_version = u32::from_le_bytes(version.try_into().expect("four bytes"));
        if format_version != SNAPSHOT_FORMAT_VERSION {
            tracing::debug!(
                target: "harn_hostlib::code_index",
                path = %path.display(),
                format_version,
                expected = SNAPSHOT_FORMAT_VERSION,
                "code-index snapshot format mismatch; ignoring",
            );
            return Ok(None);
        }
        let decode_phase = super::timing::Phase::start("snapshot.decode");
        let mut sections = Sections(body);
        let (core, graph, trigrams, words) = (
            sections.next()?,
            sections.next()?,
            sections.next()?,
            sections.next()?,
        );
        if !sections.0.is_empty() {
            return Err(invalid("code-index snapshot has trailing bytes"));
        }
        // The sections share nothing, so decoding takes the longest of them.
        let (core, symbols, trigrams, words) = harn_parser::runtime_stack::scope(|scope| {
            let graph = scope.spawn(|| decode_section("snapshot.decode_graph", graph));
            let trigrams = scope.spawn(|| decode_section("snapshot.decode_trigrams", trigrams));
            let words = scope.spawn(|| decode_section("snapshot.decode_words", words));
            let core: io::Result<CoreSection> = decode_section("snapshot.decode_core", core);
            (core, joined(graph), joined(trigrams), joined(words))
        });
        let (meta, next_file_id, files, deps, versions, agents, reconcile_progress, go_modules) =
            core?;
        let snap = CodeIndexSnapshot {
            meta,
            next_file_id,
            files,
            trigrams: trigrams?,
            words: words?,
            deps,
            versions,
            agents,
            symbols: symbols?,
            reconcile_progress,
            go_modules,
        };
        drop(decode_phase);
        drop(bytes);
        if snap.symbols.is_none() || !snapshot_matches_workspace(&snap, workspace_root) {
            return Ok(None);
        }
        Ok(Some(snap))
    }
}

/// Every field outside the graph and postings, in declaration order.
type CoreSection = (
    SnapshotMeta,
    FileId,
    Vec<SnapshotFile>,
    Vec<DepRow>,
    VersionLog,
    SerializedRegistry,
    Option<super::reconcile::ReconcileProgress>,
    GoModules,
);
type CoreSectionRef<'a> = (
    &'a SnapshotMeta,
    FileId,
    &'a Vec<SnapshotFile>,
    &'a Vec<DepRow>,
    &'a VersionLog,
    &'a SerializedRegistry,
    &'a Option<super::reconcile::ReconcileProgress>,
    &'a GoModules,
);

/// Stream one section after a placeholder length, then patch the length in.
fn write_section<T: Serialize>(
    writer: &mut BufWriter<&mut std::fs::File>,
    value: &T,
) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    writer.flush()?;
    let start = writer.get_mut().stream_position()?;
    writer.write_all(&[0; SECTION_LEN_BYTES])?;
    postcard::to_io(value, &mut *writer).map_err(io::Error::other)?;
    writer.flush()?;
    let end = writer.get_mut().stream_position()?;
    let len = end - start - SECTION_LEN_BYTES as u64;
    writer.get_mut().seek(SeekFrom::Start(start))?;
    writer.get_mut().write_all(&len.to_le_bytes())?;
    writer.get_mut().seek(SeekFrom::Start(end))?;
    Ok(())
}

/// Length-prefixed sections of a snapshot body.
struct Sections<'a>(&'a [u8]);

impl<'a> Sections<'a> {
    fn next(&mut self) -> io::Result<&'a [u8]> {
        let truncated = || invalid("code-index snapshot section is truncated");
        let (len, rest) = self
            .0
            .split_first_chunk::<SECTION_LEN_BYTES>()
            .ok_or_else(truncated)?;
        let len = usize::try_from(u64::from_le_bytes(*len)).map_err(|_| truncated())?;
        if rest.len() < len {
            return Err(truncated());
        }
        let (section, rest) = rest.split_at(len);
        self.0 = rest;
        Ok(section)
    }
}

fn decode_section<T: serde::de::DeserializeOwned>(
    phase: &'static str,
    bytes: &[u8],
) -> io::Result<T> {
    let _phase = super::timing::Phase::start(phase);
    match postcard::take_from_bytes(bytes) {
        Ok((value, [])) => Ok(value),
        Ok(_) => Err(invalid("code-index snapshot section has trailing bytes")),
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Join a scoped restore thread, re-raising its panic on the caller.
fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn snapshot_matches_workspace(snap: &CodeIndexSnapshot, workspace_root: &Path) -> bool {
    let requested = super::state::canonicalize(workspace_root);
    let stored = super::state::canonicalize(Path::new(&snap.meta.workspace_root));
    if stored != requested {
        tracing::info!(
            target: "harn_hostlib::code_index",
            requested = %requested.display(),
            stored = %stored.display(),
            "code-index snapshot workspace root mismatch; ignoring",
        );
        return false;
    }
    // A changed HEAD is a reason to verify file contents, not to discard
    // every unchanged file and rebuild the whole graph. The owning refresh
    // reconciles the snapshot before it can be served.
    true
}

impl IndexState {
    /// Capture the current state as a [`CodeIndexSnapshot`].
    pub fn snapshot(&self) -> CodeIndexSnapshot {
        let _phase = super::timing::Phase::start("snapshot.capture");
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
                        access_level: s.access_level.clone(),
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
            symbols: Some(self.symbols.snapshot()),
            reconcile_progress: self.reconcile_progress.clone(),
            go_modules: self.modules.go_modules().clone(),
        }
    }

    /// Restore an [`IndexState`] from a snapshot. Callers that loaded the
    /// snapshot for a specific checkout should overwrite [`Self::root`]
    /// with that checkout's canonical path so a stored path string cannot
    /// redirect later persists.
    pub fn from_snapshot(snap: CodeIndexSnapshot) -> io::Result<Self> {
        let _phase = super::timing::Phase::start("snapshot.restore");
        let root = PathBuf::from(snap.meta.workspace_root);
        let symbols = snap
            .symbols
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing symbol graph"))?;
        // The graph and the two posting stores share no state, so restore
        // takes the longest of them rather than their sum.
        let (graph, trigrams, words) = harn_parser::runtime_stack::scope(|scope| {
            let trigrams = scope.spawn(|| {
                let _phase = super::timing::Phase::start("snapshot.restore_trigrams");
                TrigramIndex::from_postings(snap.trigrams)
            });
            let words = scope.spawn(|| {
                let _phase = super::timing::Phase::start("snapshot.restore_words");
                WordIndex::from_postings(snap.words)
            });
            let graph = {
                let _phase = super::timing::Phase::start("snapshot.restore_graph");
                SymbolGraph::from_snapshot(symbols)
            };
            (graph, joined(trigrams), joined(words))
        });
        let graph = graph.map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
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
                        access_level: s.access_level,
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
        let deps = DepGraph::from_rows(snap.deps);
        let agents = AgentRegistry::from_snapshot(RegistryConfig::default(), snap.agents);

        if graph.file_ids().iter().any(|id| !files.contains_key(id)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symbol graph names an absent file",
            ));
        }

        let mut state = Self::empty(root);
        state.files = files;
        state.path_to_id = path_to_id;
        state.trigrams = trigrams;
        state.words = words;
        state.deps = deps;
        state.versions = snap.versions;
        state.agents = agents;
        state.symbols = graph;
        state.last_built_unix_ms = snap.meta.indexed_at_ms;
        state.git_head = snap.meta.git_head;
        state.reconcile_progress = snap.reconcile_progress;
        state.set_next_file_id(snap.next_file_id);
        state.modules = super::imports::ModuleIndex::default().with_go_modules(snap.go_modules);
        // Membership and imported-module visibility derive from saved file
        // facts through the owning resolver. Explicit dependency rows alone
        // lose whole-module imports when a later edit resolves callers again.
        let _phase = super::timing::Phase::start("snapshot.restore_dependencies");
        state.rebuild_module_index();
        Ok(state)
    }

    /// Drop stale agent records and release any locks held by agents
    /// whose `last_seen_ms` is older than the configured timeout. Called
    /// at startup after restoring from a snapshot.
    pub fn reap_after_recovery(&mut self, now_ms: i64) {
        self.agents.reap(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/alpha.rs"),
            "pub fn alpha() -> i32 { 1 }\n",
        )
        .unwrap();
        dir
    }

    fn snapshot_for(dir: &Path) -> CodeIndexSnapshot {
        let (state, _) = IndexState::build_from_root(dir);
        state.snapshot()
    }

    #[test]
    fn load_rejects_snapshot_for_a_different_workspace_root() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.meta.workspace_root = "/some/other/checkout".to_string();
        snap.save(dir.path()).unwrap();
        assert!(
            CodeIndexSnapshot::load(dir.path()).unwrap().is_none(),
            "a snapshot copied from another checkout must be a miss, not adopted"
        );
    }

    #[test]
    fn load_reconciles_snapshot_when_git_head_does_not_match() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.meta.git_head = Some("ffffffffffffffff".to_string());
        snap.save(dir.path()).unwrap();
        assert!(
            CodeIndexSnapshot::load(dir.path()).unwrap().is_some(),
            "a changed HEAD requires reconciliation, not a cold rebuild"
        );
    }

    #[test]
    fn load_rejects_a_graphless_v1_snapshot() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.meta.format_version = 1;
        snap.symbols = None;
        snap.save(dir.path()).unwrap();
        assert!(
            CodeIndexSnapshot::load(dir.path()).unwrap().is_none(),
            "v1 cannot be served with an empty graph"
        );
    }

    #[test]
    fn load_rejects_a_v2_snapshot_with_order_dependent_edges() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.meta.format_version = 2;
        snap.save(dir.path()).unwrap();
        assert!(
            CodeIndexSnapshot::load(dir.path()).unwrap().is_none(),
            "a v2 graph would keep its REFS and CALLS edges for unchanged files"
        );
    }

    #[test]
    fn load_rejects_a_graphless_current_snapshot() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.symbols = None;
        snap.save(dir.path()).unwrap();
        assert!(CodeIndexSnapshot::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_rejects_v3_snapshots_without_checkpoint_semantics() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        snap.meta.format_version = 3;
        snap.save(dir.path()).unwrap();
        assert!(CodeIndexSnapshot::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn restore_refuses_duplicate_graph_node_ids() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        let graph = snap.symbols.as_mut().unwrap();
        graph.nodes.push(graph.nodes[0].clone());
        let error = IndexState::from_snapshot(snap)
            .err()
            .expect("invalid graph");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn load_refuses_a_truncated_snapshot() {
        let dir = fixture_tree();
        snapshot_for(dir.path()).save(dir.path()).unwrap();
        let path = CodeIndexSnapshot::path_for(dir.path());
        let bytes = fs::read(&path).unwrap();
        // Cut inside the header, then inside the body. Both are refused as
        // invalid data so the caller rebuilds instead of serving a prefix.
        for len in [SNAPSHOT_HEADER_LEN - 1, bytes.len() / 2] {
            fs::write(&path, &bytes[..len]).unwrap();
            let error = CodeIndexSnapshot::load(dir.path()).expect_err("refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "length {len}");
        }
    }

    #[test]
    fn load_refuses_a_file_without_the_binary_header() {
        let dir = fixture_tree();
        let snap = snapshot_for(dir.path());
        let path = CodeIndexSnapshot::path_for(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&snap).unwrap()).unwrap();
        let error = CodeIndexSnapshot::load(dir.path()).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_binary_save_retires_the_legacy_json_snapshot() {
        let dir = fixture_tree();
        let legacy = CodeIndexSnapshot::path_for(dir.path()).with_file_name(LEGACY_JSON_SNAPSHOT);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"{}").unwrap();
        snapshot_for(dir.path()).save(dir.path()).unwrap();
        assert!(!legacy.exists());
        assert!(CodeIndexSnapshot::load(dir.path()).unwrap().is_some());
    }

    #[test]
    fn restored_postings_forget_an_edited_files_old_terms() {
        let dir = fixture_tree();
        fs::write(
            dir.path().join("src/beta.rs"),
            "pub fn beta() -> i32 { 2 }\n",
        )
        .unwrap();
        snapshot_for(dir.path()).save(dir.path()).unwrap();
        let snap = CodeIndexSnapshot::load(dir.path()).unwrap().unwrap();
        let mut state = IndexState::from_snapshot(snap).unwrap();
        let alpha = state.path_to_id["src/alpha.rs"];
        let unique = super::super::trigram::query_trigrams("alpha");
        let trigram_files = |state: &IndexState| state.trigrams.query(&unique);
        assert_eq!(
            state.words.get("alpha").len(),
            1,
            "restored before the edit"
        );
        assert!(trigram_files(&state).contains(&alpha));

        // A same-size edit with a later timestamp, so the stat gate rereads it.
        let path = dir.path().join("src/alpha.rs");
        fs::write(&path, "pub fn gamma() -> i32 { 1 }\n").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(2_000_000_000, 0))
            .unwrap();
        assert_eq!(state.refresh_from_root(None).files_reindexed, 1);
        assert!(state.words.get("alpha").is_empty());
        assert!(!trigram_files(&state).contains(&alpha));
        assert_eq!(state.words.get("gamma").len(), 1);
        assert_eq!(state.words.get("beta").len(), 1, "other files keep theirs");
    }

    #[test]
    fn load_accepts_snapshot_for_the_same_tree() {
        let dir = fixture_tree();
        let snap = snapshot_for(dir.path());
        snap.save(dir.path()).unwrap();
        let loaded = CodeIndexSnapshot::load(dir.path())
            .unwrap()
            .expect("matching snapshot should restore");
        assert_eq!(loaded.meta.file_count, 1);
    }
}
