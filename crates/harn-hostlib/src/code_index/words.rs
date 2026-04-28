//! Word index over identifier-like tokens.
//!
//! Inverted index `identifier -> Vec<(file_id, line)>`. Complements the
//! trigram index: trigrams answer "which files contain this substring?",
//! the word index answers "which lines mention this exact identifier?".
//! Tokens are runs of `[A-Za-z_][A-Za-z0-9_]*` (single-character tokens
//! are skipped), one entry per occurrence.

use std::collections::{HashMap, HashSet};

use super::file_table::FileId;

/// Single occurrence of an identifier-shaped token: which file it landed
/// in and on which 1-based line number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordHit {
    /// File the token was tokenized from.
    pub file: FileId,
    /// 1-based line number of the occurrence.
    pub line: u32,
}

/// Inverted word index keyed on identifier-shaped tokens.
#[derive(Debug, Default, Clone)]
pub struct WordIndex {
    index: HashMap<String, Vec<WordHit>>,
    file_words: HashMap<FileId, HashSet<String>>,
}

impl WordIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tokenize `content` and record every hit under `id`. If `id` already
    /// has entries, they are dropped first.
    pub fn index_file(&mut self, id: FileId, content: &str) {
        self.remove_file(id);
        let mut contributed: HashSet<String> = HashSet::new();
        for (line_idx, line) in content.split('\n').enumerate() {
            let line_no = (line_idx as u32) + 1;
            tokenize(line, |word| {
                if word.len() < 2 {
                    return;
                }
                self.index
                    .entry(word.to_string())
                    .or_default()
                    .push(WordHit {
                        file: id,
                        line: line_no,
                    });
                contributed.insert(word.to_string());
            });
        }
        if !contributed.is_empty() {
            self.file_words.insert(id, contributed);
        }
    }

    /// Drop every hit contributed by `id`.
    pub fn remove_file(&mut self, id: FileId) {
        let Some(words) = self.file_words.remove(&id) else {
            return;
        };
        for word in words {
            if let Some(hits) = self.index.get_mut(&word) {
                hits.retain(|h| h.file != id);
                if hits.is_empty() {
                    self.index.remove(&word);
                }
            }
        }
    }

    /// O(1) lookup for every hit recorded under `word`.
    pub fn get(&self, word: &str) -> &[WordHit] {
        self.index.get(word).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Number of distinct tokens in the posting table.
    pub fn distinct_words(&self) -> usize {
        self.index.len()
    }

    /// Capture the inverted index as snapshot-friendly postings. Sorted
    /// by word so the on-disk JSON is reproducible.
    pub fn snapshot_postings(&self) -> Vec<super::snapshot::WordPosting> {
        let mut out: Vec<super::snapshot::WordPosting> = self
            .index
            .iter()
            .map(|(word, hits)| super::snapshot::WordPosting {
                word: word.clone(),
                hits: hits.iter().map(|h| (h.file, h.line)).collect(),
            })
            .collect();
        out.sort_by(|a, b| a.word.cmp(&b.word));
        out
    }

    /// Rebuild a [`WordIndex`] from a snapshot's postings vector.
    pub fn from_postings(postings: Vec<super::snapshot::WordPosting>) -> Self {
        let mut idx = Self::new();
        for p in postings {
            let mut contributing_files: HashSet<FileId> = HashSet::new();
            let entry = idx.index.entry(p.word.clone()).or_default();
            for (file, line) in &p.hits {
                entry.push(WordHit {
                    file: *file,
                    line: *line,
                });
                contributing_files.insert(*file);
            }
            for file in contributing_files {
                idx.file_words
                    .entry(file)
                    .or_default()
                    .insert(p.word.clone());
            }
        }
        idx
    }

    /// Order-of-magnitude resident-bytes estimate. Reported by
    /// `code_index.stats.memory_bytes`.
    pub fn estimated_bytes(&self) -> usize {
        let words = self.index.len();
        let key_bytes: usize = self.index.keys().map(|k| k.len()).sum();
        let hits: usize = self.index.values().map(Vec::len).sum();
        // Each WordHit packs into 8 bytes (FileId + u32). Add overhead for
        // the per-word vec plus the reverse map.
        words * 16 + key_bytes + hits * 8 + self.file_words.len() * 16
    }
}

/// Split a single line into identifier tokens. A token matches
/// `[A-Za-z_][A-Za-z0-9_]*`. Numeric-only runs are skipped. `yield_token`
/// is invoked once per occurrence — dedupe is the caller's responsibility.
pub fn tokenize(line: &str, mut yield_token: impl FnMut(&str)) {
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
    fn tokenize_skips_punctuation_and_numbers() {
        let mut tokens: Vec<String> = Vec::new();
        tokenize("let foo_bar = baz(1, 2.0); // 42_things", |t| {
            tokens.push(t.to_string())
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
        assert_eq!(gamma_hits, &[WordHit { file: 7, line: 2 }]);
    }

    #[test]
    fn remove_and_reindex_replace_entries() {
        let mut idx = WordIndex::new();
        idx.index_file(1, "foo bar baz");
        idx.remove_file(1);
        assert!(idx.get("foo").is_empty());
        idx.index_file(1, "qux");
        assert!(idx.get("foo").is_empty());
        assert_eq!(idx.get("qux"), &[WordHit { file: 1, line: 1 }]);
    }

    #[test]
    fn single_character_tokens_are_skipped() {
        let mut idx = WordIndex::new();
        idx.index_file(1, "a foo b bar c");
        assert!(idx.get("a").is_empty());
        assert_eq!(idx.get("foo").len(), 1);
    }
}
