use std::collections::{hash_map::Entry, HashMap};
use std::sync::Arc;

use super::{value_equality_hash_key, value_structural_hash_key, values_equal, VmValue};

#[derive(Debug, Clone)]
enum MemberBucket {
    One(usize),
    Many(Vec<usize>),
}

impl MemberBucket {
    fn indices(&self) -> &[usize] {
        match self {
            Self::One(index) => std::slice::from_ref(index),
            Self::Many(indices) => indices,
        }
    }

    fn push(&mut self, index: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, index]),
            Self::Many(indices) => indices.push(index),
        }
    }
}

/// Backing store for [`VmValue::Set`](super::VmValue::Set).
///
/// A Harn set is an *ordered* collection that deduplicates by structural
/// equality. `VmSet` keeps a resident structural-hash index alongside the
/// ordered items, so membership probes only a candidate bucket and does one key
/// computation per probe instead of rebuilding an index for each query.
/// A probe compares O(k) candidates; ambiguous numeric and mutable-value buckets
/// can grow, while unrelated keys remain indexed separately. Singleton buckets
/// store their index inline without a per-member allocation.
///
/// Invariants:
/// * `buckets` indexes item positions by a mutation-stable membership key.
///   Every candidate hit is confirmed with the owning `values_equal` operation.
/// * Insertion order is preserved in `items` (iteration, display, and
///   serialization are order-stable); equality and hashing stay
///   order-independent, matching the language spec.
#[derive(Debug, Clone, Default)]
pub struct VmSet {
    /// Items in insertion order. Stored behind an `Arc` so iteration can share
    /// the backing buffer with a `VmIter`/`IterState` without copying, and
    /// value-semantic mutation clones copy-on-write via `Arc::make_mut`.
    items: Arc<Vec<VmValue>>,
    /// Insertion-time fingerprints retain recorded replay encoding even
    /// when a member's atomic contents or iterator state later change.
    fingerprints: Arc<Vec<String>>,
    buckets: HashMap<String, MemberBucket>,
}

impl VmSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Arc::new(Vec::with_capacity(capacity)),
            fingerprints: Arc::new(Vec::with_capacity(capacity)),
            buckets: HashMap::with_capacity(capacity),
        }
    }

    /// Insert `value`, returning `true` when no equal value was already present.
    pub fn insert(&mut self, value: VmValue) -> bool {
        match self.buckets.entry(value_equality_hash_key(&value)) {
            Entry::Occupied(mut entry) => {
                if entry
                    .get()
                    .indices()
                    .iter()
                    .any(|&index| values_equal(&self.items[index], &value))
                {
                    return false;
                }
                entry.get_mut().push(self.items.len());
            }
            Entry::Vacant(entry) => {
                entry.insert(MemberBucket::One(self.items.len()));
            }
        }
        Arc::make_mut(&mut self.fingerprints).push(value_structural_hash_key(&value));
        Arc::make_mut(&mut self.items).push(value);
        true
    }

    /// Indexed membership with equality confirmation inside the candidate bucket.
    pub fn contains(&self, value: &VmValue) -> bool {
        self.buckets
            .get(&value_equality_hash_key(value))
            .is_some_and(|bucket| {
                bucket
                    .indices()
                    .iter()
                    .any(|&index| values_equal(&self.items[index], value))
            })
    }

    /// Match every member once. Harn's lossy mixed numeric equality is not
    /// transitive, so a subset test or greedy pairing can reuse the same member
    /// or reject a valid pairing. Search alternating paths only inside buckets;
    /// ordinary unique buckets take one comparison per member.
    pub(super) fn structurally_equal(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        let mut left_matches = vec![None; self.len()];
        let mut right_matches = vec![None; other.len()];
        let mut seen = vec![0; other.len()];
        let mut parent = vec![0; other.len()];
        let mut pending = Vec::new();
        for root in 0..self.len() {
            pending.clear();
            pending.push(root);
            let mut endpoint = None;
            'search: while let Some(left) = pending.pop() {
                let key = value_equality_hash_key(&self.items[left]);
                let Some(candidates) = other.buckets.get(&key) else {
                    continue;
                };
                for &right in candidates.indices() {
                    if seen[right] == root + 1
                        || !values_equal(&self.items[left], &other.items[right])
                    {
                        continue;
                    }
                    seen[right] = root + 1;
                    parent[right] = left;
                    match right_matches[right] {
                        Some(previous) => pending.push(previous),
                        None => {
                            endpoint = Some(right);
                            break 'search;
                        }
                    }
                }
            }
            let Some(mut right) = endpoint else {
                return false;
            };
            loop {
                let left = parent[right];
                let previous = left_matches[left];
                left_matches[left] = Some(right);
                right_matches[right] = Some(left);
                match previous {
                    Some(previous) => right = previous,
                    None => break,
                }
            }
        }
        true
    }

    /// Remove `value` if present, returning `true` when it was removed.
    pub fn remove(&mut self, value: &VmValue) -> bool {
        if !self.contains(value) {
            return false;
        }
        let fingerprints = Arc::make_mut(&mut self.fingerprints);
        let mut kept = 0;
        let mut cursor = 0;
        Arc::make_mut(&mut self.items).retain(|item| {
            let keep = !values_equal(item, value);
            if keep {
                fingerprints.swap(kept, cursor);
                kept += 1;
            }
            cursor += 1;
            keep
        });
        fingerprints.truncate(kept);
        self.buckets.clear();
        for (index, item) in self.items.iter().enumerate() {
            self.buckets
                .entry(value_equality_hash_key(item))
                .and_modify(|bucket| bucket.push(index))
                .or_insert(MemberBucket::One(index));
        }
        true
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

    pub(super) fn sorted_membership_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self
            .buckets
            .iter()
            .flat_map(|(key, bucket)| std::iter::repeat_n(key.as_str(), bucket.indices().len()))
            .collect();
        keys.sort_unstable();
        keys
    }

    pub(super) fn sorted_fingerprint_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.fingerprints.iter().map(String::as_str).collect();
        keys.sort_unstable();
        // The legacy resident HashSet encoded unique fingerprint strings.
        keys.dedup();
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

    #[test]
    fn collisions_and_large_mixed_numbers_confirm_equality() {
        let first = VmValue::Int(9_007_199_254_740_992);
        let second = VmValue::Int(9_007_199_254_740_993);
        let floating = VmValue::Float(9_007_199_254_740_992.0);
        let mut set: VmSet = [first.clone(), second.clone()].into_iter().collect();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&floating));
        assert!(!set.insert(floating));
        assert!(set.remove(&first));
        assert!(set.contains(&second));
        assert_eq!(set.len(), 1);

        let nan = VmValue::Float(f64::NAN);
        let mut set: VmSet = [nan.clone(), nan.clone()].into_iter().collect();
        assert_eq!(set.len(), 2);
        assert!(!set.contains(&nan));
        assert!(!set.remove(&nan));
    }

    #[test]
    fn nested_atomic_keys_stay_reachable_after_mutation() {
        use std::sync::atomic::{AtomicI64, Ordering};
        let contents = Arc::new(AtomicI64::new(1));
        let atomic = VmValue::atomic(crate::value::VmAtomicHandle {
            value: contents.clone(),
        });
        let nested = VmValue::dict([("values", VmValue::List(Arc::new(vec![atomic])))]);
        let mut set: VmSet = std::iter::once(nested.clone()).collect();
        let snapshot = set.shared_items();
        let fingerprint = value_structural_hash_key(&VmValue::set_value(set.clone()));
        contents.store(2, Ordering::SeqCst);
        assert_eq!(
            value_structural_hash_key(&VmValue::set_value(set.clone())),
            fingerprint
        );
        let equal = VmValue::dict([(
            "values",
            VmValue::List(Arc::new(vec![VmValue::atomic(
                crate::value::VmAtomicHandle {
                    value: Arc::new(AtomicI64::new(2)),
                },
            )])),
        )]);
        assert!(values_equal(&nested, &equal));
        assert!(set.contains(&equal));
        assert!(!set.insert(equal.clone()));
        assert!(set.remove(&equal));
        assert!(set.is_empty());
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn set_equality_pairs_members_once_and_reassigns_ambiguous_matches() {
        let i1 = VmValue::Int(9_007_199_254_740_992);
        let i2 = VmValue::Int(9_007_199_254_740_993);
        let f = VmValue::Float(9_007_199_254_740_992.0);
        let a = VmValue::set([i1.clone(), i2.clone()]);
        let b = VmValue::set([f.clone(), VmValue::Int(7)]);
        assert!(!values_equal(&a, &b));
        assert!(!values_equal(&b, &a));

        let list = |values| VmValue::List(Arc::new(values));
        let left = VmValue::set([
            list(vec![i2.clone(), f.clone()]),
            list(vec![i1.clone(), i1.clone()]),
        ]);
        let right = VmValue::set([list(vec![f, i1]), list(vec![i2.clone(), i2])]);
        assert!(values_equal(&left, &right));
        assert!(values_equal(&right, &left));
        let nested = VmValue::set([left.clone(), right.clone()]);
        assert_eq!(nested_set_len(&left), 2);
        assert_eq!(nested_set_len(&right), 2);
        assert_eq!(nested_set_len(&nested), 1);
        let different = VmValue::set([VmValue::Int(3), VmValue::Int(4)]);
        assert!(!values_equal(&left, &different));
        assert!(!nested_set_contains(&nested, &different));
    }

    fn nested_set_len(value: &VmValue) -> usize {
        match value {
            VmValue::Set(set) => set.len(),
            _ => unreachable!(),
        }
    }

    fn nested_set_contains(set: &VmValue, value: &VmValue) -> bool {
        match set {
            VmValue::Set(set) => set.contains(value),
            _ => unreachable!(),
        }
    }

    #[test]
    fn fingerprints_keep_the_persisted_encoding() {
        let i1 = VmValue::Int(9_007_199_254_740_993);
        let f = VmValue::Float(9_007_199_254_740_992.0);
        assert_eq!(value_structural_hash_key(&i1), "i9007199254740993;");
        assert_eq!(value_structural_hash_key(&f), "i9007199254740992;");
        assert_eq!(
            value_structural_hash_key(&VmValue::dict([("number", i1)])),
            "D6:number=i9007199254740993;,}"
        );
        assert_eq!(
            value_structural_hash_key(&VmValue::set([VmValue::Int(2), VmValue::Int(1)])),
            "Si1;,i2;,}"
        );
    }

    #[test]
    fn resource_membership_uses_payload_identity_not_display_labels() {
        use crate::value::VmResourceHandle;
        let payload = Arc::new(7);
        let first = VmValue::resource(VmResourceHandle::from_arc("first", payload.clone()));
        let equal = VmValue::resource(VmResourceHandle::from_arc("second", payload));
        let different = VmValue::resource(VmResourceHandle::new("first", 7));
        let mut set: VmSet = std::iter::once(first.clone()).collect();
        assert!(values_equal(&first, &equal));
        assert!(set.contains(&equal));
        assert!(!set.insert(equal));
        assert!(!set.contains(&different));
        assert!(set.insert(different));
        assert_eq!(set.len(), 2);
    }
}
