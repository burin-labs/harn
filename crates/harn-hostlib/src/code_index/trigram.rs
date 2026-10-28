//! Trigram index over file contents.
//!
//! Substring-accelerated full-text index: every 3-byte sliding window in a
//! file becomes a posting in `index[trigram] -> Set<FileId>`. A query is
//! decomposed into its trigrams; the candidate set is the intersection of
//! those posting lists. The wire-stable algorithm ASCII case-folds each
//! byte (`A-Z` -> `a-z`), leaves non-ASCII bytes unchanged, and packs each
//! 3-byte sliding window as `(a << 16) | (b << 8) | c` into a `u32`.

use std::borrow::Cow;
use std::collections::HashSet;

use super::file_table::FileId;

/// Packed 3-byte sliding-window key. Layout: `(a << 16) | (b << 8) | c`,
/// case-folded on the ASCII range so lookups are case-insensitive.
pub type Trigram = u32;

/// Trigram posting list: `trigram -> set of file ids that contain it`.
/// See [`super::postings`] for the base, tombstone, and delta layers.
#[derive(Debug, Default, Clone)]
pub struct TrigramIndex {
    pub(super) postings: super::postings::FilePostings<Trigram, FileId>,
}

impl TrigramIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the postings for `id` with the trigrams of `content`.
    pub fn index_file(&mut self, id: FileId, content: &str) {
        self.install_file(id, extract_trigrams(content));
    }

    /// Replace the postings for `id` with already-extracted distinct
    /// `trigrams`.
    pub(super) fn install_file(&mut self, id: FileId, trigrams: impl IntoIterator<Item = Trigram>) {
        self.postings
            .replace_file(id, trigrams.into_iter().map(|tg| (tg, std::iter::once(id))));
    }

    /// Build from each file's distinct trigrams, in strictly ascending
    /// file id order. Answers exactly as [`Self::index_file`] per file would.
    pub(super) fn from_files<T>(files: impl IntoIterator<Item = (FileId, T)>) -> Self
    where
        T: IntoIterator<Item = Trigram>,
    {
        Self {
            postings: super::postings::FilePostings::from_files(
                files
                    .into_iter()
                    .map(|(id, trigrams)| (id, trigrams.into_iter().map(move |tg| (tg, id)))),
            ),
        }
    }

    /// Drop every posting contributed by `id`.
    pub fn remove_file(&mut self, id: FileId) {
        self.postings.remove_file(id);
    }

    /// Return file ids that contain ALL of `trigrams`. Empty input or any
    /// missing trigram yields an empty set.
    pub fn query(&self, trigrams: &[Trigram]) -> HashSet<FileId> {
        if trigrams.is_empty() {
            return HashSet::new();
        }
        let mut postings: Vec<Cow<'_, [FileId]>> = Vec::with_capacity(trigrams.len());
        for tg in trigrams {
            let files = self.postings.get(tg);
            if files.is_empty() {
                return HashSet::new();
            }
            postings.push(files);
        }
        postings.sort_by_key(|s| s.len());
        let (head, tail) = postings.split_first().expect("non-empty by construction");
        let mut acc: HashSet<FileId> = head.iter().copied().collect();
        for set in tail {
            acc.retain(|id| set.binary_search(id).is_ok());
            if acc.is_empty() {
                return acc;
            }
        }
        acc
    }

    /// Number of distinct trigrams in the posting table.
    pub fn distinct_trigrams(&self) -> usize {
        self.postings.len()
    }

    /// Capture the posting table as a snapshot-friendly vector. Used by
    /// the on-disk snapshot path; sorted by trigram for deterministic
    /// serialisation.
    pub fn snapshot_postings(&self) -> Vec<super::snapshot::TrigramPosting> {
        self.posting_rows()
            .into_iter()
            .map(|(trigram, files)| super::snapshot::TrigramPosting {
                trigram: *trigram,
                files: files.into_owned(),
            })
            .collect()
    }

    /// Every posting, sorted by trigram, borrowed where no edit touched it.
    pub(super) fn posting_rows(&self) -> Vec<(&Trigram, Cow<'_, [FileId]>)> {
        let mut rows = self.postings.rows();
        rows.sort_unstable_by_key(|(trigram, _)| **trigram);
        rows
    }

    /// Rebuild a [`TrigramIndex`] from a snapshot's postings vector.
    pub fn from_postings(postings: Vec<super::snapshot::TrigramPosting>) -> Self {
        Self {
            postings: super::postings::FilePostings::from_rows(
                postings.into_iter().map(|p| (p.trigram, p.files)),
            ),
        }
    }

    /// Nominal live-content bytes, independent of storage layout. Reported by
    /// `code_index.stats.memory_bytes`.
    pub fn estimated_bytes(&self) -> usize {
        self.postings.estimated_bytes(|_| 0)
    }

    /// Fold accumulated edits into a fresh shared base once they are worth it.
    pub(super) fn compact_if_heavy(&mut self) {
        self.postings.compact_if_heavy();
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
    fn restored_memberships_normalize_and_survive_replacement() {
        let tg = pack(b'a', b'b', b'c');
        let mut idx = TrigramIndex::from_postings(vec![
            super::super::snapshot::TrigramPosting {
                trigram: tg,
                files: vec![9, 2, 9],
            },
            super::super::snapshot::TrigramPosting {
                trigram: tg,
                files: vec![2, 4],
            },
        ]);
        assert_eq!(idx.query(&[tg]), HashSet::from([2, 4, 9]));
        idx.remove_file(2);
        assert_eq!(idx.query(&[tg]), HashSet::from([4, 9]));
        idx.index_file(2, "abc xyz");
        assert_eq!(idx.query(&[tg]), HashSet::from([2, 4, 9]));
        idx.index_file(4, "xyz");
        assert_eq!(idx.query(&[tg]), HashSet::from([2, 9]));
        let restored = TrigramIndex::from_postings(idx.snapshot_postings());
        assert_eq!(
            restored.query(&query_trigrams("xyz")),
            HashSet::from([2, 4])
        );
        assert!(restored.query(&query_trigrams("absent")).is_empty());
    }

    #[test]
    fn a_bulk_build_answers_like_per_file_indexing() {
        let sources = [
            (1, "fn alpha() { beta() }"),
            (2, "struct Beta;"),
            (4, ""),
            (7, "alpha beta gamma ALPHA"),
        ];
        let mut incremental = TrigramIndex::new();
        for (id, source) in sources {
            incremental.index_file(id, source);
        }
        let mut bulk = TrigramIndex::from_files(
            sources
                .iter()
                .map(|(id, source)| (*id, extract_trigrams(source))),
        );
        let same = |bulk: &TrigramIndex, incremental: &TrigramIndex| {
            let rows = |index: &TrigramIndex| -> Vec<(Trigram, Vec<FileId>)> {
                index
                    .snapshot_postings()
                    .into_iter()
                    .map(|posting| (posting.trigram, posting.files))
                    .collect()
            };
            assert_eq!(rows(bulk), rows(incremental));
            assert_eq!(bulk.distinct_trigrams(), incremental.distinct_trigrams());
            for needle in ["alp", "beta", "gamma", "absent"] {
                assert_eq!(
                    bulk.query(&query_trigrams(needle)),
                    incremental.query(&query_trigrams(needle))
                );
            }
        };
        same(&bulk, &incremental);
        // The removal index must survive the bulk path, or later edits
        // would leave stale postings behind.
        for index in [&mut bulk, &mut incremental] {
            index.remove_file(1);
            index.index_file(7, "delta");
            index.index_file(9, "alpha");
        }
        same(&bulk, &incremental);
    }

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
