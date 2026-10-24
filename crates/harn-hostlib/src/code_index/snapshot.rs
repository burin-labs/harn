//! Persistent on-disk snapshot of the workspace index.
//!
//! One file at `.burin/index/snapshot.bin`: an eight-byte magic, the
//! little-endian format version, then postcard-encoded sections, each
//! preceded by its little-endian `u64` length. In order: file facts and
//! bookkeeping, the graph's next node id, graph edges, trigram postings,
//! word postings, the path and language of each file with graph nodes, and
//! then the graph's nodes split into up to [`NODE_SECTIONS`] runs. A node
//! record omits its path and language, which its file entry holds once for
//! every node of that file. The header lets a reader reject another layout
//! before decoding it, and every section decodes on its own thread because
//! the sections share nothing; nodes are split because they are the largest.
//! Earlier versions, including every JSON snapshot, are rebuilt rather than
//! restored.
//!
//! The snapshot is the recovery primitive. On daemon startup, the
//! embedder restores from the snapshot if one exists, then calls
//! [`super::IndexState::reap_after_recovery`] to drop stale agent
//! records and locks before serving any traffic.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::agents::{AgentRegistry, RegistryConfig, SerializedRegistry};
use super::file_table::{FileId, IndexedFile};
use super::graph::DepGraph;
use super::imports_go::GoModules;
use super::parallel::{joined, scope};
use super::symbol_graph::{Edge, GraphSnapshot, Node, NodeId, SymbolGraph};
use super::trigram::TrigramIndex;
use super::versions::VersionLog;
use super::words::{WordHit, WordIndex};
use super::IndexState;

/// Current format version. Bumped whenever the snapshot layout changes
/// in a non-additive way, or when graph edges a snapshot stores would be
/// computed differently, since a restore keeps the stored edges for every
/// file that has not changed. v4 adds partial HEAD-verification progress;
/// older readers must not treat a checkpoint as a fully verified HEAD when
/// the checkout reverts to the previously recorded commit. v5 replaces JSON
/// with a binary layout; v6 splits the graph into sections; v7 stores each
/// file's path and language once, as the module documentation describes.
/// v8 adds member-name columns and Java receiver ownership and rebuilds the
/// older name-only Java call edges. v9 rebuilds Java edges that could confuse
/// annotated type parameters with unrelated classes of the same name.
/// v10 rebuilds identifier positions missing from exported declaration spans.
/// v11 rebuilds Java edges that could miss switch-local shadows or inherited member types.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 11;

/// The most runs graph nodes are split into, each decoded on its own thread.
const NODE_SECTIONS: usize = 4;

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
    /// File table: the live rows themselves, so writing it from a live
    /// index copies nothing.
    pub files: Vec<IndexedFile>,
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

    /// Encode and atomically replace the snapshot. See
    /// [`IndexState::save_snapshot`], which writes the same layout straight
    /// from a live index without building this owned copy first.
    pub fn save(&self, workspace_root: &Path) -> std::io::Result<()> {
        let graph = self.symbols.as_ref();
        let words: Vec<(&str, Cow<'_, [WordHit]>)> = self
            .words
            .iter()
            .map(|posting| {
                let hits = posting
                    .hits
                    .iter()
                    .map(|&(file, line)| WordHit { file, line })
                    .collect();
                (posting.word.as_str(), Cow::Owned(hits))
            })
            .collect();
        SectionsRef {
            meta: &self.meta,
            next_file_id: self.next_file_id,
            files: self.files.iter().collect(),
            deps: &self.deps,
            versions: &self.versions,
            agents: &self.agents,
            reconcile_progress: &self.reconcile_progress,
            go_modules: &self.go_modules,
            next_node_id: graph.map(|graph| graph.next_id),
            nodes: graph.map_or_else(Vec::new, |graph| graph.nodes.iter().collect()),
            edges: graph.map_or(&[], |graph| &graph.edges),
            trigrams: self
                .trigrams
                .iter()
                .map(|posting| (&posting.trigram, Cow::Borrowed(posting.files.as_slice())))
                .collect(),
            words,
        }
        .encode()?
        .write(workspace_root)
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
        let mut sections = SectionReader(body);
        let (core, next_node_id, edges, trigrams, words, graph_files) = (
            sections.next()?,
            sections.next()?,
            sections.next()?,
            sections.next()?,
            sections.next()?,
            sections.next()?,
        );
        let graph_files = GraphFileTable::decode(graph_files)?;
        let mut node_runs = Vec::new();
        while !sections.0.is_empty() {
            node_runs.push(sections.next()?);
        }
        if node_runs.len() > NODE_SECTIONS {
            return Err(invalid("code-index snapshot has trailing sections"));
        }
        // The sections share nothing, so decoding takes the longest of them.
        let (core, next_node_id, edges, trigrams, words, node_runs) = scope(|scope| {
            let edges = scope.spawn(|| decode_section::<Vec<Edge>>("snapshot.decode_edges", edges));
            let trigrams = scope.spawn(|| decode_section("snapshot.decode_trigrams", trigrams));
            let words = scope.spawn(|| decode_section("snapshot.decode_words", words));
            let node_runs: Vec<_> = node_runs
                .iter()
                .map(|run| scope.spawn(|| graph_files.decode_nodes(run)))
                .collect();
            let core: io::Result<CoreSection> = decode_section("snapshot.decode_core", core);
            let next_node_id: io::Result<Option<NodeId>> =
                decode_section("snapshot.decode_core", next_node_id);
            (
                core,
                next_node_id,
                joined(edges),
                joined(trigrams),
                joined(words),
                node_runs.into_iter().map(joined).collect::<Vec<_>>(),
            )
        });
        let (meta, next_file_id, files, deps, versions, agents, reconcile_progress, go_modules) =
            core?;
        let symbols = match next_node_id? {
            Some(next_id) => {
                let mut runs = node_runs.into_iter();
                let mut nodes = runs.next().transpose()?.unwrap_or_default();
                for run in runs {
                    nodes.append(&mut run?);
                }
                Some(GraphSnapshot {
                    next_id,
                    nodes,
                    edges: edges?,
                })
            }
            None => None,
        };
        let snap = CodeIndexSnapshot {
            meta,
            next_file_id,
            files,
            trigrams: trigrams?,
            words: words?,
            deps,
            versions,
            agents,
            symbols,
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
    Vec<IndexedFile>,
    Vec<DepRow>,
    VersionLog,
    SerializedRegistry,
    Option<super::reconcile::ReconcileProgress>,
    GoModules,
);
/// A snapshot's sections, borrowed from whichever form of the index is
/// being written. An owned [`CodeIndexSnapshot`] and a live [`IndexState`]
/// both encode through this one layout, which the reader mirrors.
struct SectionsRef<'a> {
    meta: &'a SnapshotMeta,
    next_file_id: FileId,
    files: Vec<&'a IndexedFile>,
    deps: &'a [DepRow],
    versions: &'a VersionLog,
    agents: &'a SerializedRegistry,
    reconcile_progress: &'a Option<super::reconcile::ReconcileProgress>,
    go_modules: &'a GoModules,
    next_node_id: Option<NodeId>,
    nodes: Vec<&'a Node>,
    edges: &'a [Edge],
    trigrams: Vec<(&'a u32, Cow<'a, [FileId]>)>,
    words: Vec<(&'a str, Cow<'a, [WordHit]>)>,
}

impl SectionsRef<'_> {
    /// Encode every section, each on its own thread, in file order.
    fn encode(&self) -> io::Result<EncodedSnapshot> {
        let _phase = super::timing::Phase::start("snapshot.encode");
        let core = (
            self.meta,
            self.next_file_id,
            &self.files,
            self.deps,
            self.versions,
            self.agents,
            self.reconcile_progress,
            self.go_modules,
        );
        let run_len = self.nodes.len().div_ceil(NODE_SECTIONS).max(1);
        let graph_files = GraphFiles::of(&self.nodes)?;
        let sections = scope(|scope| {
            let next_node_id = scope.spawn(|| encode_section(&self.next_node_id));
            let edges = scope.spawn(|| encode_section(&self.edges));
            let trigrams = scope.spawn(|| encode_section(&self.trigrams));
            let words = scope.spawn(|| encode_section(&self.words));
            let runs: Vec<_> = self
                .nodes
                .chunks(run_len)
                .map(|run| scope.spawn(move || encode_section(&NodeRun(run))))
                .collect();
            let core = encode_section(&core);
            [
                core,
                joined(next_node_id),
                joined(edges),
                joined(trigrams),
                joined(words),
                encode_section(&graph_files),
            ]
            .into_iter()
            .chain(runs.into_iter().map(joined))
            .collect::<io::Result<Vec<_>>>()
        })?;
        Ok(EncodedSnapshot {
            format_version: self.meta.format_version,
            sections,
        })
    }
}

/// Each file's path and language, written once for all of its graph nodes.
#[derive(Serialize)]
struct GraphFiles<'a>(Vec<(FileId, &'a str, &'a str)>);

impl<'a> GraphFiles<'a> {
    /// Collect the file entries of `nodes`, refusing a node whose path or
    /// language disagrees with its file's, which this layout could not keep.
    fn of(nodes: &[&'a Node]) -> io::Result<Self> {
        let mut files: foldhash::HashMap<FileId, (&'a str, &'a str)> = Default::default();
        for node in nodes {
            let entry = *files
                .entry(node.file_id)
                .or_insert((&node.path, node.language));
            if entry != (&*node.path, node.language) {
                return Err(invalid("graph nodes of one file disagree on its path"));
            }
        }
        let mut files: Vec<_> = files
            .into_iter()
            .map(|(file, (path, language))| (file, path, language))
            .collect();
        files.sort_unstable_by_key(|(file, _, _)| *file);
        Ok(Self(files))
    }
}

/// Decoded file entries, shared by every node of each file.
struct GraphFileTable(foldhash::HashMap<FileId, (std::sync::Arc<str>, &'static str)>);

impl GraphFileTable {
    fn decode(bytes: &[u8]) -> io::Result<GraphFileTable> {
        let rows: Vec<(FileId, String, String)> =
            decode_section("snapshot.decode_graph_files", bytes)?;
        let mut table = foldhash::HashMap::default();
        for (file, path, language) in rows {
            let language = super::symbol_graph::static_language_name(&language)
                .ok_or_else(|| invalid("code-index snapshot names an unknown graph language"))?;
            table.insert(file, (std::sync::Arc::from(path), language));
        }
        Ok(GraphFileTable(table))
    }
}

/// A node as a snapshot stores it: every field but the path and language its
/// file entry already holds.
type NodeRecord = (
    NodeId,
    super::symbol_graph::NodeKind,
    String,
    FileId,
    u32,
    String,
    Option<String>,
    Option<String>,
    Option<u32>,
    Option<String>,
);

/// A run of nodes written as [`NodeRecord`]s without copying them.
struct NodeRun<'a, 'n>(&'a [&'n Node]);

impl Serialize for NodeRun<'_, '_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(|node| {
            (
                node.id,
                node.kind,
                &node.name,
                node.file_id,
                node.line,
                &node.signature,
                &node.container,
                &node.access_level,
                node.col,
                &node.owner_type,
            )
        }))
    }
}

impl GraphFileTable {
    /// Decode one run of node records and give each its file's shared path
    /// and language.
    fn decode_nodes(&self, bytes: &[u8]) -> io::Result<Vec<Node>> {
        let records: Vec<NodeRecord> = decode_section("snapshot.decode_nodes", bytes)?;
        records
            .into_iter()
            .map(
                |(
                    id,
                    kind,
                    name,
                    file_id,
                    line,
                    signature,
                    container,
                    access_level,
                    col,
                    owner_type,
                )| {
                    let (path, language) = self
                        .0
                        .get(&file_id)
                        .ok_or_else(|| invalid("graph node names a file with no entry"))?;
                    Ok(Node {
                        id,
                        kind,
                        name,
                        file_id,
                        path: path.clone(),
                        line,
                        col,
                        owner_type,
                        signature,
                        container,
                        access_level,
                        language,
                    })
                },
            )
            .collect()
    }
}

fn encode_section<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    postcard::to_allocvec(value).map_err(io::Error::other)
}

/// What a snapshot of a live index holds, detached from that index so it can
/// be encoded without holding the index's lock.
pub(super) struct SnapshotSource {
    meta: SnapshotMeta,
    next_file_id: FileId,
    files: Vec<IndexedFile>,
    deps: Vec<DepRow>,
    versions: VersionLog,
    agents: SerializedRegistry,
    reconcile_progress: Option<super::reconcile::ReconcileProgress>,
    go_modules: GoModules,
    symbols: SymbolGraph,
    trigrams: TrigramIndex,
    words: WordIndex,
}

impl SnapshotSource {
    pub(super) fn encode(&self) -> io::Result<EncodedSnapshot> {
        let (next_node_id, nodes, edges) = self.symbols.snapshot_refs();
        SectionsRef {
            meta: &self.meta,
            next_file_id: self.next_file_id,
            files: self.files.iter().collect(),
            deps: &self.deps,
            versions: &self.versions,
            agents: &self.agents,
            reconcile_progress: &self.reconcile_progress,
            go_modules: &self.go_modules,
            next_node_id: Some(next_node_id),
            nodes,
            edges: &edges,
            trigrams: self.trigrams.posting_rows(),
            words: self.words.posting_rows(),
        }
        .encode()
    }
}

/// A snapshot's encoded sections, ready to write without the index.
pub(super) struct EncodedSnapshot {
    format_version: u32,
    sections: Vec<Vec<u8>>,
}

impl EncodedSnapshot {
    /// Write to a unique sibling temporary file and atomically replace the
    /// snapshot, so concurrent writers never share one `.tmp` path.
    pub(super) fn write(&self, workspace_root: &Path) -> io::Result<()> {
        let _phase = super::timing::Phase::start("snapshot.write");
        let path = CodeIndexSnapshot::path_for(workspace_root);
        let parent = path.parent().expect("snapshot path has a parent");
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(tmp.as_file_mut());
            writer.write_all(SNAPSHOT_MAGIC)?;
            writer.write_all(&self.format_version.to_le_bytes())?;
            for section in &self.sections {
                writer.write_all(&(section.len() as u64).to_le_bytes())?;
                writer.write_all(section)?;
            }
            writer.flush()?;
        }
        tmp.persist(&path).map_err(|error| error.error)?;
        #[cfg(test)]
        super::persistence_tests::after_replace(workspace_root);
        // A superseded JSON snapshot is hundreds of megabytes nothing reads.
        let _ = std::fs::remove_file(parent.join(LEGACY_JSON_SNAPSHOT));
        Ok(())
    }
}

/// Length-prefixed sections of a snapshot body.
struct SectionReader<'a>(&'a [u8]);

impl<'a> SectionReader<'a> {
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
    /// Capture the current state as an owned [`CodeIndexSnapshot`]. Writing
    /// one is [`Self::save_snapshot`], which skips this copy.
    pub fn snapshot(&self) -> CodeIndexSnapshot {
        let _phase = super::timing::Phase::start("snapshot.capture");
        CodeIndexSnapshot {
            meta: self.snapshot_meta(),
            next_file_id: self.next_file_id_internal(),
            files: self.files.values().cloned().collect(),
            trigrams: self.trigrams.snapshot_postings(),
            words: self.words.snapshot_postings(),
            deps: self.deps.snapshot_rows(),
            versions: self.versions.clone(),
            agents: self.agents.snapshot(),
            symbols: Some(self.symbols.snapshot()),
            reconcile_progress: self.reconcile_progress.clone(),
            go_modules: self.modules.go_modules().clone(),
        }
    }

    /// Encode and atomically replace the on-disk snapshot, borrowing from
    /// the live index rather than copying it first.
    pub fn save_snapshot(&self) -> io::Result<()> {
        self.snapshot_source().encode()?.write(&self.root)
    }

    /// Detach what a snapshot holds from the live index. The graph and the
    /// postings are shared generations, so this copies only their edits plus
    /// the file table; a shared owner holds its lock for this, not for the
    /// encode or the write.
    pub(super) fn snapshot_source(&self) -> SnapshotSource {
        let _phase = super::timing::Phase::start("snapshot.detach");
        SnapshotSource {
            meta: self.snapshot_meta(),
            next_file_id: self.next_file_id_internal(),
            files: self.files.values().cloned().collect(),
            deps: self.deps.snapshot_rows(),
            versions: self.versions.clone(),
            agents: self.agents.snapshot(),
            reconcile_progress: self.reconcile_progress.clone(),
            go_modules: self.modules.go_modules().clone(),
            symbols: self.symbols.clone(),
            trigrams: self.trigrams.clone(),
            words: self.words.clone(),
        }
    }

    fn snapshot_meta(&self) -> SnapshotMeta {
        SnapshotMeta {
            format_version: SNAPSHOT_FORMAT_VERSION,
            workspace_root: self.root.to_string_lossy().into_owned(),
            git_head: self.git_head.clone(),
            indexed_at_ms: self.last_built_unix_ms,
            file_count: self.files.len(),
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
        let (graph, trigrams, words) = scope(|scope| {
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
        for file in snap.files {
            path_to_id.insert(file.relative_path.clone(), file.id);
            files.insert(file.id, file);
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
    fn live_and_owned_saves_write_identical_bytes() {
        let dir = fixture_tree();
        let root = dir.path();
        fs::write(
            root.join("src/beta.rs"),
            "pub fn beta() -> i32 { alpha() }\n",
        )
        .unwrap();
        snapshot_for(root).save(root).unwrap();
        let mut state =
            IndexState::from_snapshot(CodeIndexSnapshot::load(root).unwrap().unwrap()).unwrap();
        // Edit over the restored base, so the graph has a delta and the
        // postings have tombstones as well as delta hits.
        let path = root.join("src/alpha.rs");
        fs::write(&path, "pub fn alpha() -> i32 { beta() }\n").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(2_000_000_000, 0))
            .unwrap();
        assert_eq!(state.refresh_from_root(None).files_reindexed, 1);

        let snapshot_path = CodeIndexSnapshot::path_for(root);
        state.save_snapshot().unwrap();
        let live = fs::read(&snapshot_path).unwrap();
        state.snapshot().save(root).unwrap();
        let owned = fs::read(&snapshot_path).unwrap();
        assert_eq!(live, owned, "both writers share one layout");
        let restored =
            IndexState::from_snapshot(CodeIndexSnapshot::load(root).unwrap().unwrap()).unwrap();
        assert_eq!(restored.words.get("beta").len(), 2);
        assert_eq!(restored.symbols.node_count(), state.symbols.node_count());
    }

    #[test]
    fn restored_nodes_of_one_file_share_its_path_and_language() {
        let dir = fixture_tree();
        snapshot_for(dir.path()).save(dir.path()).unwrap();
        let snap = CodeIndexSnapshot::load(dir.path()).unwrap().unwrap();
        let state = IndexState::from_snapshot(snap).unwrap();
        let nodes: Vec<_> = state
            .symbols
            .iter_nodes()
            .filter(|node| &*node.path == "src/alpha.rs")
            .collect();
        assert!(nodes.len() >= 2, "a module node and a function node");
        assert!(nodes
            .windows(2)
            .all(|pair| std::sync::Arc::ptr_eq(&pair[0].path, &pair[1].path)));
        assert!(nodes.iter().all(|node| node.language == "rust"));
    }

    #[test]
    fn saving_refuses_nodes_that_disagree_on_their_files_path() {
        let dir = fixture_tree();
        let mut snap = snapshot_for(dir.path());
        let nodes = &mut snap.symbols.as_mut().unwrap().nodes;
        assert!(nodes.len() >= 2);
        let file = nodes[0].file_id;
        let other = nodes.iter().rposition(|node| node.file_id == file).unwrap();
        assert_ne!(other, 0, "the fixture file has several nodes");
        nodes[other].path = std::sync::Arc::from("src/elsewhere.rs");
        let error = snap.save(dir.path()).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn load_refuses_an_unknown_graph_language() {
        let dir = fixture_tree();
        let snap = snapshot_for(dir.path());
        let mut bytes = serde_json::to_value(&snap).unwrap();
        bytes["symbols"]["nodes"][0]["language"] = "klingon".into();
        let error = serde_json::from_value::<CodeIndexSnapshot>(bytes).expect_err("refused");
        assert!(error.to_string().contains("unknown graph language"));
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
