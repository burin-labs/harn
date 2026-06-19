use std::collections::HashSet;
use std::sync::Arc;

use super::{value_structural_hash_key, VmValue};

/// Backing store for [`VmValue::Set`](super::VmValue::Set).
///
/// A Harn set is an *ordered* collection that deduplicates by structural
/// equality. Membership used to be answered by rebuilding a
/// `HashSet<String>` of structural hash keys from the backing `Vec` on every
/// `contains` / `union` / `intersect` / … call — O(n) work (plus an
/// allocation) per query. `VmSet` keeps that index resident alongside the
/// items, so membership is O(1) and the set-algebra builtins drop from
/// rebuild-per-call to a single key computation per probe.
///
/// Invariants:
/// * `keys` holds exactly one entry per item: `value_structural_hash_key(item)`.
/// * Insertion order is preserved in `items` (iteration, display, and
///   serialization are order-stable); equality and hashing stay
///   order-independent, matching the language spec.
#[derive(Debug, Clone, Default)]
pub struct VmSet {
    /// Items in insertion order. Stored behind an `Arc` so iteration can share
    /// the backing buffer with a `VmIter`/`IterState` without copying, and
    /// value-semantic mutation clones copy-on-write via `Arc::make_mut`.
    items: Arc<Vec<VmValue>>,
    keys: HashSet<String>,
}

impl VmSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Arc::new(Vec::with_capacity(capacity)),
            keys: HashSet::with_capacity(capacity),
        }
    }

    /// Insert `value`, returning `true` when it was newly added (its structural
    /// key was not already present).
    pub fn insert(&mut self, value: VmValue) -> bool {
        let key = value_structural_hash_key(&value);
        if self.keys.insert(key) {
            Arc::make_mut(&mut self.items).push(value);
            true
        } else {
            false
        }
    }

    /// O(1) membership test by structural equality.
    pub fn contains(&self, value: &VmValue) -> bool {
        self.keys.contains(&value_structural_hash_key(value))
    }

    /// Remove `value` if present, returning `true` when it was removed.
    pub fn remove(&mut self, value: &VmValue) -> bool {
        let key = value_structural_hash_key(value);
        if self.keys.remove(&key) {
            Arc::make_mut(&mut self.items).retain(|item| value_structural_hash_key(item) != key);
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, VmValue> {
        self.items.iter()
    }

    pub fn items(&self) -> &[VmValue] {
        self.items.as_slice()
    }

    /// The shared backing buffer, for iterators that walk a set without
    /// copying its elements (a refcount bump rather than an O(n) clone).
    pub fn shared_items(&self) -> Arc<Vec<VmValue>> {
        Arc::clone(&self.items)
    }

    /// Consume the set, yielding its items in insertion order. Used by the
    /// iterative teardown path so deeply nested sets drop without recursing.
    pub fn into_items(self) -> Vec<VmValue> {
        Arc::try_unwrap(self.items).unwrap_or_else(|items| (*items).clone())
    }

    /// `self ∪ other`, preserving `self`'s order then `other`'s new items.
    pub fn union(&self, other: &VmSet) -> VmSet {
        let mut out = self.clone();
        for value in other.iter() {
            out.insert(value.clone());
        }
        out
    }

    /// `self ∩ other` — items of `self` also present in `other`.
    pub fn intersect(&self, other: &VmSet) -> VmSet {
        self.items
            .iter()
            .filter(|item| other.contains(item))
            .cloned()
            .collect()
    }

    /// `self \ other` — items of `self` not present in `other`.
    pub fn difference(&self, other: &VmSet) -> VmSet {
        self.items
            .iter()
            .filter(|item| !other.contains(item))
            .cloned()
            .collect()
    }

    /// `self △ other` — items in exactly one of the two sets.
    pub fn symmetric_difference(&self, other: &VmSet) -> VmSet {
        let mut out: VmSet = self
            .iter()
            .filter(|item| !other.contains(item))
            .cloned()
            .collect();
        for value in other.iter() {
            if !self.contains(value) {
                out.insert(value.clone());
            }
        }
        out
    }

    pub fn is_subset(&self, other: &VmSet) -> bool {
        self.items.iter().all(|item| other.contains(item))
    }

    pub fn is_superset(&self, other: &VmSet) -> bool {
        other.is_subset(self)
    }

    pub fn is_disjoint(&self, other: &VmSet) -> bool {
        !self.items.iter().any(|item| other.contains(item))
    }

    /// Structural keys in sorted order. Backs the order-independent set hash
    /// in [`value_structural_hash_key`](super::value_structural_hash_key)
    /// without re-deriving a key per element.
    pub fn sorted_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.keys.iter().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }
}

impl FromIterator<VmValue> for VmSet {
    fn from_iter<I: IntoIterator<Item = VmValue>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let mut set = VmSet::with_capacity(iter.size_hint().0);
        for value in iter {
            set.insert(value);
        }
        set
    }
}

impl<'a> IntoIterator for &'a VmSet {
    type Item = &'a VmValue;
    type IntoIter = std::slice::Iter<'a, VmValue>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{value_structural_hash_key, values_equal};

    fn int_set(values: &[i64]) -> VmSet {
        values.iter().map(|n| VmValue::Int(*n)).collect()
    }

    #[test]
    fn insert_dedups_and_preserves_first_seen_order() {
        let set = int_set(&[3, 1, 3, 2, 1]);
        assert_eq!(set.len(), 3);
        let order: Vec<i64> = set
            .iter()
            .map(|v| match v {
                VmValue::Int(n) => *n,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(order, vec![3, 1, 2]);
    }

    #[test]
    fn contains_is_structural() {
        let set = int_set(&[1, 2, 3]);
        assert!(set.contains(&VmValue::Int(2)));
        assert!(!set.contains(&VmValue::Int(9)));
        // `1` and `1.0` are structurally equal in Harn.
        assert!(set.contains(&VmValue::Float(1.0)));
    }

    #[test]
    fn remove_updates_index_and_order() {
        let mut set = int_set(&[1, 2, 3]);
        assert!(set.remove(&VmValue::Int(2)));
        assert!(!set.remove(&VmValue::Int(2)));
        assert!(!set.contains(&VmValue::Int(2)));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn set_algebra() {
        let a = int_set(&[1, 2, 3]);
        let b = int_set(&[2, 3, 4]);
        assert_eq!(a.union(&b).len(), 4);
        assert_eq!(a.intersect(&b).len(), 2);
        assert!(a.intersect(&b).contains(&VmValue::Int(2)));
        let diff = a.difference(&b);
        assert_eq!(diff.len(), 1);
        assert!(diff.contains(&VmValue::Int(1)));
        let sym = a.symmetric_difference(&b);
        assert_eq!(sym.len(), 2);
        assert!(sym.contains(&VmValue::Int(1)) && sym.contains(&VmValue::Int(4)));
        assert!(int_set(&[1, 2]).is_subset(&a));
        assert!(a.is_superset(&int_set(&[1, 2])));
        assert!(a.is_disjoint(&int_set(&[7, 8])));
        assert!(!a.is_disjoint(&b));
    }

    #[test]
    fn equality_and_hash_are_order_independent() {
        let a = VmValue::set([VmValue::Int(1), VmValue::Int(2), VmValue::Int(3)]);
        let b = VmValue::set([VmValue::Int(3), VmValue::Int(2), VmValue::Int(1)]);
        assert!(values_equal(&a, &b));
        assert_eq!(value_structural_hash_key(&a), value_structural_hash_key(&b));
        let c = VmValue::set([VmValue::Int(1), VmValue::Int(2)]);
        assert!(!values_equal(&a, &c));
    }
}
