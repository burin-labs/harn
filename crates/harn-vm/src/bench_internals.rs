use std::rc::Rc;

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
                VmValue::Closure(Rc::new(nested_inner)),
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
        chunk: Rc::new(Chunk::new()),
        is_generator: false,
        is_stream: false,
        has_rest_param: false,
        has_runtime_type_checks: false,
    };
    VmClosure {
        func: Rc::new(func),
        env,
        source_dir: None,
        module_functions: None,
        module_state: None,
    }
}
