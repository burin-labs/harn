use crate::chunk::{
    AdaptiveBinaryOp, AdaptiveBinaryState, Chunk, DirectCallState, InlineCacheEntry,
    MethodCacheTarget, PropertyCacheTarget,
};

use super::Vm;

impl Vm {
    pub(crate) fn inline_cache_set_index_for_chunk(&mut self, chunk: &Chunk) -> usize {
        self.inline_cache_set_index_for_key(chunk.cache_id(), chunk.inline_cache_slot_count())
    }

    fn inline_cache_set_index_for_key(&mut self, cache_id: u64, slot_count: usize) -> usize {
        if let Some(&index) = self.inline_cache_set_by_chunk.get(&cache_id) {
            self.ensure_inline_cache_set_len(index, slot_count);
            return index;
        }

        let index = self.inline_cache_sets.len();
        self.inline_cache_sets
            .push(vec![InlineCacheEntry::Empty; slot_count]);
        self.inline_cache_set_by_chunk.insert(cache_id, index);
        index
    }

    fn ensure_inline_cache_set_len(&mut self, cache_set: usize, slot_count: usize) {
        let Some(entries) = self.inline_cache_sets.get_mut(cache_set) else {
            return;
        };
        if entries.len() < slot_count {
            entries.resize(slot_count, InlineCacheEntry::Empty);
        }
    }

    #[inline]
    pub(crate) fn peek_adaptive_binary_cache_by_index(
        &self,
        cache_set: usize,
        slot: usize,
    ) -> Option<(AdaptiveBinaryOp, AdaptiveBinaryState)> {
        match self.inline_cache_sets.get(cache_set)?.get(slot)? {
            &InlineCacheEntry::AdaptiveBinary { op, state } => Some((op, state)),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_method_cache_by_index(
        &self,
        cache_set: usize,
        slot: usize,
    ) -> Option<(u16, usize, MethodCacheTarget)> {
        match self.inline_cache_sets.get(cache_set)?.get(slot)? {
            &InlineCacheEntry::Method {
                name_idx,
                argc,
                target,
            } => Some((name_idx, argc, target)),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_property_cache_by_index(
        &self,
        cache_set: usize,
        slot: usize,
    ) -> Option<(u16, PropertyCacheTarget)> {
        match self.inline_cache_sets.get(cache_set)?.get(slot)? {
            InlineCacheEntry::Property { name_idx, target } => Some((*name_idx, target.clone())),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_direct_call_state_by_index(
        &self,
        cache_set: usize,
        slot: usize,
    ) -> Option<DirectCallState> {
        match self.inline_cache_sets.get(cache_set)?.get(slot)? {
            InlineCacheEntry::DirectCall { state } => Some(state.clone()),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn set_inline_cache_entry_by_index(
        &mut self,
        cache_set: usize,
        slot_count: usize,
        slot: usize,
        entry: InlineCacheEntry,
    ) {
        self.ensure_inline_cache_set_len(cache_set, slot_count);
        let Some(entries) = self.inline_cache_sets.get_mut(cache_set) else {
            return;
        };
        if let Some(existing) = entries.get_mut(slot) {
            *existing = entry;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::chunk::{
        AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, Chunk, InlineCacheEntry, Op,
    };
    use crate::Vm;

    #[test]
    fn cache_set_index_reuses_chunk_identity() {
        let mut chunk = Chunk::new();
        let op_offset = chunk.code.len();
        chunk.emit(Op::Add, 1);
        let slot = chunk
            .inline_cache_slot(op_offset)
            .expect("adaptive op registers inline-cache slot");
        let slot_count = chunk.inline_cache_slot_count();

        let mut vm = Vm::new();
        let cache_set = vm.inline_cache_set_index_for_chunk(&chunk);
        assert_eq!(cache_set, vm.inline_cache_set_index_for_chunk(&chunk));

        let cloned_chunk = chunk.clone();
        assert_eq!(
            cache_set,
            vm.inline_cache_set_index_for_chunk(&cloned_chunk),
            "Chunk clones preserve cache identity and should share VM feedback"
        );

        let mut other_chunk = Chunk::new();
        other_chunk.emit(Op::Add, 1);
        assert_ne!(
            cache_set,
            vm.inline_cache_set_index_for_chunk(&other_chunk),
            "distinct chunks need distinct VM-local cache sets"
        );

        vm.set_inline_cache_entry_by_index(
            cache_set,
            slot_count,
            slot,
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Int,
                    hits: 7,
                    misses: 0,
                },
            },
        );

        let cached = vm
            .peek_adaptive_binary_cache_by_index(cache_set, slot)
            .expect("warmed cache entry");
        assert!(matches!(
            cached,
            (
                AdaptiveBinaryOp::Add,
                AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Int,
                    hits: 7,
                    misses: 0,
                }
            )
        ));
    }
}
