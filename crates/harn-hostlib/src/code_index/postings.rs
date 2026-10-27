//! One owner for postings: a shared base generation, file tombstones, and a
//! delta.
//!
//! Postings restored from a snapshot, or folded in by compaction, form an
//! immutable base shared by clones. A file indexed or removed afterwards is
//! tombstoned in the base, and its new hits live in a delta that keeps the
//! per-file removal projection, so the base never needs one: restoring it is
//! one pass over the snapshot rows. Reads drop tombstoned files from base hits
//! and add the delta's. [`FilePostings::compact_if_heavy`] folds the delta and
//! tombstones into a fresh base once they accumulate.

use std::borrow::{Borrow, Cow};
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::Arc;

use foldhash::HashMap as FastMap;

use super::file_table::FileId;

pub(super) trait FilePosting: Sized + Clone {
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

/// Fewer tombstones than this are not worth a whole-index compaction.
const TOMBSTONE_FLOOR: usize = 256;
/// Fewer delta terms than this are not worth a whole-index compaction.
const DELTA_FLOOR: usize = 4096;

#[derive(Debug, Clone)]
pub(super) struct FilePostings<K, H> {
    // Word keys come from file contents, so the hasher stays seeded per
    // process; foldhash does that at a fraction of SipHash's cost.
    base: Arc<FastMap<K, Vec<H>>>,
    /// Files whose base hits no longer count: removed or re-indexed since
    /// the base was built.
    stale: FileSet,
    /// Hits of files indexed since the base was built.
    forward: FastMap<K, Vec<H>>,
    /// Each delta file's terms, for removing it again.
    reverse: FastMap<FileId, Vec<K>>,
}

impl<K, H> Default for FilePostings<K, H> {
    fn default() -> Self {
        Self {
            base: Arc::default(),
            stale: FileSet::default(),
            forward: FastMap::default(),
            reverse: FastMap::default(),
        }
    }
}

impl<K: Eq + Hash + Clone, H: FilePosting> FilePostings<K, H> {
    /// Every live hit for `key`. Borrowed when no edit touched the term.
    pub fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Cow<'_, [H]>
    where
        K: Borrow<Q>,
    {
        let base = self.base.get(key).map_or(&[][..], Vec::as_slice);
        let delta = self.forward.get(key).map_or(&[][..], Vec::as_slice);
        let base_live =
            self.stale.is_empty() || !base.iter().any(|hit| self.stale.contains(hit.file()));
        if base_live && delta.is_empty() {
            return Cow::Borrowed(base);
        }
        if base.is_empty() {
            return Cow::Borrowed(delta);
        }
        let mut hits: Vec<H> = base
            .iter()
            .filter(|hit| !self.stale.contains(hit.file()))
            .cloned()
            .collect();
        H::append(&mut hits, delta.iter().cloned());
        Cow::Owned(hits)
    }

    pub fn key<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&K>
    where
        K: Borrow<Q>,
    {
        self.forward
            .get_key_value(key)
            .or_else(|| self.base.get_key_value(key))
            .map(|(key, _)| key)
    }

    /// Every term with live hits, in no particular order. Hits are borrowed
    /// unless an edit since the last compaction touched the term.
    pub fn rows(&self) -> Vec<(&K, Cow<'_, [H]>)> {
        let mut rows: Vec<(&K, Cow<'_, [H]>)> = self
            .base
            .keys()
            .map(|key| (key, self.get(key)))
            .filter(|(_, hits)| !hits.is_empty())
            .collect();
        rows.extend(
            self.forward
                .iter()
                .filter(|(key, _)| !self.base.contains_key(*key))
                .map(|(key, hits)| (key, Cow::Borrowed(hits.as_slice()))),
        );
        rows
    }

    /// Distinct terms with live hits.
    pub fn len(&self) -> usize {
        self.rows().len()
    }

    pub fn remove_file(&mut self, file: FileId) {
        self.stale.insert(file);
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

    /// Build the base from whole files in strictly ascending id order, as a
    /// cold build publishes them. Each hit is appended, never inserted, and
    /// the base needs no per-file removal projection.
    pub fn from_files<T>(files: impl IntoIterator<Item = (FileId, T)>) -> Self
    where
        T: IntoIterator<Item = (K, H)>,
    {
        let mut base: FastMap<K, Vec<H>> = FastMap::default();
        let mut previous: Option<FileId> = None;
        for (file, hits) in files {
            debug_assert!(
                previous < Some(file),
                "files must arrive in ascending id order"
            );
            previous = Some(file);
            for (key, hit) in hits {
                debug_assert_eq!(hit.file(), file);
                base.entry(key).or_default().push(hit);
            }
        }
        // Ascending files leave membership postings sorted; this pass only
        // confirms it.
        base.values_mut().for_each(H::normalize);
        Self {
            base: Arc::new(base),
            ..Self::default()
        }
    }

    /// Adopt snapshot vectors as the base, merging repeated keys.
    pub fn from_rows(rows: impl ExactSizeIterator<Item = (K, Vec<H>)>) -> Self {
        let mut base: FastMap<K, Vec<H>> =
            FastMap::with_capacity_and_hasher(rows.len(), Default::default());
        for (key, hits) in rows {
            match base.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(hits);
                }
                Entry::Occupied(mut entry) => {
                    entry.get_mut().extend(hits);
                }
            }
        }
        base.retain(|_, hits| {
            H::normalize(hits);
            !hits.is_empty()
        });
        Self {
            base: Arc::new(base),
            ..Self::default()
        }
    }

    /// Fold the delta and tombstones into a fresh base once they are a
    /// sizable fraction of it. A full build indexes every file into the
    /// delta, so its first batch boundary always compacts.
    pub fn compact_if_heavy(&mut self) {
        let heavy_delta = self.forward.len() > DELTA_FLOOR.max(self.base.len() / 4);
        if heavy_delta || self.stale.len() > TOMBSTONE_FLOOR {
            self.compact();
        }
    }

    /// Fold every edit into a fresh base. Reads before and after are identical.
    pub fn compact(&mut self) {
        let mut forward = std::mem::take(&mut self.forward);
        let stale = std::mem::take(&mut self.stale);
        self.reverse.clear();
        let mut base = Arc::try_unwrap(std::mem::take(&mut self.base))
            .unwrap_or_else(|shared| (*shared).clone());
        base.retain(|key, hits| {
            if !stale.is_empty() {
                hits.retain(|hit| !stale.contains(hit.file()));
            }
            if let Some(delta) = forward.remove(key) {
                H::append(hits, delta);
            }
            !hits.is_empty()
        });
        base.extend(forward);
        self.base = Arc::new(base);
    }

    /// Nominal bytes for live terms and hits, independent of storage layout.
    pub fn estimated_bytes(&self, key_bytes: impl Fn(&K) -> usize) -> usize {
        self.rows()
            .into_iter()
            .map(|(key, hits)| {
                std::mem::size_of::<K>()
                    + std::mem::size_of::<Vec<H>>()
                    + hits.len() * std::mem::size_of::<H>()
                    + key_bytes(key)
            })
            .sum()
    }
}

/// A set of file ids as a bitmap. File ids are small and dense, so a
/// membership test on every base hit is one shift and mask.
#[derive(Debug, Default, Clone)]
struct FileSet {
    bits: Vec<u64>,
    len: usize,
}

impl FileSet {
    fn insert(&mut self, file: FileId) {
        let (word, bit) = (file as usize / 64, file % 64);
        if word >= self.bits.len() {
            self.bits.resize(word + 1, 0);
        }
        if self.bits[word] & (1 << bit) == 0 {
            self.bits[word] |= 1 << bit;
            self.len += 1;
        }
    }

    fn contains(&self, file: FileId) -> bool {
        self.bits
            .get(file as usize / 64)
            .is_some_and(|word| word & (1 << (file % 64)) != 0)
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
#[path = "posting_store_tests.rs"]
mod tests;
