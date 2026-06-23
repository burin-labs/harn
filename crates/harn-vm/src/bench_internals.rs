use std::collections::HashMap;
use std::sync::Arc;

use crate::chunk::Op;
use crate::{Chunk, CompiledFunction, Vm, VmClosure, VmEnv, VmValue};

pub const VMENV_CAPTURE_COUNTS: [usize; 4] = [0, 5, 25, 100];

/// Bytecode-length presets for the inline-cache-slot lookup microbench.
/// The interesting axis is the *number of cacheable ops* in the chunk —
/// that's what controls how much node walking a `BTreeMap` lookup pays.
/// A 32-op chunk approximates a small predicate body; 128 a non-trivial
/// loop body; 512 a deep stdlib function. Beyond ~512 the lookup cost
/// plateaus per-op, but the cache miss frequency keeps growing.
pub const INLINE_CACHE_LOOKUP_COUNTS: [usize; 4] = [8, 32, 128, 512];

/// Microbench fixture for [`Chunk::inline_cache_slot`].
///
/// The lookup fires once per dispatch of every adaptive binary op,
/// every `Op::Call`, every `Op::MethodCall(Opt)`, and every
/// `Op::GetProperty(Opt)` — i.e. every hot opcode that benefits from
/// shape feedback. Even a small per-lookup win compounds across the
/// millions of dispatches a typical loop body fires.
///
/// The fixture emits `n` adjacent `Op::Add` instructions (each
/// registers an IC slot at emit time), records the resulting code
/// offsets, and on each invocation walks every offset through the
/// public lookup. That mirrors the dispatcher's call shape:
/// `op_offset = ip - 1; let slot = chunk.inline_cache_slot(op_offset);`
pub struct InlineCacheSlotLookupFixture {
    chunk: Chunk,
    offsets: Vec<usize>,
}

impl InlineCacheSlotLookupFixture {
    pub fn new(op_count: usize) -> Self {
        let mut chunk = Chunk::new();
        let mut offsets = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            offsets.push(chunk.code.len());
            chunk.emit(Op::Add, 1);
        }
        Self { chunk, offsets }
    }

    pub fn op_count(&self) -> usize {
        self.offsets.len()
    }

    /// One full sweep through every cacheable offset using the
    /// production flat-`Vec<u32>`-side-table path. Returns the sum of
    /// resolved slots so the optimizer cannot dead-code the loop.
    pub fn invoke(&self) -> usize {
        let mut acc = 0usize;
        for &offset in &self.offsets {
            if let Some(slot) = self.chunk.inline_cache_slot(offset) {
                acc = acc.wrapping_add(slot);
            }
        }
        acc
    }

    /// Control sweep using the pre-optimization `BTreeMap<usize, usize>`
    /// lookup. Same shape and same accumulator as [`Self::invoke`] so
    /// the criterion bench can A/B the two paths within a single binary.
    /// Production code MUST keep going through `inline_cache_slot`.
    pub fn invoke_btreemap_control(&self) -> usize {
        let mut acc = 0usize;
        for &offset in &self.offsets {
            if let Some(slot) = self.chunk.inline_cache_slot_via_btreemap_for_bench(offset) {
                acc = acc.wrapping_add(slot);
            }
        }
        acc
    }
}

struct VmInlineCacheReadFixture {
    vm: Vm,
    cache_set: usize,
    cache_id: u64,
    hash_control: HashMap<u64, Vec<crate::chunk::InlineCacheEntry>>,
    slots: Vec<usize>,
}

impl VmInlineCacheReadFixture {
    fn new(
        chunk: &Chunk,
        slots: Vec<usize>,
        mut entry_for_slot: impl FnMut(usize) -> crate::chunk::InlineCacheEntry,
    ) -> Self {
        let slot_count = chunk.inline_cache_slot_count();
        let cache_id = chunk.cache_id();
        let mut vm = Vm::new();
        let cache_set = vm.inline_cache_set_index_for_chunk(chunk);
        let mut hash_entries = vec![crate::chunk::InlineCacheEntry::Empty; slot_count];
        for &slot in &slots {
            let entry = entry_for_slot(slot);
            vm.set_inline_cache_entry_by_index(cache_set, slot_count, slot, entry.clone());
            hash_entries[slot] = entry;
        }

        Self {
            vm,
            cache_set,
            cache_id,
            hash_control: HashMap::from([(cache_id, hash_entries)]),
            slots,
        }
    }

    fn op_count(&self) -> usize {
        self.slots.len()
    }

    #[inline]
    fn hash_entry(&self, slot: usize) -> Option<crate::chunk::InlineCacheEntry> {
        self.hash_control
            .get(&self.cache_id)
            .and_then(|entries| entries.get(slot))
            .cloned()
    }
}

/// Bytecode-length presets for the adaptive-binary-cache read microbench.
/// The fixture walks N adjacent `Op::Add` slots, exercising the same
/// cache-read shape that `execute_adaptive_binary` issues on every
/// dispatch. The interesting axis is "how many cacheable ops per sweep"
/// — that's what the dispatch loop pays per iteration of a tight
/// arithmetic body.
pub const ADAPTIVE_BINARY_CACHE_READ_COUNTS: [usize; 4] = [8, 32, 128, 512];

/// Microbench fixture for the adaptive-binary inline-cache read path.
///
/// The control path performs the old per-dispatch hash lookup and clones the
/// wrapping `InlineCacheEntry` enum — a 24-32B memcpy that the variant-checking
/// match in `try_specialized_binary` destructures and throws away. The
/// production path uses the frame-local cache-set index and returns just the
/// `(AdaptiveBinaryOp, AdaptiveBinaryState)` pair by value (both `Copy`).
///
/// The fixture pre-warms every slot to the Specialized state (the steady
/// state of a hot loop) so the bench measures the read overhead with no
/// per-iteration state transitions. The accumulator sums the cached
/// `hits` counter so the optimizer cannot dead-code the loop.
pub struct AdaptiveBinaryCacheReadFixture {
    cache: VmInlineCacheReadFixture,
}

impl AdaptiveBinaryCacheReadFixture {
    pub fn new(op_count: usize) -> Self {
        use crate::chunk::{AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, InlineCacheEntry};
        let mut chunk = Chunk::new();
        let mut offsets = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            offsets.push(chunk.code.len());
            chunk.emit(Op::Add, 1);
        }
        let mut slots = Vec::with_capacity(op_count);
        for &offset in &offsets {
            let slot = chunk
                .inline_cache_slot(offset)
                .expect("Op::Add registers an inline-cache slot at emit time");
            slots.push(slot);
        }
        let cache = VmInlineCacheReadFixture::new(&chunk, slots, |_slot| {
            // Pre-warm to Specialized{Int}, which is the steady state of a hot
            // loop after `ADAPTIVE_QUICKEN_THRESHOLD` Int-Int Adds.
            InlineCacheEntry::AdaptiveBinary {
                op: AdaptiveBinaryOp::Add,
                state: AdaptiveBinaryState::Specialized {
                    shape: BinaryShape::Int,
                    hits: 1_000,
                    misses: 0,
                },
            }
        });
        Self { cache }
    }

    pub fn op_count(&self) -> usize {
        self.cache.op_count()
    }

    /// Sweep all slots via the production frame-indexed peek path. Returns
    /// the sum of observed `hits` counters so the optimizer cannot dead-code
    /// the loop.
    pub fn invoke_peek(&self) -> u64 {
        use crate::chunk::AdaptiveBinaryState;
        let mut acc = 0u64;
        for &slot in &self.cache.slots {
            if let Some((_op, state)) = self
                .cache
                .vm
                .peek_adaptive_binary_cache_by_index(self.cache.cache_set, slot)
            {
                let hits = match state {
                    AdaptiveBinaryState::Specialized { hits, .. } => hits,
                    AdaptiveBinaryState::Warmup { hits, .. } => hits as u64,
                };
                acc = acc.wrapping_add(hits);
            }
        }
        acc
    }

    /// Control sweep using the pre-optimization per-dispatch hash lookup plus
    /// full `InlineCacheEntry` clone. Same accumulator shape so Criterion can
    /// A/B the two paths inside a single binary.
    pub fn invoke_clone_control(&self) -> u64 {
        use crate::chunk::{AdaptiveBinaryState, InlineCacheEntry};
        let mut acc = 0u64;
        for &slot in &self.cache.slots {
            let entry = self.cache.hash_entry(slot);
            if let Some(InlineCacheEntry::AdaptiveBinary { state, .. }) = entry {
                let hits = match state {
                    AdaptiveBinaryState::Specialized { hits, .. } => hits,
                    AdaptiveBinaryState::Warmup { hits, .. } => hits as u64,
                };
                acc = acc.wrapping_add(hits);
            }
        }
        acc
    }
}

/// Bytecode-length presets for the method-cache read microbench. The
/// fixture sweeps N adjacent `Op::MethodCall` slots, exercising the
/// same cache-read shape that `execute_method_call(_sync|_spread)`
/// issues on every dispatch. N spans a small predicate body
/// through a deep stdlib pipeline.
pub const METHOD_CACHE_READ_COUNTS: [usize; 4] = [8, 32, 128, 512];

/// Microbench fixture for the method inline-cache read path.
///
/// The control path performs the old per-dispatch hash lookup and clones the
/// wrapping `InlineCacheEntry` enum — a 32-48B memcpy that the variant-checking
/// `let-else` in `try_cached_method` destructures and throws away. The
/// production path uses the frame-local cache-set index and returns just the
/// `(name_idx, argc, target)` triple by value (all three are `Copy`).
///
/// The fixture pre-warms every slot to a `Method` entry (the steady
/// state of a hot pipeline like `xs.contains(...).filter(...).count()`)
/// so the bench measures the read overhead with no per-iteration state
/// transitions. The accumulator sums the cached `argc` so the optimizer
/// cannot dead-code the loop.
pub struct MethodCacheReadFixture {
    cache: VmInlineCacheReadFixture,
}

impl MethodCacheReadFixture {
    pub fn new(op_count: usize) -> Self {
        use crate::chunk::{InlineCacheEntry, MethodCacheTarget};
        let mut chunk = Chunk::new();
        let mut offsets = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            offsets.push(chunk.code.len());
            chunk.emit_method_call(0, 1, 1);
        }
        let mut slots = Vec::with_capacity(op_count);
        for &offset in &offsets {
            let slot = chunk
                .inline_cache_slot(offset)
                .expect("Op::MethodCall registers an inline-cache slot at emit time");
            slots.push(slot);
        }
        let cache = VmInlineCacheReadFixture::new(&chunk, slots, |_slot| {
            // Pre-warm to a Method entry with `ListContains` — a 1-arg
            // method-call shape that flows through every method-call
            // dispatcher (`execute_method_call`, `execute_method_call_sync`,
            // `execute_method_call_spread`).
            InlineCacheEntry::Method {
                name_idx: 0,
                argc: 1,
                target: MethodCacheTarget::ListContains,
            }
        });
        Self { cache }
    }

    pub fn op_count(&self) -> usize {
        self.cache.op_count()
    }

    /// Sweep all slots via the production frame-indexed peek path. Returns
    /// the sum of observed `argc` values so the optimizer cannot dead-code
    /// the loop.
    pub fn invoke_peek(&self) -> usize {
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            if let Some((_name_idx, argc, _target)) = self
                .cache
                .vm
                .peek_method_cache_by_index(self.cache.cache_set, slot)
            {
                acc = acc.wrapping_add(argc);
            }
        }
        acc
    }

    /// Control sweep using the pre-optimization per-dispatch hash lookup plus
    /// full `InlineCacheEntry` clone. Same accumulator shape so Criterion can
    /// A/B the two paths inside a single binary.
    pub fn invoke_clone_control(&self) -> usize {
        use crate::chunk::InlineCacheEntry;
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            let entry = self.cache.hash_entry(slot);
            if let Some(InlineCacheEntry::Method { argc, .. }) = entry {
                acc = acc.wrapping_add(argc);
            }
        }
        acc
    }
}

/// Bytecode-length presets for the property-cache read microbench. Sweeps
/// N adjacent `Op::GetProperty` slots, exercising the same cache-read
/// shape that `execute_get_property` issues on every dispatch.
pub const PROPERTY_CACHE_READ_COUNTS: [usize; 4] = [8, 32, 128, 512];

/// Microbench fixture for the property inline-cache read path.
///
/// The control path performs the old per-dispatch hash lookup and clones the
/// wrapping `InlineCacheEntry` enum — a 32-48B memcpy (the wrapping enum is
/// padded to the largest variant, `DirectCall`) that the variant-checking
/// `let-else` in `try_cached_property` destructures and throws away. The
/// production path uses the frame-local cache-set index and returns just the
/// `Property` payload (`u16 + PropertyCacheTarget`).
///
/// The fixture pre-warms every slot to a unit `PropertyCacheTarget`
/// (`ListCount`) — the hottest steady state for any property-access
/// pipeline (`.count` on collections, `.first` / `.last`, etc.).
pub struct PropertyCacheReadFixture {
    cache: VmInlineCacheReadFixture,
}

impl PropertyCacheReadFixture {
    pub fn new(op_count: usize) -> Self {
        use crate::chunk::{InlineCacheEntry, PropertyCacheTarget};
        let mut chunk = Chunk::new();
        let mut offsets = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            offsets.push(chunk.code.len());
            chunk.emit_u16(Op::GetProperty, 0, 1);
        }
        let mut slots = Vec::with_capacity(op_count);
        for &offset in &offsets {
            let slot = chunk
                .inline_cache_slot(offset)
                .expect("Op::GetProperty registers an inline-cache slot at emit time");
            slots.push(slot);
        }
        let cache =
            VmInlineCacheReadFixture::new(&chunk, slots, |_slot| InlineCacheEntry::Property {
                name_idx: 7,
                target: PropertyCacheTarget::ListCount,
            });
        Self { cache }
    }

    pub fn op_count(&self) -> usize {
        self.cache.op_count()
    }

    /// Sweep all slots via the production frame-indexed peek path. Returns
    /// the sum of observed `name_idx` values so the optimizer cannot dead-code
    /// the loop.
    pub fn invoke_peek(&self) -> usize {
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            if let Some((name_idx, _target)) = self
                .cache
                .vm
                .peek_property_cache_by_index(self.cache.cache_set, slot)
            {
                acc = acc.wrapping_add(name_idx as usize);
            }
        }
        acc
    }

    /// Control sweep using the pre-optimization per-dispatch hash lookup plus
    /// full `InlineCacheEntry` clone. Same accumulator shape so Criterion can
    /// A/B the two paths inside a single binary.
    pub fn invoke_clone_control(&self) -> usize {
        use crate::chunk::InlineCacheEntry;
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            let entry = self.cache.hash_entry(slot);
            if let Some(InlineCacheEntry::Property { name_idx, .. }) = entry {
                acc = acc.wrapping_add(name_idx as usize);
            }
        }
        acc
    }
}

/// Bytecode-length presets for the direct-call-state read microbench.
/// Sweeps N adjacent `Op::Call` slots, exercising the same cache-read
/// shape that `execute_call_sync` and `execute_call_builtin_sync` issue
/// on every dispatch.
pub const DIRECT_CALL_STATE_READ_COUNTS: [usize; 4] = [8, 32, 128, 512];

/// Microbench fixture for the direct-call inline-cache read path.
///
/// The control path performs the old per-dispatch hash lookup and clones the
/// wrapping `InlineCacheEntry::DirectCall { state: DirectCallState }`. The
/// production path uses the frame-local cache-set index and returns just the
/// inner `DirectCallState`, avoiding the outer enum copy and variant check in
/// `try_cached_direct_call`.
///
/// Pre-warms every slot to `Specialized { argc: 1, hits: 1000, misses: 0,
/// target: Arc<VmClosure> }`, the steady state of any hot
/// `x.map(predicate)`-style direct-call call site.
pub struct DirectCallStateReadFixture {
    cache: VmInlineCacheReadFixture,
}

impl DirectCallStateReadFixture {
    pub fn new(op_count: usize) -> Self {
        use crate::chunk::{DirectCallState, DirectCallTarget, InlineCacheEntry};
        let target_closure = synthetic_direct_call_closure();
        let mut chunk = Chunk::new();
        let mut offsets = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            offsets.push(chunk.code.len());
            chunk.emit_u8(Op::Call, 1, 1);
        }
        let mut slots = Vec::with_capacity(op_count);
        for &offset in &offsets {
            let slot = chunk
                .inline_cache_slot(offset)
                .expect("Op::Call registers an inline-cache slot at emit time");
            slots.push(slot);
        }
        let cache =
            VmInlineCacheReadFixture::new(&chunk, slots, |_slot| InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized {
                    argc: 1,
                    target: DirectCallTarget::Closure(Arc::clone(&target_closure)),
                    hits: 1_000,
                    misses: 0,
                },
            });
        Self { cache }
    }

    pub fn op_count(&self) -> usize {
        self.cache.op_count()
    }

    /// Sweep all slots via the production frame-indexed peek path. Returns
    /// the sum of observed `argc` values so the optimizer cannot dead-code
    /// the loop.
    pub fn invoke_peek(&self) -> usize {
        use crate::chunk::DirectCallState;
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            if let Some(DirectCallState::Specialized { argc, .. }) = self
                .cache
                .vm
                .peek_direct_call_state_by_index(self.cache.cache_set, slot)
            {
                acc = acc.wrapping_add(argc);
            }
        }
        acc
    }

    /// Control sweep using the pre-optimization per-dispatch hash lookup plus
    /// full `InlineCacheEntry` clone. Same accumulator shape.
    pub fn invoke_clone_control(&self) -> usize {
        use crate::chunk::{DirectCallState, InlineCacheEntry};
        let mut acc = 0usize;
        for &slot in &self.cache.slots {
            let entry = self.cache.hash_entry(slot);
            if let Some(InlineCacheEntry::DirectCall {
                state: DirectCallState::Specialized { argc, .. },
            }) = entry
            {
                acc = acc.wrapping_add(argc);
            }
        }
        acc
    }
}

fn synthetic_direct_call_closure() -> Arc<VmClosure> {
    let func = CompiledFunction {
        name: "synthetic_direct_call_target".to_string(),
        type_params: Vec::new(),
        nominal_type_names: Vec::new(),
        params: Vec::new(),
        default_start: None,
        chunk: Arc::new(Chunk::new()),
        is_generator: false,
        is_stream: false,
        has_rest_param: false,
        has_runtime_type_checks: false,
    };
    Arc::new(VmClosure {
        func: Arc::new(func),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
    })
}

pub struct NonModuleClosureCallFixture {
    capture_count: usize,
    last_capture_name: Option<String>,
    caller_env: VmEnv,
    closure: VmClosure,
}

impl NonModuleClosureCallFixture {
    pub fn new(capture_count: usize) -> Self {
        let nested_inner = synthetic_closure("nested_inner", VmEnv::new());

        let mut caller_env = VmEnv::new();
        caller_env
            .define(
                "nested_inner",
                VmValue::Closure(Arc::new(nested_inner)),
                false,
            )
            .expect("synthetic caller closure binding should be valid");

        let mut closure_env = VmEnv::new();
        for index in 0..capture_count {
            closure_env
                .define(
                    &format!("captured_{index:03}"),
                    VmValue::Int(index as i64),
                    false,
                )
                .expect("synthetic captured binding should be valid");
        }

        let closure = synthetic_closure(&format!("capture_{capture_count:03}"), closure_env);
        Self {
            capture_count,
            last_capture_name: capture_count
                .checked_sub(1)
                .map(|index| format!("captured_{index:03}")),
            caller_env,
            closure,
        }
    }

    pub fn capture_count(&self) -> usize {
        self.capture_count
    }

    pub fn invoke(&self) -> usize {
        let env = Vm::closure_call_env(&self.caller_env, &self.closure);
        let mut score = env.scope_depth();
        if let Some(name) = self.last_capture_name.as_deref() {
            if let Some(VmValue::Int(value)) = env.get(name) {
                score += value as usize;
            }
        }
        if matches!(env.get("nested_inner"), Some(VmValue::Closure(_))) {
            score += 1;
        }
        score
    }
}

fn synthetic_closure(name: &str, env: VmEnv) -> VmClosure {
    let func = CompiledFunction {
        name: name.to_string(),
        type_params: Vec::new(),
        nominal_type_names: Vec::new(),
        params: Vec::new(),
        default_start: None,
        chunk: Arc::new(Chunk::new()),
        is_generator: false,
        is_stream: false,
        has_rest_param: false,
        has_runtime_type_checks: false,
    };
    VmClosure {
        func: Arc::new(func),
        env,
        source_dir: None,
        module_functions: None,
        module_state: None,
    }
}
