//! Word index over identifier-like tokens.
//!
//! Inverted index `identifier -> Vec<(file_id, line)>`. Complements the
//! trigram index: trigrams answer "which files contain this substring?",
//! the word index answers "which lines mention this exact identifier?".
//! Tokens are runs of `[A-Za-z_][A-Za-z0-9_]*` (single-character tokens
//! are skipped), one entry per occurrence.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use super::file_table::FileId;

/// Single occurrence of an identifier-shaped token: which file it landed
/// in and on which 1-based line number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WordHit {
    /// File the token was tokenized from.
    pub file: FileId,
    /// 1-based line number of the occurrence.
    pub line: u32,
}

impl super::postings::FilePosting for WordHit {
    fn file(&self) -> FileId {
        self.file
    }
}

/// Inverted word index keyed on identifier-shaped tokens.
#[derive(Debug, Default, Clone)]
pub struct WordIndex {
    pub(super) postings: super::postings::FilePostings<Arc<str>, WordHit>,
}

impl WordIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tokenize `content` and record every hit under `id`. If `id` already
    /// has entries, they are dropped first.
    pub fn index_file(&mut self, id: FileId, content: &str) {
        self.install_file(id, occurrence_lines(content), usize::MAX);
    }

    /// Replace `id`'s hits with already-tokenized `words`, as produced by
    /// [`occurrence_lines`]. Returns the interned keys of the words at least
    /// `ref_len` long, for the REFS scan that reads the same tokens.
    pub(super) fn install_file(
        &mut self,
        id: FileId,
        words: Vec<(String, Vec<u32>)>,
        ref_len: usize,
    ) -> Vec<Arc<str>> {
        let mut refs = Vec::new();
        let rows: Vec<_> = words
            .into_iter()
            .map(|(word, lines)| {
                let key = self
                    .postings
                    .key(word.as_str())
                    .cloned()
                    .unwrap_or_else(|| Arc::from(word));
                if key.len() >= ref_len {
                    refs.push(Arc::clone(&key));
                }
                let hits = lines
                    .into_iter()
                    .map(move |line| WordHit { file: id, line });
                (key, hits)
            })
            .collect();
        self.postings.replace_file(id, rows);
        refs
    }

    /// Drop every hit contributed by `id`.
    pub fn remove_file(&mut self, id: FileId) {
        self.postings.remove_file(id);
    }

    /// Every hit recorded under `word`. Borrowed unless an edit since the
    /// last compaction touched the word, in which case the live hits are
    /// assembled from the shared base and the edits.
    pub fn get(&self, word: &str) -> Cow<'_, [WordHit]> {
        self.postings.get(word)
    }

    /// Number of distinct tokens in the posting table.
    pub fn distinct_words(&self) -> usize {
        self.postings.len()
    }

    /// Capture the inverted index as snapshot-friendly postings. Sorted
    /// by word so the on-disk JSON is reproducible.
    pub fn snapshot_postings(&self) -> Vec<super::snapshot::WordPosting> {
        self.posting_rows()
            .into_iter()
            .map(|(word, hits)| super::snapshot::WordPosting {
                word: word.to_string(),
                hits: hits.iter().map(|h| (h.file, h.line)).collect(),
            })
            .collect()
    }

    /// Every posting, sorted by word, borrowed where no edit touched it.
    pub(super) fn posting_rows(&self) -> Vec<(&str, Cow<'_, [WordHit]>)> {
        let mut rows: Vec<(&str, Cow<'_, [WordHit]>)> = self
            .postings
            .rows()
            .into_iter()
            .map(|(word, hits)| (&**word, hits))
            .collect();
        rows.sort_unstable_by(|a, b| a.0.cmp(b.0));
        rows
    }

    /// Rebuild a [`WordIndex`] from a snapshot's postings vector.
    pub fn from_postings(postings: Vec<super::snapshot::WordPosting>) -> Self {
        Self {
            postings: super::postings::FilePostings::from_rows(postings.into_iter().map(|p| {
                (
                    Arc::from(p.word),
                    p.hits
                        .into_iter()
                        .map(|(file, line)| WordHit { file, line })
                        .collect(),
                )
            })),
        }
    }

    /// Nominal live-content bytes, independent of storage layout. Reported by
    /// `code_index.stats.memory_bytes`.
    pub fn estimated_bytes(&self) -> usize {
        self.postings.estimated_bytes(|key| key.len() + 16)
    }

    /// Fold accumulated edits into a fresh shared base once they are worth it.
    pub(super) fn compact_if_heavy(&mut self) {
        self.postings.compact_if_heavy();
    }
}

/// Shortest token the index records.
const MIN_WORD_LEN: usize = 2;

/// Distinct word-index tokens of `content`, each with the 1-based line of
/// every occurrence in order.
pub(super) fn occurrence_lines(content: &str) -> Vec<(String, Vec<u32>)> {
    let mut lines: HashMap<&str, Vec<u32>> = HashMap::new();
    for (index, line) in content.split('\n').enumerate() {
        let line_no = index as u32 + 1;
        tokenize(line, |word| {
            if word.len() >= MIN_WORD_LEN {
                lines.entry(word).or_default().push(line_no);
            }
        });
    }
    lines
        .into_iter()
        .map(|(word, lines)| (word.to_owned(), lines))
        .collect()
}

/// Whether [`WordIndex`] records `word` at all, so an empty
/// [`WordIndex::get`] means "absent" rather than "never indexed". False
/// for one-character names and anything [`tokenize`] would split or skip.
pub fn records(word: &str) -> bool {
    let mut tokens = 0;
    let mut whole = false;
    tokenize(word, |token| {
        tokens += 1;
        whole = token == word;
    });
    tokens == 1 && whole && word.len() >= MIN_WORD_LEN
}

/// Split a single line into identifier tokens. A token matches
/// `[A-Za-z_][A-Za-z0-9_]*`. Numeric-only runs are skipped. `yield_token`
/// is invoked once per occurrence — dedupe is the caller's responsibility.
pub fn tokenize<'a>(line: &'a str, mut yield_token: impl FnMut(&'a str)) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && is_ident_cont(bytes[i]) {
            i += 1;
        }
        // SAFETY: ASCII slice boundaries — `is_ident_start`/`is_ident_cont`
        // only ever match ASCII bytes, so `start..i` is on a UTF-8 boundary.
        let token = std::str::from_utf8(&bytes[start..i]).expect("ASCII run");
        yield_token(token);
    }
}

#[inline(always)]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline(always)]
fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_occurrences_preserve_multiplicity_and_survive_replacement() {
        let mut idx = WordIndex::from_postings(vec![
            super::super::snapshot::WordPosting {
                word: "alpha".into(),
                hits: vec![(2, 3), (9, 1), (2, 3)],
            },
            super::super::snapshot::WordPosting {
                word: "alpha".into(),
                hits: vec![(4, 2)],
            },
        ]);
        assert_eq!(
            &*idx.get("alpha"),
            &[
                WordHit { file: 2, line: 3 },
                WordHit { file: 9, line: 1 },
                WordHit { file: 2, line: 3 },
                WordHit { file: 4, line: 2 },
            ]
        );
        idx.remove_file(2);
        idx.index_file(4, "beta beta\nalpha alpha");
        assert_eq!(
            &*idx.get("alpha"),
            &[
                WordHit { file: 9, line: 1 },
                WordHit { file: 4, line: 2 },
                WordHit { file: 4, line: 2 },
            ]
        );
        let mut restored = WordIndex::from_postings(idx.snapshot_postings());
        assert_eq!(restored.get("alpha"), idx.get("alpha"));
        restored.remove_file(4);
        assert!(restored.get("beta").is_empty());
        assert_eq!(&*restored.get("alpha"), &[WordHit { file: 9, line: 1 }]);
    }

    #[test]
    fn tokenize_skips_punctuation_and_numbers() {
        let mut tokens: Vec<String> = Vec::new();
        tokenize("let foo_bar = baz(1, 2.0); // 42_things", |t| {
            tokens.push(t.to_string());
        });
        assert_eq!(tokens, vec!["let", "foo_bar", "baz", "_things"]);
    }

    #[test]
    fn index_records_line_numbers() {
        let mut idx = WordIndex::new();
        idx.index_file(7, "alpha\n  beta gamma\nalpha");
        let alpha_hits = idx.get("alpha");
        assert_eq!(alpha_hits.len(), 2);
        assert_eq!(alpha_hits[0], WordHit { file: 7, line: 1 });
        assert_eq!(alpha_hits[1], WordHit { file: 7, line: 3 });
        let gamma_hits = idx.get("gamma");
        assert_eq!(&*gamma_hits, &[WordHit { file: 7, line: 2 }]);
    }

    #[test]
    fn remove_and_reindex_replace_entries() {
        let mut idx = WordIndex::new();
        idx.index_file(1, "foo bar baz");
        idx.remove_file(1);
        assert!(idx.get("foo").is_empty());
        idx.index_file(1, "qux");
        assert!(idx.get("foo").is_empty());
        assert_eq!(&*idx.get("qux"), &[WordHit { file: 1, line: 1 }]);
    }

    #[test]
    fn single_character_tokens_are_skipped() {
        let mut idx = WordIndex::new();
        idx.index_file(1, "a foo b bar c");
        assert!(idx.get("a").is_empty());
        assert_eq!(idx.get("foo").len(), 1);
    }
}
