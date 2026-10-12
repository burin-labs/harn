//! One owner for forward postings and their per-file removal projection.
//!
//! Posting vectors remain contiguous across recovery. The reverse projection
//! stores one shared term per contributing file, never one key per occurrence.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

use super::file_table::FileId;

pub(super) trait FilePosting: Sized {
    fn file(&self) -> FileId;

    fn normalize(_hits: &mut Vec<Self>) {}

    fn append(hits: &mut Vec<Self>, incoming: impl IntoIterator<Item = Self>) {
        hits.extend(incoming);
    }

    fn remove(hits: &mut Vec<Self>, file: FileId) {
        hits.retain(|hit| hit.file() != file);
    }
}

impl FilePosting for FileId {
    fn file(&self) -> FileId {
        *self
    }

    fn normalize(hits: &mut Vec<Self>) {
        hits.sort_unstable();
        hits.dedup();
    }

    fn append(hits: &mut Vec<Self>, incoming: impl IntoIterator<Item = Self>) {
        for file in incoming {
            if let Err(position) = hits.binary_search(&file) {
                hits.insert(position, file);
            }
        }
    }

    fn remove(hits: &mut Vec<Self>, file: FileId) {
        if let Ok(position) = hits.binary_search(&file) {
            hits.remove(position);
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct FilePostings<K, H> {
    forward: HashMap<K, Vec<H>>,
    reverse: HashMap<FileId, Vec<K>>,
}

impl<K, H> Default for FilePostings<K, H> {
    fn default() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, H: FilePosting> FilePostings<K, H> {
    pub fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> &[H]
    where
        K: Borrow<Q>,
    {
        self.forward.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn key<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&K>
    where
        K: Borrow<Q>,
    {
        self.forward.get_key_value(key).map(|(key, _)| key)
    }

    pub fn terms(&self) -> impl Iterator<Item = (&K, &Vec<H>)> {
        self.forward.iter()
    }

    pub fn len(&self) -> usize {
        self.forward.len()
    }

    pub fn remove_file(&mut self, file: FileId) {
        let Some(terms) = self.reverse.remove(&file) else {
            return;
        };
        for term in terms {
            if let Some(hits) = self.forward.get_mut(&term) {
                H::remove(hits, file);
                if hits.is_empty() {
                    self.forward.remove(&term);
                }
            }
        }
    }

    /// Replace one file's distinct terms and its removal projection together.
    /// Occurrences retain their order; membership postings remain sorted.
    pub fn replace_file<T: IntoIterator<Item = H>>(
        &mut self,
        file: FileId,
        rows: impl IntoIterator<Item = (K, T)>,
    ) {
        self.remove_file(file);
        let mut terms = Vec::new();
        for (key, hits) in rows {
            let entry = self.forward.entry(key);
            terms.push(entry.key().clone());
            let incoming = hits
                .into_iter()
                .inspect(|hit| debug_assert_eq!(hit.file(), file));
            H::append(entry.or_default(), incoming);
        }
        if !terms.is_empty() {
            self.reverse.insert(file, terms);
        }
    }

    /// Adopt snapshot vectors, merge repeated keys, then derive removal once.
    pub fn from_rows(rows: impl ExactSizeIterator<Item = (K, Vec<H>)>) -> Self {
        let mut forward: HashMap<K, Vec<H>> = HashMap::with_capacity(rows.len());
        for (key, hits) in rows {
            match forward.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(hits);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().extend(hits);
                }
            }
        }
        let mut reverse: HashMap<FileId, Vec<K>> = HashMap::new();
        let mut files = Vec::new();
        forward.retain(|key, hits| {
            H::normalize(hits);
            files.clear();
            for hit in hits.iter() {
                let file = hit.file();
                if files.last() != Some(&file) {
                    files.push(file);
                }
            }
            files.sort_unstable();
            files.dedup();
            for file in &files {
                reverse.entry(*file).or_default().push(key.clone());
            }
            !hits.is_empty()
        });
        Self { forward, reverse }
    }

    pub fn estimated_bytes(&self) -> usize {
        self.forward.capacity() * (std::mem::size_of::<K>() + std::mem::size_of::<Vec<H>>())
            + self
                .forward
                .values()
                .map(|hits| hits.capacity() * std::mem::size_of::<H>())
                .sum::<usize>()
            + self.reverse.capacity()
                * (std::mem::size_of::<FileId>() + std::mem::size_of::<Vec<K>>())
            + self
                .reverse
                .values()
                .map(|terms| terms.capacity() * std::mem::size_of::<K>())
                .sum::<usize>()
    }
}
