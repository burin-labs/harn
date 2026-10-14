//! The artifact header check every cache read goes through, over any byte
//! source: a cache file, an adjacent artifact, or a table embedded in the
//! binary. One owner decides whether bytes describe a key.

use std::io;

use super::{provenance_tag, CacheKey, CODEGEN_FINGERPRINT, MAGIC, SCHEMA_VERSION};

/// Reads `len` bytes and reports whether they equal `expected`.
///
/// `len` comes off disk, so it is bounded before it becomes an allocation: a
/// corrupted or hostile file must not be able to ask for an unbounded read.
/// A length that cannot match `expected` is rejected without reading at all.
fn read_length_prefixed_match(file: &mut impl io::Read, len: usize, expected: &[u8]) -> bool {
    if len > 256 || len != expected.len() {
        return false;
    }
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).is_ok() && buf == expected
}

/// Parsed cache header. Read by both the chunk and module loaders so the
/// header-validation logic stays in one place.
pub(super) struct ParsedHeader {
    pub(super) kind: u8,
    pub(super) context_hash: [u8; 32],
    pub(super) payload: Vec<u8>,
}

/// The header check of `read_header_if_matches` over any byte source, so an
/// artifact embedded in the binary is judged by exactly the rules a file is.
pub(super) fn read_header_from(
    mut file: impl io::Read,
    key: &CacheKey,
    expected_context: Option<&[u8; 32]>,
) -> Option<ParsedHeader> {
    let mut header = [0u8; 8 + 4 + 4];
    if file.read_exact(&mut header).is_err() {
        return None;
    }
    if &header[..8] != MAGIC {
        return None;
    }
    let schema = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if schema != SCHEMA_VERSION {
        return None;
    }
    let version_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    if !read_length_prefixed_match(&mut file, version_len, key.harn_version.as_bytes()) {
        return None;
    }
    // Which build produced this artifact, checkable without computing anything.
    // The entry fast path proves its graph unchanged from a manifest and never
    // recomputes the context hash, so a fingerprint carried only inside that
    // hash would go unexamined and a chunk from a previous build of the same
    // release would be replayed. See #5610.
    let mut fingerprint_len_bytes = [0u8; 4];
    if file.read_exact(&mut fingerprint_len_bytes).is_err() {
        return None;
    }
    let fingerprint_len = u32::from_le_bytes(fingerprint_len_bytes) as usize;
    if !read_length_prefixed_match(&mut file, fingerprint_len, CODEGEN_FINGERPRINT.as_bytes()) {
        return None;
    }
    let mut compiler_kind_provenance = [0u8; 3];
    if file.read_exact(&mut compiler_kind_provenance).is_err() {
        return None;
    }
    if compiler_kind_provenance[0] != key.compiler_tag {
        return None;
    }
    let kind = compiler_kind_provenance[1];
    // The authority assertion, and the reason it is a header field rather than
    // only a key field. Shared-cache entries are found by a key-derived
    // filename, so the key alone separates them. An ADJACENT artifact is found
    // by path: `dep.harnmod` beside `dep.harn` is offered to whoever imports
    // that file. Without this comparison an ordinary import accepts an
    // artifact compiled under a privileged authority, because every other
    // header field is identical for the same source and context.
    if compiler_kind_provenance[2] != provenance_tag(key.provenance) {
        return None;
    }
    let mut hashes = [0u8; 64];
    if file.read_exact(&mut hashes).is_err() {
        return None;
    }
    if hashes[..32] != key.source_hash {
        return None;
    }
    let mut context_hash = [0u8; 32];
    context_hash.copy_from_slice(&hashes[32..]);
    if expected_context.is_some_and(|expected| *expected != context_hash) {
        return None;
    }
    let mut payload = Vec::new();
    if file.read_to_end(&mut payload).is_err() {
        return None;
    }
    Some(ParsedHeader {
        kind,
        context_hash,
        payload,
    })
}
