//! Content-addressed on-disk cache for compiled `.harn` pipelines.
//!
//! Cold-start `harn run` re-parses, type-checks, and compiles the entry
//! pipeline before the VM gets a single instruction to execute. For short
//! Harn subcommands that wrap a few `llm_call`s in a small pipeline, that
//! compile cost dominates wall-clock time.
//!
//! This module persists [`Chunk`] bytecode under
//! `$HARN_CACHE_DIR/<source-hash>.harnbc` (XDG-aware). The cache key is
//! derived from the entry source plus the content hash of every
//! transitively-imported user file; stdlib imports are covered by the
//! embedded `harn_version` field in the header. Any change to any input
//! flips the key and the next run recompiles.
//!
//! File layout — little-endian throughout:
//!
//! ```text
//! magic        : [u8; 8]   = "HARNBC\0\0"
//! schema_ver   : u32       = SCHEMA_VERSION
//! version_len  : u32
//! harn_version : [u8; version_len]
//! compiler_tag : u8        bitmask of active CompilerOptions
//! kind         : u8        1 = entry chunk, 2 = module artifact
//! source_hash  : [u8; 32]
//! import_hash  : [u8; 32]
//! payload      : bincode-serialized payload for `kind`
//! ```
//!
//! The header lets a stale binary detect a future-version artifact
//! without crashing: a magic mismatch, schema mismatch, or version
//! mismatch is returned as `Ok(None)` so the caller transparently
//! recompiles. Real I/O errors propagate.
//!
//! Concurrency: writes are atomic (write-tmp-then-rename), and parallel
//! invocations on a cache miss race safely — the last writer wins, but
//! every reader observes a consistent file because the rename is atomic
//! on every supported filesystem.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::chunk::{CachedChunk, Chunk};
use crate::compiler::CompilerOptions;
use crate::module_artifact::ModuleArtifact;

/// Header magic for all bytecode-cache artifact families.
pub const MAGIC: &[u8; 8] = b"HARNBC\0\0";

/// On-disk format version. Bump when [`CachedChunk`] or the header
/// layout changes in a backwards-incompatible way.
pub const SCHEMA_VERSION: u32 = 3;

/// Compile-time Harn release. Cache files written by a different release
/// are rejected on load.
pub const HARN_VERSION: &str = env!("CARGO_PKG_VERSION");

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
}

/// Cache key components for a single pipeline source. Equality of all
/// fields is necessary and sufficient for cache reuse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub source_hash: [u8; 32],
    pub import_graph_hash: [u8; 32],
    pub harn_version: &'static str,
    /// Compact tag for active [`CompilerOptions`]. Flipping
    /// `HARN_DISABLE_OPTIMIZATIONS` between runs would otherwise reuse a
    /// chunk compiled under the wrong setting.
    pub compiler_tag: u8,
}

impl CacheKey {
    /// Compute the cache key for a `.harn` source file plus its transitive
    /// user imports. `read_source` is the entry-file contents; the import
    /// graph is walked from disk relative to `source_path`.
    pub fn from_source(source_path: &Path, source: &str) -> Self {
        let source_hash = sha256(source.as_bytes());
        let import_graph_hash = hash_transitive_user_imports(source_path, source);
        Self {
            source_hash,
            import_graph_hash,
            harn_version: HARN_VERSION,
            compiler_tag: compiler_options_tag(CompilerOptions::from_env()),
        }
    }

    /// Entry-chunk filename for this key. We hash by source content
    /// alone so two invocations of the same source from different paths
    /// share a cache entry; the header's import-graph hash still gates
    /// reuse on a per-load basis.
    pub fn filename(&self) -> String {
        format!("{}.{}", hex(&self.source_hash), CACHE_EXTENSION)
    }

    /// Module-artifact filename for this key.
    pub fn module_filename(&self) -> String {
        format!("{}.{}", hex(&self.source_hash), MODULE_CACHE_EXTENSION)
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
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("harn")
            .join("bytecode");
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
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("harn")
            .join("packs");
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
    let key = CacheKey::from_source(source_path, source);
    if !cache_enabled() {
        return LookupOutcome { key, chunk: None };
    }
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(2);
    if let Some(adjacent) = adjacent_cache_path(source_path) {
        candidates.push(adjacent);
    }
    candidates.push(cache_dir().join(key.filename()));
    for path in candidates {
        match read_chunk_if_matches(&path, &key) {
            Ok(Some(chunk)) => {
                return LookupOutcome {
                    key,
                    chunk: Some(chunk),
                }
            }
            Ok(None) => continue,
            Err(_) => continue,
        }
    }
    LookupOutcome { key, chunk: None }
}

/// Persist `chunk` to the shared cache directory under `key`. Atomic: a
/// temp file is written then renamed into place. Concurrent invocations
/// on the same key race safely.
pub fn store(key: &CacheKey, chunk: &Chunk) -> io::Result<()> {
    if !cache_enabled() {
        return Ok(());
    }
    let dir = cache_dir();
    fs::create_dir_all(&dir)?;
    write_atomic_chunk(&dir.join(key.filename()), key, chunk)
}

/// Write a precompiled entry-chunk artifact to an explicit path, for
/// use by the `harn precompile` subcommand. The header still records
/// the key, so adjacent artifacts shipped with source are validated
/// like any other cache hit.
pub fn store_at(path: &Path, key: &CacheKey, chunk: &Chunk) -> io::Result<()> {
    ensure_parent_dir(path)?;
    write_atomic_chunk(path, key, chunk)
}

/// Look up the [`ModuleArtifact`] for `source_path` (whose contents are
/// `source`). Mirrors [`load`] but for the `.harnmod` family.
pub fn load_module(source_path: &Path, source: &str) -> ModuleLookupOutcome {
    let key = CacheKey::from_source(source_path, source);
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
        match read_module_if_matches(&path, &key) {
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

fn write_atomic_chunk(target: &Path, key: &CacheKey, chunk: &Chunk) -> io::Result<()> {
    let buf = serialize_chunk_artifact(key, chunk)?;
    write_atomic(target, &buf)
}

fn write_atomic_module(target: &Path, key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<()> {
    let buf = serialize_module_artifact(key, artifact)?;
    write_atomic(target, &buf)
}

/// Serialize an entry-chunk artifact (header + payload) to bytes. The
/// resulting buffer is byte-identical to the file [`store_at`] would
/// have written for the same `(key, chunk)`. Use this when packaging
/// artifacts into a container (e.g. `harn pack`) without going through
/// the filesystem.
pub fn serialize_chunk_artifact(key: &CacheKey, chunk: &Chunk) -> io::Result<Vec<u8>> {
    let cached = chunk.freeze_for_cache();
    let payload = bincode::serialize(&cached)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(encode_artifact(key, KIND_ENTRY_CHUNK, &payload))
}

/// Serialize a module artifact (header + payload) to bytes. Companion
/// to [`serialize_chunk_artifact`] for the `.harnmod` family.
pub fn serialize_module_artifact(key: &CacheKey, artifact: &ModuleArtifact) -> io::Result<Vec<u8>> {
    let payload = bincode::serialize(artifact)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(encode_artifact(key, KIND_MODULE_ARTIFACT, &payload))
}

fn encode_artifact(key: &CacheKey, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::with_capacity(payload.len() + 128);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    let version_bytes = HARN_VERSION.as_bytes();
    buf.extend_from_slice(&(version_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(version_bytes);
    buf.push(key.compiler_tag);
    buf.push(kind);
    buf.extend_from_slice(&key.source_hash);
    buf.extend_from_slice(&key.import_graph_hash);
    buf.extend_from_slice(payload);
    buf
}

fn write_atomic(target: &Path, buf: &[u8]) -> io::Result<()> {
    let tmp_name = match target.file_name() {
        Some(name) => format!(".{}.{}.tmp", name.to_string_lossy(), std::process::id(),),
        None => format!(".harn-cache.{}.tmp", std::process::id()),
    };
    let tmp_path = target.with_file_name(tmp_name);
    let mut tmp_file = fs::File::create(&tmp_path)?;
    tmp_file.write_all(buf)?;
    tmp_file.sync_all()?;
    drop(tmp_file);
    match fs::rename(&tmp_path, target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(err)
        }
    }
}

/// Parsed cache header. Read by both the chunk and module loaders so the
/// header-validation logic stays in one place.
struct ParsedHeader {
    kind: u8,
    payload: Vec<u8>,
}

fn read_header_if_matches(path: &Path, key: &CacheKey) -> io::Result<Option<ParsedHeader>> {
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
    if version_len > 256 {
        // Bound the alloc so a corrupted file cannot force an unbounded read.
        return Ok(None);
    }
    let mut version_buf = vec![0u8; version_len];
    if file.read_exact(&mut version_buf).is_err() {
        return Ok(None);
    }
    if version_buf != key.harn_version.as_bytes() {
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
    if hashes[..32] != key.source_hash || hashes[32..] != key.import_graph_hash {
        return Ok(None);
    }
    let mut payload = Vec::new();
    if file.read_to_end(&mut payload).is_err() {
        return Ok(None);
    }
    Ok(Some(ParsedHeader { kind, payload }))
}

fn read_chunk_if_matches(path: &Path, key: &CacheKey) -> io::Result<Option<Chunk>> {
    let Some(header) = read_header_if_matches(path, key)? else {
        return Ok(None);
    };
    if header.kind != KIND_ENTRY_CHUNK {
        return Ok(None);
    }
    let cached: CachedChunk = match bincode::deserialize(&header.payload) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    Ok(Some(Chunk::from_cached(&cached)))
}

fn read_module_if_matches(path: &Path, key: &CacheKey) -> io::Result<Option<ModuleArtifact>> {
    let Some(header) = read_header_if_matches(path, key)? else {
        return Ok(None);
    };
    if header.kind != KIND_MODULE_ARTIFACT {
        return Ok(None);
    }
    match bincode::deserialize::<ModuleArtifact>(&header.payload) {
        Ok(artifact) => Ok(Some(artifact)),
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

/// Lightweight regex-free scan that surfaces user imports without paying
/// a full lex+parse. False positives only increase cache churn, never
/// correctness; comments and string literals are skipped so neither a
/// commented-out import nor a `"import …"` value appearing inside an
/// unrelated string gates the hash.
fn collect_user_imports(source: &str) -> Vec<String> {
    let scrubbed = strip_comments(source);
    let mut out: Vec<String> = Vec::new();
    let bytes = scrubbed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Skip past any string literal so identifiers inside string
            // values cannot trigger the keyword match below.
            match read_string_literal(bytes, i) {
                Some((_, end)) => {
                    i = end;
                    continue;
                }
                None => {
                    i += 1;
                    continue;
                }
            }
        }
        if !matches_keyword(bytes, i, b"import") {
            i += 1;
            continue;
        }
        // Skip past `import` and any selective `{ ... } from` clause; we
        // only need the source-position of the path string literal.
        let mut j = i + b"import".len();
        let mut depth = 0i32;
        while j < bytes.len() {
            match bytes[j] {
                b'"' => {
                    if let Some((path, end)) = read_string_literal(bytes, j) {
                        if !path.starts_with("std/") {
                            out.push(path);
                        }
                        i = end;
                        break;
                    }
                    j += 1;
                }
                b'{' => {
                    depth += 1;
                    j += 1;
                }
                b'}' => {
                    depth -= 1;
                    j += 1;
                }
                b'\n' if depth == 0 => {
                    // No string literal on this logical line; bail and
                    // continue scanning after the keyword to avoid an
                    // infinite loop.
                    i = j;
                    break;
                }
                _ => j += 1,
            }
        }
        if j >= bytes.len() {
            break;
        }
        if i < j {
            // Defensive: ensure forward progress when the inner loop
            // exited without setting `i`.
            i = j;
        }
    }
    out
}

fn matches_keyword(bytes: &[u8], at: usize, keyword: &[u8]) -> bool {
    let end = at + keyword.len();
    if end > bytes.len() {
        return false;
    }
    if &bytes[at..end] != keyword {
        return false;
    }
    if at > 0 && is_ident_char(bytes[at - 1]) {
        return false;
    }
    if end < bytes.len() && is_ident_char(bytes[end]) {
        return false;
    }
    true
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn read_string_literal(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes[at], b'"');
    let mut out = String::new();
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                if i + 1 >= bytes.len() {
                    return None;
                }
                match bytes[i + 1] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    other => out.push(other as char),
                }
                i += 2;
            }
            b'\n' => return None,
            byte => {
                out.push(byte as char);
                i += 1;
            }
        }
    }
    None
}

fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'"' {
            if let Some((_, end)) = read_string_literal(bytes, i) {
                out.push_str(&source[i..end]);
                i = end;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Walk the user-import graph rooted at `source_path` and produce a
/// stable hash of every transitively-reachable file. The hash is
/// order-independent: each visited file is keyed by canonical path and
/// emitted in sorted order, so reordering imports inside a file does
/// not invalidate the cache while changing any file's content does.
fn hash_transitive_user_imports(source_path: &Path, source: &str) -> [u8; 32] {
    let mut visited: std::collections::BTreeMap<PathBuf, ImportNode> =
        std::collections::BTreeMap::new();
    let mut frontier: Vec<(PathBuf, String)> = collect_user_imports(source)
        .into_iter()
        .map(|import| (source_path.to_path_buf(), import))
        .collect();

    while let Some((anchor, import)) = frontier.pop() {
        let Some(resolved) = harn_modules::resolve_import_path(&anchor, &import) else {
            // Unresolved imports get a sentinel keyed by their resolution
            // anchor so that dropping a real file under that anchor later
            // produces a different key.
            let sentinel = anchor.join(format!("__unresolved__/{import}"));
            visited
                .entry(sentinel)
                .or_insert(ImportNode::Unresolved { import });
            continue;
        };
        let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
        if visited.contains_key(&canonical) {
            continue;
        }
        match fs::read_to_string(&resolved) {
            Ok(content) => {
                let nested = collect_user_imports(&content);
                visited.insert(
                    canonical.clone(),
                    ImportNode::Resolved {
                        content: content.clone(),
                    },
                );
                for nested_import in nested {
                    frontier.push((resolved.clone(), nested_import));
                }
            }
            Err(err) => {
                visited.insert(
                    canonical,
                    ImportNode::IoError {
                        kind: err.kind().to_string(),
                    },
                );
            }
        }
    }

    let mut hasher = Sha256::new();
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
    hasher.finalize().into()
}

enum ImportNode {
    Resolved { content: String },
    Unresolved { import: String },
    IoError { kind: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_source;

    #[test]
    fn header_round_trips_chunk() {
        let chunk = compile_source("__io_println(\"hello\")").expect("compile");
        let key = CacheKey::from_source(Path::new("/tmp/example.harn"), "__io_println(\"hello\")");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("entry.harnbc");
        store_at(&path, &key, &chunk).expect("write");
        let loaded = read_chunk_if_matches(&path, &key).unwrap();
        assert!(loaded.is_some(), "expected cached chunk to load");
    }

    #[test]
    fn serialize_chunk_artifact_matches_store_at() {
        // `serialize_chunk_artifact` packages an artifact into a buffer for
        // in-memory consumers (e.g. `harn pack` writing into a tar.zst
        // bundle). The contract is: the resulting bytes match what
        // `store_at` would have written for the same key+chunk, so the
        // shipped artifact is byte-identical to the on-disk cache form.
        let chunk = compile_source("__io_println(\"hi\")").expect("compile");
        let key = CacheKey::from_source(Path::new("/tmp/pack.harn"), "__io_println(\"hi\")");
        let tmp = tempfile::tempdir().unwrap();
        let on_disk = tmp.path().join("pack.harnbc");
        store_at(&on_disk, &key, &chunk).expect("write");
        let on_disk_bytes = std::fs::read(&on_disk).unwrap();
        let in_memory_bytes = serialize_chunk_artifact(&key, &chunk).expect("serialize");
        assert_eq!(in_memory_bytes, on_disk_bytes);
    }

    #[test]
    fn header_mismatch_returns_none() {
        let chunk = compile_source("1 + 1").expect("compile");
        let key = CacheKey::from_source(Path::new("/tmp/a.harn"), "1 + 1");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.harnbc");
        store_at(&path, &key, &chunk).expect("write");
        let other = CacheKey {
            source_hash: [0xAB; 32],
            import_graph_hash: key.import_graph_hash,
            harn_version: HARN_VERSION,
            compiler_tag: key.compiler_tag,
        };
        assert!(read_chunk_if_matches(&path, &other).unwrap().is_none());
    }

    #[test]
    fn compiler_tag_mismatch_returns_none() {
        let chunk = compile_source("1 + 1").expect("compile");
        let key = CacheKey::from_source(Path::new("/tmp/b.harn"), "1 + 1");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("b.harnbc");
        store_at(&path, &key, &chunk).expect("write");
        let other = CacheKey {
            compiler_tag: key.compiler_tag ^ 0xFF,
            ..key.clone()
        };
        assert!(
            read_chunk_if_matches(&path, &other).unwrap().is_none(),
            "flipped HARN_DISABLE_OPTIMIZATIONS must not reuse a chunk \
             compiled under the opposite setting"
        );
    }

    #[test]
    fn collect_user_imports_ignores_stdlib_and_comments() {
        let source = r#"
            // import "comment/should/be/ignored"
            import "std/agents"
            import { foo } from "pkg/bar"
            import "./relative/path"
        "#;
        let imports = collect_user_imports(source);
        assert_eq!(
            imports,
            vec!["pkg/bar".to_string(), "./relative/path".to_string()]
        );
    }

    #[test]
    fn cache_enabled_respects_env() {
        std::env::set_var(CACHE_ENABLED_ENV, "0");
        assert!(!cache_enabled());
        std::env::set_var(CACHE_ENABLED_ENV, "1");
        assert!(cache_enabled());
        std::env::remove_var(CACHE_ENABLED_ENV);
        assert!(cache_enabled());
    }

    #[test]
    fn import_path_inside_string_literal_is_ignored() {
        let source = r#"
            let payload = "import { foo } from \"./other\""
            import "./real"
        "#;
        let imports = collect_user_imports(source);
        assert_eq!(imports, vec!["./real".to_string()]);
    }

    #[test]
    fn import_hash_is_stable_across_import_order() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.harn"),
            "pub fn a() -> int { return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b.harn"),
            "pub fn b() -> int { return 2 }\n",
        )
        .unwrap();
        let ab = tmp.path().join("entry_ab.harn");
        std::fs::write(
            &ab,
            "import \"./a\"\nimport \"./b\"\n__io_println(\"hi\")\n",
        )
        .unwrap();
        let ba = tmp.path().join("entry_ba.harn");
        std::fs::write(
            &ba,
            "import \"./b\"\nimport \"./a\"\n__io_println(\"hi\")\n",
        )
        .unwrap();
        let hash_ab = hash_transitive_user_imports(&ab, &std::fs::read_to_string(&ab).unwrap());
        let hash_ba = hash_transitive_user_imports(&ba, &std::fs::read_to_string(&ba).unwrap());
        assert_eq!(
            hash_ab, hash_ba,
            "import-graph hash must be order-independent so reordering imports \
             does not bust the cache"
        );
    }

    #[test]
    fn import_hash_picks_up_nested_imports() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("leaf.harn"),
            "pub fn x() -> int { return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("mid.harn"),
            "import \"./leaf\"\npub fn y() -> int { return 2 }\n",
        )
        .unwrap();
        let entry = tmp.path().join("entry.harn");
        std::fs::write(&entry, "import \"./mid\"\n__io_println(\"hi\")\n").unwrap();

        let before =
            hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
        std::fs::write(
            tmp.path().join("leaf.harn"),
            "pub fn x() -> int { return 999 }\n",
        )
        .unwrap();
        let after = hash_transitive_user_imports(&entry, &std::fs::read_to_string(&entry).unwrap());
        assert_ne!(
            before, after,
            "editing a transitively-imported file must change the import-graph hash"
        );
    }
}
