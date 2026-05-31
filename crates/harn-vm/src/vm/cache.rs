use crate::chunk::{
    AdaptiveBinaryOp, AdaptiveBinaryState, ChunkRef, DirectCallState, InlineCacheEntry,
    MethodCacheTarget, PropertyCacheTarget,
};

use super::Vm;

impl Vm {
    fn inline_cache_entries_mut(&mut self, chunk: &ChunkRef) -> &mut Vec<InlineCacheEntry> {
        let slot_count = chunk.inline_cache_slot_count();
        let entries = self
            .inline_caches
            .entry(chunk.cache_id())
            .or_insert_with(|| vec![InlineCacheEntry::Empty; slot_count]);
        if entries.len() < slot_count {
            entries.resize(slot_count, InlineCacheEntry::Empty);
        }
        entries
    }

    #[inline]
    pub(crate) fn peek_adaptive_binary_cache(
        &mut self,
        chunk: &ChunkRef,
        slot: usize,
    ) -> Option<(AdaptiveBinaryOp, AdaptiveBinaryState)> {
        match self.inline_cache_entries_mut(chunk).get(slot)? {
            &InlineCacheEntry::AdaptiveBinary { op, state } => Some((op, state)),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_method_cache(
        &mut self,
        chunk: &ChunkRef,
        slot: usize,
    ) -> Option<(u16, usize, MethodCacheTarget)> {
        match self.inline_cache_entries_mut(chunk).get(slot)? {
            &InlineCacheEntry::Method {
                name_idx,
                argc,
                target,
            } => Some((name_idx, argc, target)),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_property_cache(
        &mut self,
        chunk: &ChunkRef,
        slot: usize,
    ) -> Option<(u16, PropertyCacheTarget)> {
        match self.inline_cache_entries_mut(chunk).get(slot)? {
            InlineCacheEntry::Property { name_idx, target } => Some((*name_idx, target.clone())),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn peek_direct_call_state(
        &mut self,
        chunk: &ChunkRef,
        slot: usize,
    ) -> Option<DirectCallState> {
        match self.inline_cache_entries_mut(chunk).get(slot)? {
            InlineCacheEntry::DirectCall { state } => Some(state.clone()),
            _ => None,
        }
    }

    pub(crate) fn set_inline_cache_entry(
        &mut self,
        chunk: &ChunkRef,
        slot: usize,
        entry: InlineCacheEntry,
    ) {
        let entries = self.inline_cache_entries_mut(chunk);
        if let Some(existing) = entries.get_mut(slot) {
            *existing = entry;
        }
    }

    #[cfg(test)]
    pub(crate) fn inline_cache_entries_for_chunk(
        &self,
        chunk: &crate::chunk::Chunk,
    ) -> Vec<InlineCacheEntry> {
        self.inline_caches
            .get(&chunk.cache_id())
            .cloned()
            .unwrap_or_else(|| vec![InlineCacheEntry::Empty; chunk.inline_cache_slot_count()])
    }
}
