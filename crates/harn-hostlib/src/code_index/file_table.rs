//! Per-file metadata.
//!
//! `IndexedFile` holds the structural data the index needs at query time —
//! language, size, content hash, raw import strings, and the list of
//! outline symbols. `FileId` is a monotonically-assigned `u32` that all
//! sub-indexes (`TrigramIndex`, `WordIndex`, `DepGraph`, `VersionLog`) key
//! on so re-indexing a path doesn't have to invalidate string keys.

/// Monotonically-assigned identifier for a file in the index. Stable
/// across re-indexes of the same path so sub-indexes can key on `FileId`
/// without invalidating string keys.
pub type FileId = u32;

/// Outline-style symbol entry. Reserved for AST integration; the code-index
/// importer leaves `IndexedFile::symbols` empty, but the shape is kept stable
/// so storage upgrades won't have to re-key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedSymbol {
    /// Symbol name (e.g. `"helper"`).
    pub name: String,
    /// Language-specific kind (`"function"`, `"struct"`, …).
    pub kind: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based inclusive end line.
    pub end_line: u32,
    /// Single-line signature/preview of the declaration.
    pub signature: String,
}

/// Per-file metadata persisted in the index.
#[derive(Debug, Clone)]
pub struct IndexedFile {
    /// Stable file identifier.
    pub id: FileId,
    /// Workspace-relative path with `/` separators. The empty string is
    /// reserved for the root and never appears in the table.
    pub relative_path: String,
    /// Best-effort language tag (e.g. `"rust"`, `"swift"`, `"python"`). For
    /// unrecognised extensions this is the extension itself.
    pub language: String,
    /// File size in bytes (UTF-8 contents).
    pub size_bytes: u64,
    /// Newline-delimited line count.
    pub line_count: u32,
    /// FNV-1a 64-bit content hash, used for cheap change detection.
    pub content_hash: u64,
    /// Last-modified time in milliseconds since the Unix epoch.
    pub mtime_ms: i64,
    /// Outline symbols supplied by callers that have richer syntax context.
    pub symbols: Vec<IndexedSymbol>,
    /// Raw import statement strings extracted from the file.
    pub imports: Vec<String>,
}

/// Stable 64-bit FNV-1a hash used for content-change detection and
/// snapshot compatibility.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_matches_swift_reference() {
        // FNV-1a 64-bit is deterministic; this guards against accidental
        // changes (e.g. switching to a different seed/prime) that would
        // silently break shared snapshot interop.
        assert_eq!(fnv1a64(b"hello world"), 0x779a_65e7_023c_d2e7);
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    }
}
