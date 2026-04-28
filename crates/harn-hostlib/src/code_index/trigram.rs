//! Trigram index over file contents.
//!
//! Substring-accelerated full-text index: every 3-byte sliding window in a
//! file becomes a posting in `index[trigram] -> Set<FileId>`. A query is
//! decomposed into its trigrams; the candidate set is the intersection of
//! those posting lists. The wire-stable algorithm ASCII case-folds each
//! byte (`A-Z` -> `a-z`), leaves non-ASCII bytes unchanged, and packs each
//! 3-byte sliding window as `(a << 16) | (b << 8) | c` into a `u32`.

use std::collections::{HashMap, HashSet};

use super::file_table::FileId;

/// Packed 3-byte sliding-window key. Layout: `(a << 16) | (b << 8) | c`,
/// case-folded on the ASCII range so lookups are case-insensitive.
pub type Trigram = u32;

/// Trigram posting list: `trigram -> set of file ids that contain it`,
/// plus a per-file reverse map for cheap re-indexing.
#[derive(Debug, Default, Clone)]
pub struct TrigramIndex {
    index: HashMap<Trigram, HashSet<FileId>>,
    file_trigrams: HashMap<FileId, HashSet<Trigram>>,
}

impl TrigramIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the postings for `id` with the trigrams of `content`.
    pub fn index_file(&mut self, id: FileId, content: &str) {
        self.remove_file(id);
        let trigrams = extract_trigrams(content);
        if trigrams.is_empty() {
            return;
        }
        for tg in &trigrams {
            self.index.entry(*tg).or_default().insert(id);
        }
        self.file_trigrams.insert(id, trigrams);
    }

    /// Drop every posting contributed by `id`.
    pub fn remove_file(&mut self, id: FileId) {
        let Some(trigrams) = self.file_trigrams.remove(&id) else {
            return;
        };
        for tg in trigrams {
            if let Some(set) = self.index.get_mut(&tg) {
                set.remove(&id);
                if set.is_empty() {
                    self.index.remove(&tg);
                }
            }
        }
    }

    /// Return file ids that contain ALL of `trigrams`. Empty input or any
    /// missing trigram yields an empty set.
    pub fn query(&self, trigrams: &[Trigram]) -> HashSet<FileId> {
        if trigrams.is_empty() {
            return HashSet::new();
        }
        let mut postings: Vec<&HashSet<FileId>> = Vec::with_capacity(trigrams.len());
        for tg in trigrams {
            match self.index.get(tg) {
                Some(set) => postings.push(set),
                None => return HashSet::new(),
            }
        }
        postings.sort_by_key(|s| s.len());
        let (head, tail) = postings.split_first().expect("non-empty by construction");
        let mut acc: HashSet<FileId> = (*head).clone();
        for set in tail {
            acc.retain(|id| set.contains(id));
            if acc.is_empty() {
                return acc;
            }
        }
        acc
    }

    /// Number of distinct trigrams in the posting table.
    pub fn distinct_trigrams(&self) -> usize {
        self.index.len()
    }

    /// Capture the posting table as a snapshot-friendly vector. Used by
    /// the on-disk snapshot path; sorted by trigram for deterministic
    /// serialisation.
    pub fn snapshot_postings(&self) -> Vec<super::snapshot::TrigramPosting> {
        let mut out: Vec<super::snapshot::TrigramPosting> = self
            .index
            .iter()
            .map(|(tg, files)| {
                let mut files: Vec<FileId> = files.iter().copied().collect();
                files.sort_unstable();
                super::snapshot::TrigramPosting {
                    trigram: *tg,
                    files,
                }
            })
            .collect();
        out.sort_unstable_by_key(|p| p.trigram);
        out
    }

    /// Rebuild a [`TrigramIndex`] from a snapshot's postings vector.
    pub fn from_postings(postings: Vec<super::snapshot::TrigramPosting>) -> Self {
        let mut idx = Self::new();
        for p in postings {
            let entry = idx.index.entry(p.trigram).or_default();
            for f in &p.files {
                entry.insert(*f);
                idx.file_trigrams.entry(*f).or_default().insert(p.trigram);
            }
        }
        idx
    }

    /// Order-of-magnitude resident-bytes estimate. Reported by
    /// `code_index.stats.memory_bytes`.
    pub fn estimated_bytes(&self) -> usize {
        // Cheap order-of-magnitude estimate: 4 bytes per posting key plus
        // ~4 bytes per FileID per posting on both sides.
        let postings: usize = self.index.values().map(|s| s.len()).sum();
        let reverse: usize = self.file_trigrams.values().map(|s| s.len()).sum();
        self.index.len() * 4 + postings * 4 + reverse * 4 + self.file_trigrams.len() * 4
    }
}

/// Extract every distinct trigram from `text`. Bytes are lowercased on the
/// ASCII range; non-ASCII bytes pass through unchanged. Sliding 3-byte
/// window over the UTF-8 bytes.
pub fn extract_trigrams(text: &str) -> HashSet<Trigram> {
    let bytes = text.as_bytes();
    if bytes.len() < 3 {
        return HashSet::new();
    }
    let mut out: HashSet<Trigram> = HashSet::with_capacity(bytes.len().max(8) / 4);
    for window in bytes.windows(3) {
        out.insert(pack(
            normalize(window[0]),
            normalize(window[1]),
            normalize(window[2]),
        ));
    }
    out
}

/// Decompose a query into its constituent trigrams (deduped) so callers
/// that pre-compute on the script side stay in lock-step with the host.
pub fn query_trigrams(query: &str) -> Vec<Trigram> {
    extract_trigrams(query).into_iter().collect()
}

/// Pack three normalised bytes into a `Trigram` using the canonical
/// `(a << 16) | (b << 8) | c` layout.
#[inline(always)]
pub fn pack(a: u8, b: u8, c: u8) -> Trigram {
    (a as u32) << 16 | (b as u32) << 8 | c as u32
}

#[inline(always)]
fn normalize(byte: u8) -> u8 {
    if byte.is_ascii_uppercase() {
        byte + 0x20
    } else {
        byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_handles_short_strings() {
        assert!(extract_trigrams("").is_empty());
        assert!(extract_trigrams("ab").is_empty());
        assert_eq!(extract_trigrams("abc").len(), 1);
    }

    #[test]
    fn extract_is_case_insensitive() {
        let lower = extract_trigrams("foo");
        let upper = extract_trigrams("FOO");
        assert_eq!(lower, upper);
    }

    #[test]
    fn query_intersects_postings() {
        let mut idx = TrigramIndex::new();
        idx.index_file(1, "the quick brown fox");
        idx.index_file(2, "jumped over the lazy dog");
        idx.index_file(3, "lorem ipsum dolor sit amet");

        let needle = query_trigrams("the");
        let hits = idx.query(&needle);
        assert!(hits.contains(&1));
        assert!(hits.contains(&2));
        assert!(!hits.contains(&3));
    }

    #[test]
    fn missing_trigram_short_circuits_to_empty() {
        let mut idx = TrigramIndex::new();
        idx.index_file(1, "hello world");
        let needle = query_trigrams("zzzzzz");
        assert!(idx.query(&needle).is_empty());
    }

    #[test]
    fn remove_file_drops_postings() {
        let mut idx = TrigramIndex::new();
        idx.index_file(1, "alpha beta");
        idx.index_file(2, "alpha gamma");
        idx.remove_file(1);
        let needle = query_trigrams("alp");
        let hits = idx.query(&needle);
        assert!(!hits.contains(&1));
        assert!(hits.contains(&2));
    }

    #[test]
    fn reindex_replaces_postings() {
        let mut idx = TrigramIndex::new();
        idx.index_file(1, "alpha beta");
        idx.index_file(1, "gamma delta");
        let alpha = query_trigrams("alp");
        let gamma = query_trigrams("gam");
        assert!(idx.query(&alpha).is_empty());
        assert!(idx.query(&gamma).contains(&1));
    }
}
