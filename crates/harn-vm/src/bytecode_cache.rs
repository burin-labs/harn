//! Content-addressed on-disk cache for compiled `.harn` pipelines.
//!
//! Cold-start `harn run` re-parses, type-checks, and compiles the entry
//! pipeline before the VM gets a single instruction to execute. For short
//! Harn subcommands that wrap a few `llm_call`s in a small pipeline, that
//! compile cost dominates wall-clock time.
//!
//! This module persists [`Chunk`] bytecode under
//! `$HARN_CACHE_DIR/<source-hash>.harnbc` (XDG-aware). The cache key is
//! derived from the source plus its compilation context. Entry chunks include
//! the content of every transitively-imported user file because they compile
//! the complete program. Module artifacts compile exactly one file and retain
//! unresolved import specs, so their context includes compiler and embedded
//! stdlib identity but deliberately excludes user dependencies. Any change to
//! an artifact's actual compilation inputs flips the key and recompiles it.
//!
//! File layout — little-endian throughout:
//!
//! ```text
//! magic        : [u8; 8]   = "HARNBC\0\0"
//! schema_ver   : u32       = SCHEMA_VERSION
//! version_len  : u32
//! harn_version : [u8; version_len]
//! fp_len       : u32
//! codegen_fp   : [u8; fp_len]   CODEGEN_FINGERPRINT of the producing build
//! compiler_tag : u8        bitmask of active CompilerOptions
//! kind         : u8        1 = entry chunk, 2 = module artifact
//! source_hash  : [u8; 32]
//! context_hash : [u8; 32]
//! payload      : postcard-serialized payload for `kind`
//! ```
//!
//! The header lets a stale binary detect a future-version artifact
//! without crashing: a magic mismatch, schema mismatch, or version
//! mismatch is returned as `Ok(None)` so the caller transparently
//! recompiles. Real I/O errors propagate.
//!
//! Concurrency: writes go through [`crate::atomic_io`] (write-tmp, fsync,
//! rename, fsync parent dir), and parallel invocations on a cache miss race
//! safely — the last writer wins, but every reader observes a consistent file
//! because the rename is atomic on every supported filesystem.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::chunk::{CachedChunk, Chunk};
use crate::compiler::CompilerOptions;
use crate::context_manifest::{
    ContextManifest, ManifestCheck, ManifestFile, ManifestUnreadable, ManifestUnresolved,
};
use crate::module_artifact::ModuleArtifact;
use crate::module_source::{self, ModuleSource};

/// Header magic for all bytecode-cache artifact families.
pub const MAGIC: &[u8; 8] = b"HARNBC\0\0";

/// On-disk format version. Bump when [`CachedChunk`] or the header
/// layout changes in a backwards-incompatible way.
/// v5: `ModuleArtifact` gained `public_type_names` (`pub type` exports).
/// v6: payload encoding replaced with postcard.
/// v7: exported type schemas moved from eager JSON strings to an initializer
/// chunk that resolves imported aliases in the module environment.
/// v7: `ModuleArtifact` replaced split name sets with the typed public export
/// contract shared by the module graph and runtime.
/// v8: entry-chunk payload carries a [`ContextManifest`] so a warm lookup can
/// prove the import graph is unchanged with stats instead of re-walking it.
/// v9: the manifest records the entry it was walked from, so it cannot vouch
/// for a different entry that happens to have identical source bytes (#5591);
/// the header carries [`CODEGEN_FINGERPRINT`], which the manifest fast path
/// needs in order to reject a chunk built by another compiler at the same
/// version (#5610); and manifest entries carry a content digest, and the
/// manifest a capture time, so a rewrite inside the filesystem's timestamp
/// granularity cannot present itself as unchanged (#5582).
pub const SCHEMA_VERSION: u32 = 9;

/// Compile-time Harn release. Cache files written by a different release
/// are rejected on load.
pub const HARN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build-time fingerprint of the compiler front-end — the lexer, parser, IR,
/// and code generator — computed in `build.rs` from those crates' source and
/// baked in via `cargo:rustc-env`. Folded into the cache key so a compiler
/// change that alters emitted bytecode for unchanged source invalidates stale
/// entries automatically, within a single version, with no manual cache wipe.
/// `HARN_VERSION` only busts the cache across release bumps; this closes the
/// same gap for the within-version compiler edits that masked #2610. See #2621.
///
/// It reaches a lookup two ways. The header comparison is what *rejects* a
/// stale artifact, and is the only one the entry fast path can afford, since
/// that path proves its graph from a manifest and never recomputes the context
/// hash (#5610). Folding it into the context hash as well is what keeps two
/// builds' module artifacts on distinct filenames rather than overwriting each
/// other, since `module_filename` is derived from that hash.
pub const CODEGEN_FINGERPRINT: &str = env!("HARN_CODEGEN_FINGERPRINT");

/// Conventional extension for entry-chunk cache files.
pub const CACHE_EXTENSION: &str = "harnbc";

/// Conventional extension for module-artifact cache files. Distinct from
/// [`CACHE_EXTENSION`] so the same `.harn` source can have both shipped
/// adjacent if needed (e.g. when a file is both an executable entry and
/// imported by other files).
pub const MODULE_CACHE_EXTENSION: &str = "harnmod";

/// On-disk discriminant for a [`Chunk`] payload.
const KIND_ENTRY_CHUNK: u8 = 1;
/// On-disk discriminant for a [`ModuleArtifact`] payload.
const KIND_MODULE_ARTIFACT: u8 = 2;

/// Environment override for the cache directory. When set, takes
/// precedence over the XDG and home-directory fallbacks.
pub const CACHE_DIR_ENV: &str = "HARN_CACHE_DIR";

/// Environment override that turns the cache off entirely. Setting this
/// to `0`, `false`, `no`, or `off` skips both reads and writes; useful
/// when debugging compiler changes.
pub const CACHE_ENABLED_ENV: &str = "HARN_BYTECODE_CACHE";

/// Result of a cache lookup. Carries the precomputed key so the caller
/// can write it back on a miss without rehashing.
pub struct LookupOutcome {
    pub key: CacheKey,
    pub chunk: Option<Chunk>,
    /// Graph observations to persist alongside the chunk, so the next spawn can
    /// re-check them with stats instead of walking. `None` when the graph holds
    /// something stats cannot describe.
    pub manifest: Option<ContextManifest>,
}

impl LookupOutcome {
    /// Persist `chunk` under the key this lookup computed, with the manifest it
    /// observed.
    ///
    /// The pairing is the point: the key and the manifest describe one walk of
    /// one graph, and storing a chunk against a manifest from a different walk
    /// would let a later spawn prove the wrong thing. Callers cannot get that
    /// pairing wrong if they never have to assemble it.
    pub fn store(&self, chunk: &Chunk) -> io::Result<()> {
        store(&self.key, chunk, self.manifest.as_ref())
    }
}

/// Cache key components for a single pipeline source. Equality of all
/// fields is necessary and sufficient for cache reuse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub source_hash: [u8; 32],
    pub context_hash: [u8; 32],
    pub harn_version: &'static str,
    /// Compact tag for active [`CompilerOptions`]. Flipping
    /// `HARN_DISABLE_OPTIMIZATIONS` between runs would otherwise reuse a
    /// chunk compiled under the wrong setting.
    pub compiler_tag: u8,
}

impl CacheKey {
    /// Compute the cache key for a `.harn` source file plus its transitive
    /// user imports. `source` is the entry-file contents; the import
    /// graph is walked from disk relative to `source_path`.
    pub fn from_source(source_path: &Path, source: &str) -> Self {
        let source_hash = sha256(source.as_bytes());
        let context_hash = hash_transitive_user_imports(source_path, source);
        Self {
            source_hash,
            context_hash,
            harn_version: HARN_VERSION,
            compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
        }
    }

    /// Compute the cache key for one independently-compiled module artifact.
    ///
    /// A [`ModuleArtifact`] stores unresolved import specs and never compiles
    /// dependency contents into the parent artifact. Walking the transitive
    /// graph here therefore adds cold-start I/O without protecting correctness:
    /// the runtime loads every dependency under its own source-local key.
    /// Diagnostic paths are rebound when the artifact is loaded, so adjacent
    /// and packaged artifacts remain relocatable without aliasing attribution.
    pub fn from_module_source(source: &ModuleSource) -> Self {
        Self {
            source_hash: source.sha256(),
            context_hash: module_compilation_context_hash(),
            harn_version: HARN_VERSION,
            compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
        }
    }

    /// Entry-chunk filename for this key. We hash by source content
    /// alone so two invocations of the same source from different paths
    /// share a cache entry; the header's compilation-context hash still gates
    /// reuse on a per-load basis.
    pub fn filename(&self) -> String {
        format!("{}.{}", hex(&self.source_hash), CACHE_EXTENSION)
    }

    /// Module-artifact filename for this complete compilation key. Diagnostic
    /// source paths are rebound at load time, so identical source and compiler
    /// inputs share one relocatable artifact across paths.
    pub fn module_filename(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.source_hash);
        hasher.update(self.context_hash);
        hasher.update(self.harn_version.as_bytes());
        hasher.update([self.compiler_tag]);
        let identity: [u8; 32] = hasher.finalize().into();
        format!("{}.{}", hex(&identity), MODULE_CACHE_EXTENSION)
    }
}

/// Returns the directory the shared cache lives in. Honors
/// `$HARN_CACHE_DIR`, then `$XDG_CACHE_HOME/harn/bytecode`, then
/// `$HOME/.cache/harn/bytecode`. The directory is *not* created here —
/// [`store`] creates it lazily on write so read-only environments don't
/// pay an mkdir cost.
pub fn cache_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os(CACHE_DIR_ENV) {
        return PathBuf::from(custom);
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return xdg.join("harn").join("bytecode");
        }
    }
    if let Some(home) = crate::user_dirs::home_dir() {
        return home.join(".cache").join("harn").join("bytecode");
    }
    // Final fallback: a directory beside the binary's working dir. Mostly
    // hit in tests that scrub HOME from the environment.
    PathBuf::from(".harn-cache").join("bytecode")
}

/// Root for `.harnpack` archives unpacked by `harn run <bundle.harnpack>`.
/// Each verified bundle is replayed into `<root>/<sanitized-bundle-hash>/`
/// so re-runs reuse the unpacked tree. Honors `$HARN_CACHE_DIR/packs`
/// when set, otherwise XDG / `$HOME/.cache/harn/packs`.
pub fn packs_cache_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os(CACHE_DIR_ENV) {
        return PathBuf::from(custom).join("packs");
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return xdg.join("harn").join("packs");
        }
    }
    if let Some(home) = crate::user_dirs::home_dir() {
        return home.join(".cache").join("harn").join("packs");
    }
    PathBuf::from(".harn-cache").join("packs")
}

/// True when the cache is enabled by the current environment.
pub fn cache_enabled() -> bool {
    match std::env::var(CACHE_ENABLED_ENV).ok().as_deref() {
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// Try to load a cached chunk for `source_path` whose contents are
/// `source`. Returns the key alongside the (optional) chunk so callers
/// avoid recomputing the key on miss.
pub fn load(source_path: &Path, source: &str) -> LookupOutcome {
    // Only the entry file's own hash is needed to find candidates. The context
    // hash — the expensive half — is deferred until a candidate actually asks
    // for it, because a candidate carrying a still-valid manifest never does.
    let mut key = CacheKey {
        source_hash: sha256(source.as_bytes()),
        context_hash: [0u8; 32],
        harn_version: HARN_VERSION,
        compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
    };
    let mut walk = GraphWalk::new(source_path, source);

    if !cache_enabled() {
        let (context_hash, manifest) = walk.finish();
        key.context_hash = context_hash;
        return LookupOutcome {
            key,
            chunk: None,
            manifest,
        };
    }

    let mut candidates: Vec<PathBuf> = Vec::with_capacity(2);
    if let Some(adjacent) = adjacent_cache_path(source_path) {
        candidates.push(adjacent);
    }
    candidates.push(cache_dir().join(key.filename()));

    // Candidates are found by entry source hash alone, so a candidate may have
    // been written by a *different* entry with byte-identical source. Its
    // manifest has to say it describes this one before its observations mean
    // anything here.
    let entry = module_source::canonical_identity(source_path);

    for path in candidates {
        let Ok(Some(candidate)) = read_entry_candidate(&path, &key) else {
            continue;
        };
        match candidate
            .manifest
            .as_ref()
            .map(|manifest| manifest.check(&entry))
        {
            // The graph is provably unchanged, so the stored context hash is
            // still the one this source would produce.
            Some(ManifestCheck::Valid) => {
                key.context_hash = candidate.context_hash;
                return LookupOutcome {
                    key,
                    chunk: Some(candidate.chunk),
                    manifest: candidate.manifest,
                };
            }
            // Same answer, but it cost a content read because some entry was
            // still inside the racy window when this manifest was captured.
            // Writing the re-stamped manifest back settles those entries, so
            // the read is paid once rather than on every later spawn.
            Some(ManifestCheck::ValidAfterRecheck { refreshed }) => {
                key.context_hash = candidate.context_hash;
                let _ = write_atomic_chunk(&path, &key, &candidate.chunk, Some(&refreshed));
                return LookupOutcome {
                    key,
                    chunk: Some(candidate.chunk),
                    manifest: Some(refreshed),
                };
            }
            Some(ManifestCheck::Stale) | None => {}
        }
        if walk.context_hash() != candidate.context_hash {
            continue;
        }
        // The graph moved in a way that does not change the key — a touched
        // mtime, a restored checkout. Refresh the artifact so the next spawn
        // gets the fast path back instead of re-walking forever.
        key.context_hash = candidate.context_hash;
        let manifest = walk.manifest().cloned();
        let _ = write_atomic_chunk(&path, &key, &candidate.chunk, manifest.as_ref());
        return LookupOutcome {
            key,
            chunk: Some(candidate.chunk),
            manifest,
        };
    }

    let (context_hash, manifest) = walk.finish();
    key.context_hash = context_hash;
    LookupOutcome {
        key,
        chunk: None,
        manifest,
    }
}

/// The import-graph walk, run at most once per lookup and only when a
/// candidate cannot prove itself with its manifest.
struct GraphWalk<'a> {
    source_path: &'a Path,
    source: &'a str,
    result: Option<([u8; 32], Option<ContextManifest>)>,
}

impl<'a> GraphWalk<'a> {
    fn new(source_path: &'a Path, source: &'a str) -> Self {
        Self {
            source_path,
            source,
            result: None,
        }
    }

    fn run(&mut self) -> &([u8; 32], Option<ContextManifest>) {
        self.result.get_or_insert_with(|| {
            hash_transitive_user_imports_with_manifest(self.source_path, self.source)
        })
    }

    fn context_hash(&mut self) -> [u8; 32] {
        self.run().0
    }

    fn manifest(&mut self) -> Option<&ContextManifest> {
        self.run().1.as_ref()
    }

    fn finish(mut self) -> ([u8; 32], Option<ContextManifest>) {
        self.run();
        self.result.expect("the walk was just run")
    }
}

/// Persist `chunk` to the shared cache directory under `key`. Atomic: a
/// temp file is written then renamed into place. Concurrent invocations
/// on the same key race safely.
pub fn store(key: &CacheKey, chunk: &Chunk, manifest: Option<&ContextManifest>) -> io::Result<()> {
    if !cache_enabled() {
        return Ok(());
    }
    let dir = cache_dir();
    fs::create_dir_all(&dir)?;
    write_atomic_chunk(&dir.join(key.filename()), key, chunk, manifest)
}

/// Write a precompiled entry-chunk artifact to an explicit path, for
/// use by the `harn precompile` subcommand. The header still records
/// the key, so adjacent artifacts shipped with source are validated
/// like any other cache hit.
pub fn store_at(path: &Path, key: &CacheKey, chunk: &Chunk) -> io::Result<()> {
    ensure_parent_dir(path)?;
    write_atomic_chunk(path, key, chunk, None)
}

/// Look up the [`ModuleArtifact`] for `source_path` (whose contents are
/// `source`). Mirrors [`load`] but for the `.harnmod` family.
pub fn load_module(source_path: &Path, source: &ModuleSource) -> ModuleLookupOutcome {
    let key = CacheKey::from_module_source(source);
    if !cache_enabled() {
        return ModuleLookupOutcome {
            key,
            artifact: None,
        };
    }
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(2);
    if let Some(adjacent) = adjacent_module_cache_path(source_path) {
        candidates.push(adjacent);
    }
    candidates.push(cache_dir().join(key.module_filename()));
    for path in candidates {
        match read_module_if_matches(&path, &key, source_path) {
            Ok(Some(artifact)) => {
                return ModuleLookupOutcome {
                    key,
                    artifact: Some(artifact),
                }
            }
            Ok(None) => continue,
            Err(_) => continue,
        }
    }
    ModuleLookupOutcome {
        key,
        artifact: None,
    }
}

/// Persist `artifact` to the shared cache under `key`. Atomic;
/// concurrent invocations race safely.
pub fn store_module(key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<()> {
    if !cache_enabled() {
        return Ok(());
    }
    let dir = cache_dir();
    fs::create_dir_all(&dir)?;
    write_atomic_module(&dir.join(key.module_filename()), key, artifact)
}

/// Write a module artifact to an explicit path.
pub fn store_module_at(path: &Path, key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<()> {
    ensure_parent_dir(path)?;
    write_atomic_module(path, key, artifact)
}

/// Result of a [`load_module`] lookup. Carries the precomputed key so
/// the caller can write it back on a miss without rehashing.
pub struct ModuleLookupOutcome {
    pub key: CacheKey,
    pub artifact: Option<ModuleArtifact>,
}

/// Path to the adjacent precompiled entry-chunk artifact for
/// `source_path`. `foo.harn` → `foo.harnbc`.
pub fn adjacent_cache_path(source_path: &Path) -> Option<PathBuf> {
    adjacent_path_with_extension(source_path, CACHE_EXTENSION)
}

/// Path to the adjacent precompiled module-artifact for `source_path`.
/// `foo.harn` → `foo.harnmod`.
pub fn adjacent_module_cache_path(source_path: &Path) -> Option<PathBuf> {
    adjacent_path_with_extension(source_path, MODULE_CACHE_EXTENSION)
}

fn adjacent_path_with_extension(source_path: &Path, ext: &str) -> Option<PathBuf> {
    let stem = source_path.file_stem()?;
    if stem.is_empty() {
        return None;
    }
    let parent = source_path.parent().unwrap_or_else(|| Path::new(""));
    let mut out = parent.join(stem);
    out.set_extension(ext);
    Some(out)
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn write_atomic_chunk(
    target: &Path,
    key: &CacheKey,
    chunk: &Chunk,
    manifest: Option<&ContextManifest>,
) -> io::Result<()> {
    let buf = serialize_chunk_artifact_with_manifest(key, chunk, manifest)?;
    crate::atomic_io::atomic_write(target, &buf)
}

fn write_atomic_module(target: &Path, key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<()> {
    let buf = serialize_module_artifact(key, artifact)?;
    crate::atomic_io::atomic_write(target, &buf)
}

/// Serialize an entry-chunk artifact (header + payload) to bytes. The
/// resulting buffer is byte-identical to the file [`store_at`] would
/// have written for the same `(key, chunk)`. Use this when packaging
/// artifacts into a container (e.g. `harn pack`) without going through
/// the filesystem.
pub fn serialize_chunk_artifact(key: &CacheKey, chunk: &Chunk) -> io::Result<Vec<u8>> {
    serialize_chunk_artifact_with_manifest(key, chunk, None)
}

/// As [`serialize_chunk_artifact`], but records `manifest` so a later lookup
/// can prove the graph unchanged without walking it.
///
/// Callers producing *relocatable* artifacts (`harn pack`, `harn precompile`)
/// pass `None`: a manifest names absolute paths on the machine that built it,
/// which say nothing on the machine that runs it. Those artifacts stay on the
/// walk, which is correct everywhere.
pub fn serialize_chunk_artifact_with_manifest(
    key: &CacheKey,
    chunk: &Chunk,
    manifest: Option<&ContextManifest>,
) -> io::Result<Vec<u8>> {
    let payload = serialize_cache_payload(&EntryPayload {
        manifest: manifest.cloned(),
        chunk: chunk.freeze_for_cache(),
    })?;
    Ok(encode_artifact(key, KIND_ENTRY_CHUNK, &payload))
}

/// Serialize a module artifact (header + payload) to bytes. Companion
/// to [`serialize_chunk_artifact`] for the `.harnmod` family.
pub fn serialize_module_artifact(key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<Vec<u8>> {
    let payload = serialize_cache_payload(artifact)?;
    Ok(encode_artifact(key, KIND_MODULE_ARTIFACT, &payload))
}

/// Entry-chunk payload. The manifest rides with the chunk so one atomic write
/// keeps them consistent: a chunk can never be paired with a manifest that
/// describes a different graph.
#[derive(serde::Serialize, serde::Deserialize)]
struct EntryPayload {
    manifest: Option<ContextManifest>,
    chunk: CachedChunk,
}

fn serialize_cache_payload<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    postcard::to_allocvec(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
}

fn deserialize_cache_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, String> {
    let (value, remaining) = postcard::take_from_bytes(payload).map_err(|err| err.to_string())?;
    if remaining.is_empty() {
        Ok(value)
    } else {
        Err("cache payload contains trailing bytes".to_string())
    }
}

fn encode_artifact(key: &CacheKey, kind: u8, payload: &[u8]) -> Vec<u8> {
    encode_artifact_fingerprinted(key, kind, payload, CODEGEN_FINGERPRINT)
}

/// Inner form of [`encode_artifact`] parameterized on the compiler fingerprint
/// so tests can write an artifact as if a different build had produced it;
/// production always passes [`CODEGEN_FINGERPRINT`].
fn encode_artifact_fingerprinted(
    key: &CacheKey,
    kind: u8,
    payload: &[u8],
    codegen_fingerprint: &str,
) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::with_capacity(payload.len() + 128);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    let version_bytes = HARN_VERSION.as_bytes();
    buf.extend_from_slice(&(version_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(version_bytes);
    let fingerprint_bytes = codegen_fingerprint.as_bytes();
    buf.extend_from_slice(&(fingerprint_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(fingerprint_bytes);
    buf.push(key.compiler_tag);
    buf.push(kind);
    buf.extend_from_slice(&key.source_hash);
    buf.extend_from_slice(&key.context_hash);
    buf.extend_from_slice(payload);
    buf
}

/// Reads `len` bytes and reports whether they equal `expected`.
///
/// `len` comes off disk, so it is bounded before it becomes an allocation: a
/// corrupted or hostile file must not be able to ask for an unbounded read.
/// A length that cannot match `expected` is rejected without reading at all.
fn read_length_prefixed_match(file: &mut fs::File, len: usize, expected: &[u8]) -> bool {
    if len > 256 || len != expected.len() {
        return false;
    }
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).is_ok() && buf == expected
}

/// Parsed cache header. Read by both the chunk and module loaders so the
/// header-validation logic stays in one place.
struct ParsedHeader {
    kind: u8,
    context_hash: [u8; 32],
    payload: Vec<u8>,
}

/// Read and validate a header.
///
/// `expected_context` is `None` for entry chunks, which decide validity from
/// the artifact's own manifest before they are willing to pay for the
/// context hash. Every other field is checked the same way for both families.
fn read_header_if_matches(
    path: &Path,
    key: &CacheKey,
    expected_context: Option<&[u8; 32]>,
) -> io::Result<Option<ParsedHeader>> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut header = [0u8; 8 + 4 + 4];
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    if &header[..8] != MAGIC {
        return Ok(None);
    }
    let schema = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if schema != SCHEMA_VERSION {
        return Ok(None);
    }
    let version_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    if !read_length_prefixed_match(&mut file, version_len, key.harn_version.as_bytes()) {
        return Ok(None);
    }
    // Which build produced this artifact, checkable without computing anything.
    // The entry fast path proves its graph unchanged from a manifest and never
    // recomputes the context hash, so a fingerprint carried only inside that
    // hash would go unexamined and a chunk from a previous build of the same
    // release would be replayed. See #5610.
    let mut fingerprint_len_bytes = [0u8; 4];
    if file.read_exact(&mut fingerprint_len_bytes).is_err() {
        return Ok(None);
    }
    let fingerprint_len = u32::from_le_bytes(fingerprint_len_bytes) as usize;
    if !read_length_prefixed_match(&mut file, fingerprint_len, CODEGEN_FINGERPRINT.as_bytes()) {
        return Ok(None);
    }
    let mut compiler_and_kind = [0u8; 2];
    if file.read_exact(&mut compiler_and_kind).is_err() {
        return Ok(None);
    }
    if compiler_and_kind[0] != key.compiler_tag {
        return Ok(None);
    }
    let kind = compiler_and_kind[1];
    let mut hashes = [0u8; 64];
    if file.read_exact(&mut hashes).is_err() {
        return Ok(None);
    }
    if hashes[..32] != key.source_hash {
        return Ok(None);
    }
    let mut context_hash = [0u8; 32];
    context_hash.copy_from_slice(&hashes[32..]);
    if expected_context.is_some_and(|expected| *expected != context_hash) {
        return Ok(None);
    }
    let mut payload = Vec::new();
    if file.read_to_end(&mut payload).is_err() {
        return Ok(None);
    }
    Ok(Some(ParsedHeader {
        kind,
        context_hash,
        payload,
    }))
}

/// A candidate entry artifact whose header matches everything except the
/// context hash, which the caller decides about.
struct CandidateEntry {
    context_hash: [u8; 32],
    manifest: Option<ContextManifest>,
    chunk: Chunk,
}

fn read_entry_candidate(path: &Path, key: &CacheKey) -> io::Result<Option<CandidateEntry>> {
    let Some(header) = read_header_if_matches(path, key, None)? else {
        return Ok(None);
    };
    if header.kind != KIND_ENTRY_CHUNK {
        return Ok(None);
    }
    let payload: EntryPayload = match deserialize_cache_payload(&header.payload) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    Ok(Some(CandidateEntry {
        context_hash: header.context_hash,
        manifest: payload.manifest,
        chunk: Chunk::from_cached(payload.chunk),
    }))
}

fn read_module_if_matches(
    path: &Path,
    key: &CacheKey,
    source_path: &Path,
) -> io::Result<Option<ModuleArtifact>> {
    let Some(header) = read_header_if_matches(path, key, Some(&key.context_hash))? else {
        return Ok(None);
    };
    if header.kind != KIND_MODULE_ARTIFACT {
        return Ok(None);
    }
    match deserialize_cache_payload::<ModuleArtifact>(&header.payload) {
        Ok(mut artifact) => {
            artifact.bind_source_file(source_path);
            Ok(Some(artifact))
        }
        Err(_) => Ok(None),
    }
}

/// Compact representation of [`CompilerOptions`] for the cache header.
/// Independent flags get distinct bits so adding a new flag never
/// silently changes existing keys when an old binary reads a new
/// artifact — the header check will fail-closed before we get there
/// anyway, but mapping to bits also keeps the tag a stable function
/// of the option set.
fn compiler_options_tag(options: CompilerOptions) -> u8 {
    let mut tag: u8 = 0;
    if options.optimizations_enabled() {
        tag |= 0b0000_0001;
    }
    tag
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Stable digest over every embedded stdlib source. Folded into the
/// user-file cache key so that bumping a stdlib module (changing its
/// embedded `.harn` content) invalidates cached user bytecode that may
/// reference stale function-pool layouts from a prior stdlib snapshot.
/// `HARN_VERSION` already busts the cache across release bumps; this
/// closes the same gap for within-version stdlib edits (a frequent
/// pattern during local development).
///
/// Cached in a `OnceLock` because `STDLIB_SOURCES` is a static `const`
/// slice — the digest is identical for the lifetime of the process.
fn embedded_stdlib_digest() -> &'static [u8; 32] {
    use std::sync::OnceLock;
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut entries: Vec<(&'static str, &'static str)> = harn_stdlib::STDLIB_SOURCES
            .iter()
            .map(|src| (src.module, src.source))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut hasher = Sha256::new();
        for (module, source) in entries {
            hasher.update(module.as_bytes());
            hasher.update(b"\0");
            hasher.update(source.as_bytes());
            hasher.update(b"\0");
        }
        hasher.finalize().into()
    })
}

/// Stable compilation context for a source-local module artifact.
///
/// Module compilation does not inspect user dependencies. Artifact-local
/// compiler and stdlib identity belongs in the key; the source path does not,
/// because it is load context and is rebound after deserialization.
fn module_compilation_context_hash() -> [u8; 32] {
    module_compilation_context_hash_fingerprinted(CODEGEN_FINGERPRINT)
}

fn module_compilation_context_hash_fingerprinted(codegen_fingerprint: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"module-artifact-source-local-v3\0");
    hasher.update(b"stdlib-digest\0");
    hasher.update(embedded_stdlib_digest());
    hasher.update(b"\0codegen-fingerprint\0");
    hasher.update(codegen_fingerprint.as_bytes());
    hasher.finalize().into()
}

// Test seam: how many times the import-graph walk has actually run on this
// thread.
//
// The manifest fast path and the walk agree on results *by construction* —
// both trust the same `(len, mtime_ns)` identity — so no observable output can
// tell them apart. Only the work done differs, and this counts it. Thread-local
// so tests running in parallel cannot perturb each other.
#[cfg(test)]
thread_local! {
    pub(crate) static WALKS_PERFORMED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Walk the user-import graph rooted at `source_path` and produce a
/// stable hash of every transitively-reachable file. The hash is
/// order-independent: each visited file is keyed by canonical path and
/// emitted in sorted order, so reordering imports inside a file does
/// not invalidate the cache while changing any file's content does.
///
/// Embedded stdlib content is folded into the hash too — `collect_user_imports`
/// deliberately skips `std/*` paths (they resolve to in-binary sources, not
/// disk files), so without this fold a stdlib edit between development
/// builds would leave user-file caches pinned to a stale stdlib snapshot.
fn hash_transitive_user_imports(source_path: &Path, source: &str) -> [u8; 32] {
    hash_transitive_user_imports_fingerprinted(source_path, source, CODEGEN_FINGERPRINT).0
}

/// As [`hash_transitive_user_imports`], but also returns the manifest that
/// proves the walk's observations, for callers that will persist it.
fn hash_transitive_user_imports_with_manifest(
    source_path: &Path,
    source: &str,
) -> ([u8; 32], Option<ContextManifest>) {
    hash_transitive_user_imports_fingerprinted(source_path, source, CODEGEN_FINGERPRINT)
}

/// Inner form of [`hash_transitive_user_imports`] parameterized on the compiler
/// fingerprint so tests can vary it; production always passes
/// [`CODEGEN_FINGERPRINT`].
fn hash_transitive_user_imports_fingerprinted(
    source_path: &Path,
    source: &str,
    codegen_fingerprint: &str,
) -> ([u8; 32], Option<ContextManifest>) {
    #[cfg(test)]
    WALKS_PERFORMED.with(|c| c.set(c.get() + 1));

    let mut visited: std::collections::BTreeMap<PathBuf, ImportNode> =
        std::collections::BTreeMap::new();
    let entry = ModuleSource::from_text(source);
    let mut frontier: Vec<(PathBuf, Arc<str>)> = entry
        .imports()
        .iter()
        .map(|import| (source_path.to_path_buf(), Arc::clone(import)))
        .collect();
    // Built alongside the hash: the same observations, in a form a later
    // process can re-check with stats instead of repeating this walk. Anchored
    // at the entry, because that is what the observations are relative to, and
    // stamped before the first file is stat'ed, so entries observed inside a
    // timestamp tick are recognizable as such later. Set to `None` the moment
    // the graph contains something stats cannot describe.
    let mut manifest = Some(ContextManifest::begin(module_source::canonical_identity(
        source_path,
    )));

    while let Some((anchor, import)) = frontier.pop() {
        let Some(resolved) = harn_modules::resolve_import_path(&anchor, &import) else {
            // Unresolved imports get a sentinel keyed by their resolution
            // anchor so that dropping a real file under that anchor later
            // produces a different key.
            let sentinel = anchor.join(format!("__unresolved__/{import}"));
            if let std::collections::btree_map::Entry::Vacant(slot) = visited.entry(sentinel) {
                slot.insert(ImportNode::Unresolved {
                    import: Arc::clone(&import),
                });
                if let Some(m) = manifest.as_mut() {
                    m.unresolved.push(ManifestUnresolved {
                        anchor: anchor.clone(),
                        import: import.to_string(),
                    });
                }
            }
            continue;
        };
        let canonical = module_source::canonical_identity(&resolved);
        if visited.contains_key(&canonical) {
            continue;
        }
        // The read and the import scan are owned by [`module_source`], which
        // memoizes both by the file's stat identity. The same handful of core
        // library modules (`lib/host/*`, `lib/runtime/*`, ...) sit on the import
        // graph of nearly every module, and the VM's module loader reads every
        // one of these files again — so without a shared owner a single spawn
        // re-reads and re-scans the same sources many times over.
        match module_source::read(&resolved) {
            Ok(module) => {
                visited.insert(
                    canonical.clone(),
                    ImportNode::Resolved {
                        content: Arc::clone(module.text()),
                    },
                );
                match ManifestFile::observe(&canonical, &module) {
                    Some(file) => {
                        if let Some(m) = manifest.as_mut() {
                            m.files.push(file);
                        }
                    }
                    // Read succeeded but the file cannot be stat'ed now. Rather
                    // than record a fact we could not re-check, drop the
                    // manifest and leave this graph on the walk.
                    None => manifest = None,
                }
                for nested_import in module.imports() {
                    frontier.push((resolved.clone(), Arc::clone(nested_import)));
                }
            }
            Err(error) => {
                let unreadable_path = canonical.clone();
                visited.insert(
                    canonical,
                    ImportNode::IoError {
                        kind: error.kind().to_string(),
                    },
                );
                // Real trees contain these — an `import "./types"` where
                // `types/` is a directory resolves, then fails to read. Dropping
                // the manifest for them would silently disable the fast path on
                // exactly the graphs it exists for.
                if let Some(m) = manifest.as_mut() {
                    m.unreadable.push(ManifestUnreadable {
                        path: unreadable_path,
                        kind: error.kind().to_string(),
                    });
                }
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"stdlib-digest\0");
    hasher.update(embedded_stdlib_digest());
    hasher.update(b"\0");
    // Fold in the compiler's code-generation fingerprint so a compiler change
    // that alters emitted bytecode for unchanged source busts stale cache
    // entries within a single version — the gap that masked the #2610 fix until
    // the cache was cleared by hand. See `build.rs` and `CODEGEN_FINGERPRINT`.
    hasher.update(b"codegen-fingerprint\0");
    hasher.update(codegen_fingerprint.as_bytes());
    hasher.update(b"\0");
    for (path, node) in &visited {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        match node {
            ImportNode::Resolved { content } => {
                hasher.update(b"resolved\0");
                hasher.update(content.as_bytes());
            }
            ImportNode::Unresolved { import } => {
                hasher.update(b"unresolved\0");
                hasher.update(import.as_bytes());
            }
            ImportNode::IoError { kind } => {
                hasher.update(b"ioerror\0");
                hasher.update(kind.as_bytes());
            }
        }
        hasher.update(b"\0");
    }
    // Sorted so one graph always serializes to one byte sequence, whatever
    // order the frontier happened to pop.
    if let Some(m) = manifest.as_mut() {
        m.files.sort_by(|a, b| a.path.cmp(&b.path));
        m.unresolved
            .sort_by(|a, b| (&a.anchor, &a.import).cmp(&(&b.anchor, &b.import)));
        m.unreadable.sort_by(|a, b| a.path.cmp(&b.path));
    }
    (hasher.finalize().into(), manifest)
}

enum ImportNode {
    Resolved { content: Arc<str> },
    Unresolved { import: Arc<str> },
    IoError { kind: String },
}

#[cfg(test)]
#[path = "bytecode_cache_tests.rs"]
mod tests;
