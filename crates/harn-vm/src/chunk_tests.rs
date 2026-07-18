use std::sync::Arc;

use super::{
    Chunk, CompiledFunction, Constant, DirectCallState, DirectCallTarget, InlineCacheEntry,
    MethodCacheTarget, Op, ParamSlot, PropertyCacheTarget,
};
use crate::BuiltinId;

#[test]
fn op_from_byte_matches_repr_order() {
    for (byte, op) in Op::ALL.iter().copied().enumerate() {
        assert_eq!(byte as u8, op as u8);
        assert_eq!(Op::from_byte(byte as u8), Some(op));
    }
    assert_eq!(Op::from_byte(Op::ALL.len() as u8), None);
    assert_eq!(Op::COUNT, Op::ALL.len());
}

#[test]
fn disassemble_covers_every_opcode_variant() {
    // The macro-generated `disassemble_op` match is exhaustive on
    // `Op`, so this is a compile-time guarantee. The runtime check
    // pins that no helper falls through to `UNKNOWN(...)` for a
    // valid opcode byte — catching any future macro refactor that
    // silently drops a helper arm. Each opcode is exercised in
    // isolation against a hand-built chunk so the test logic does
    // not depend on operand sizes (and so a single short opcode
    // does not bleed into reading trailing padding as a follow-on
    // opcode in the chunk-level loop).
    for op in Op::ALL.iter().copied() {
        let mut chunk = Chunk::new();
        chunk.add_constant(super::Constant::String("__probe__".to_string()));
        // Pad to the worst-case operand width (CallBuiltin: u64 +
        // u16 + u8 = 11 bytes) so any helper has well-formed bytes
        // to consume regardless of its layout.
        for _ in 0..16 {
            chunk.code.push(0);
        }
        let mut ip: usize = 0;
        let mut out = String::new();
        chunk.disassemble_op(op, &mut ip, &mut out);
        assert!(
            !out.contains("UNKNOWN"),
            "disasm emitted UNKNOWN for {op:?}: {out}",
        );
        assert!(!out.is_empty(), "disasm produced no output for {op:?}");
    }
}

// --- references_outer_names tracking ---
//
// Drives the compile-time guard used in `Vm::closure_call_env`
// and `Vm::closure_call_env_for_current_frame` to skip the
// per-invocation caller-scope late-bind walks. Coverage parity
// matters because false negatives would regress recursive /
// mutually-recursive fns.

#[test]
fn empty_chunk_does_not_reference_outer_names() {
    let chunk = Chunk::new();
    assert!(!chunk.references_outer_names);
}

#[test]
fn arithmetic_only_chunk_does_not_reference_outer_names() {
    // The hot `.map(x -> x * 2)` / `.filter(x -> x % 2 == 0)`
    // shape: pure stack/arithmetic ops and slot locals, no env
    // reads. Must NOT flag — that's the whole point of the
    // optimization.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetLocalSlot, 0, 1);
    chunk.emit_u16(Op::Constant, 0, 1);
    chunk.emit(Op::MulInt, 1);
    chunk.emit(Op::Pop, 1);
    chunk.emit(Op::Return, 1);
    assert!(!chunk.references_outer_names);
}

#[test]
fn slot_only_chunk_does_not_reference_outer_names() {
    // Compiler-resolved locals never need env-based late-bind.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::DefLocalSlot, 0, 1);
    chunk.emit_u16(Op::GetLocalSlot, 0, 1);
    chunk.emit_u16(Op::SetLocalSlot, 0, 1);
    assert!(!chunk.references_outer_names);
}

#[test]
fn get_var_flags_outer_name_reference() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetVar, 0, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn set_var_flags_outer_name_reference() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::SetVar, 0, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn check_type_flags_outer_name_reference() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::CheckType, 0, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn call_builtin_flags_outer_name_reference() {
    let mut chunk = Chunk::new();
    chunk.emit_call_builtin(BuiltinId::from_name("any_name"), 0, 1, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn call_builtin_spread_flags_outer_name_reference() {
    let mut chunk = Chunk::new();
    chunk.emit_call_builtin_spread(BuiltinId::from_name("any_name"), 0, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn tail_call_flags_outer_name_reference() {
    // `return fn_name(...)` compiles to Constant + TailCall —
    // TailCall does a runtime name lookup, so it has to flag.
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::TailCall, 1, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn call_flags_outer_name_reference() {
    // Op::Call can receive a String callee from the stack (the
    // by-name dispatch shape), so it has to flag too.
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 1, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn pipe_flags_outer_name_reference() {
    // `x |> name` resolves `name` through env when the value on
    // the stack is a String / BuiltinRef.
    let mut chunk = Chunk::new();
    chunk.emit(Op::Pipe, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn method_call_does_not_flag_outer_name_reference() {
    // Method receivers come off the operand stack, not the env;
    // emitting MethodCall alone must not force the walk.
    let mut chunk = Chunk::new();
    chunk.emit_method_call(0, 1, 1);
    chunk.emit_method_call_opt(0, 1, 1);
    assert!(!chunk.references_outer_names);
}

#[test]
fn jump_and_control_flow_do_not_flag_outer_name_reference() {
    // Jumps, returns, pops — control flow stays inside the
    // frame and never touches env lookups.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::Constant, 0, 1);
    chunk.emit(Op::JumpIfFalse, 1);
    chunk.emit(Op::Jump, 1);
    chunk.emit(Op::Return, 1);
    chunk.emit(Op::Pop, 1);
    assert!(!chunk.references_outer_names);
}

#[test]
fn references_outer_names_is_monotonic() {
    // Once flagged, subsequent non-flagging emits must not
    // clear the bit — flags are sticky.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetVar, 0, 1);
    assert!(chunk.references_outer_names);
    chunk.emit_u16(Op::GetLocalSlot, 0, 1);
    chunk.emit(Op::MulInt, 1);
    assert!(chunk.references_outer_names);
}

#[test]
fn freeze_thaw_round_trips_references_outer_names() {
    // Bytecode-cache hits must observe the same flag as a
    // fresh compile — otherwise the first call after a cache
    // hit would either over- or under-skip the walk.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetVar, 0, 1);
    assert!(chunk.references_outer_names);
    let frozen = chunk.freeze_for_cache();
    let thawed = Chunk::from_cached(frozen);
    assert!(thawed.references_outer_names);

    let plain = Chunk::new();
    assert!(!plain.references_outer_names);
    let frozen_plain = plain.freeze_for_cache();
    let thawed_plain = Chunk::from_cached(frozen_plain);
    assert!(!thawed_plain.references_outer_names);
}

#[test]
fn cached_chunk_hydration_moves_owned_storage() {
    let mut nested_chunk = Chunk::new();
    nested_chunk.emit(Op::Return, 7);
    let nested = CompiledFunction {
        name: "nested".to_string(),
        type_params: vec!["T".to_string()],
        nominal_type_names: vec!["Widget".to_string()],
        params: vec![ParamSlot {
            name: "value".to_string(),
            type_expr: None,
            runtime_guard: None,
            has_default: false,
        }],
        default_start: None,
        chunk: Arc::new(nested_chunk),
        is_generator: false,
        is_stream: false,
        has_rest_param: false,
        has_runtime_type_checks: false,
    };

    let mut chunk = Chunk::new();
    chunk.source_file = Some("owned.harn".to_string());
    chunk.add_constant(Constant::String("payload".to_string()));
    chunk.add_local_slot("local".to_string(), false, 0);
    chunk.emit(Op::Return, 11);
    chunk.functions.push(Arc::new(nested));

    let cached = chunk.freeze_for_cache();
    let code = cached.code.as_ptr();
    let constants = cached.constants.as_ptr();
    let lines = cached.lines.as_ptr();
    let columns = cached.columns.as_ptr();
    let source_file = cached.source_file.as_ref().unwrap().as_ptr();
    let local_slots = cached.local_slots.as_ptr();
    let function_name = cached.functions[0].name.as_ptr();
    let type_params = cached.functions[0].type_params.as_ptr();
    let nominal_type_names = cached.functions[0].nominal_type_names.as_ptr();
    let param_name = cached.functions[0].params[0].name.as_ptr();
    let nested_code = cached.functions[0].chunk.code.as_ptr();

    let hydrated = Chunk::from_cached(cached);

    assert_eq!(hydrated.code.as_ptr(), code);
    assert_eq!(hydrated.constants.as_ptr(), constants);
    assert_eq!(hydrated.lines.as_ptr(), lines);
    assert_eq!(hydrated.columns.as_ptr(), columns);
    assert_eq!(hydrated.source_file.as_ref().unwrap().as_ptr(), source_file);
    assert_eq!(hydrated.local_slots.as_ptr(), local_slots);
    assert_eq!(hydrated.functions[0].name.as_ptr(), function_name);
    assert_eq!(hydrated.functions[0].type_params.as_ptr(), type_params);
    assert_eq!(
        hydrated.functions[0].nominal_type_names.as_ptr(),
        nominal_type_names
    );
    assert_eq!(hydrated.functions[0].params[0].name.as_ptr(), param_name);
    assert_eq!(hydrated.functions[0].chunk.code.as_ptr(), nested_code);
}

// --- inline_cache_slot flat-index parity ---
//
// Slot lookups fire on every dispatch of an adaptive binary op
// (Add/Sub/Mul/Div/Mod/Eq/Neq/Less/Greater/LessEq/GreaterEq),
// every `Op::Call`, every `Op::MethodCall(Opt)`, and every
// `Op::GetProperty(Opt)`. The flat `Vec<u32>` index has to stay
// perfectly in sync with the serialization-stable BTreeMap or
// a cached call site would either skip its inline cache (slow
// path with no learning) or read a stale slot (silently
// mis-specialized arithmetic). These tests pin the contract.

#[test]
fn inline_cache_slot_returns_none_for_non_cacheable_offsets() {
    // GetLocalSlot is a sync-fast-path opcode with no inline
    // cache; the index must report no slot.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetLocalSlot, 0, 1);
    chunk.emit(Op::Pop, 1);
    chunk.emit(Op::Return, 1);
    assert!(chunk.inline_cache_slot(0).is_none());
    assert!(chunk.inline_cache_slot(3).is_none());
    assert!(chunk.inline_cache_slot(4).is_none());
}

#[test]
fn inline_cache_slot_registered_for_adaptive_binary_op() {
    // Pure-arithmetic ops use the adaptive-binary IC for shape
    // specialization. The slot has to be 0 because the chunk is
    // otherwise empty.
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    assert_eq!(chunk.inline_cache_slot(0), Some(0));
}

#[test]
fn inline_cache_slot_distinct_for_sequential_adaptive_binary_ops() {
    // Three back-to-back Adds must get three distinct slots so
    // each instruction's shape feedback evolves independently
    // (otherwise the same call site would clobber a neighbor's
    // learning every dispatch).
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    chunk.emit(Op::Sub, 1);
    chunk.emit(Op::Mul, 1);
    let s0 = chunk.inline_cache_slot(0).expect("Add slot");
    let s1 = chunk.inline_cache_slot(1).expect("Sub slot");
    let s2 = chunk.inline_cache_slot(2).expect("Mul slot");
    assert_ne!(s0, s1);
    assert_ne!(s1, s2);
    assert_ne!(s0, s2);
}

#[test]
fn inline_cache_slot_returns_none_for_out_of_bounds_offset() {
    // The dispatcher derives `op_offset` from `ip - 1`; an
    // out-of-bounds query must return None rather than panic.
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    assert!(chunk.inline_cache_slot(usize::MAX).is_none());
    assert!(chunk.inline_cache_slot(chunk.code.len()).is_none());
    assert!(chunk.inline_cache_slot(chunk.code.len() + 16).is_none());
}

#[test]
fn inline_cache_slot_for_get_property_and_method_call() {
    // GetProperty(Opt) and MethodCall(Opt) both register an IC
    // slot at emit time — adaptive method-call dispatch and
    // monomorphic property-cache learning depend on it.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetProperty, 0, 1); // offset 0..3
    chunk.emit_method_call(0, 1, 1); // offset 3..7
    chunk.emit_method_call_opt(0, 1, 1); // offset 7..11
    chunk.emit_u16(Op::GetPropertyOpt, 0, 1); // offset 11..14
    assert!(chunk.inline_cache_slot(0).is_some(), "GetProperty");
    assert!(chunk.inline_cache_slot(3).is_some(), "MethodCall");
    assert!(chunk.inline_cache_slot(7).is_some(), "MethodCallOpt");
    assert!(chunk.inline_cache_slot(11).is_some(), "GetPropertyOpt");
}

#[test]
fn inline_cache_slot_for_call_and_call_builtin() {
    // Both `Op::Call` (closure / by-name callee) and
    // `emit_call_builtin` register IC slots. The latter is the
    // adaptive-call fast path used for every direct user-fn
    // invocation.
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 1, 1); // offset 0..2
    let call_builtin_offset = chunk.code.len();
    chunk.emit_call_builtin(BuiltinId::from_name("any"), 0, 1, 1);
    assert!(chunk.inline_cache_slot(0).is_some(), "Op::Call IC slot");
    assert!(
        chunk.inline_cache_slot(call_builtin_offset).is_some(),
        "Op::CallBuiltin IC slot"
    );
}

#[test]
fn inline_cache_slot_register_is_idempotent_for_same_offset() {
    // The compile path uses `BTreeMap::contains_key` to dedup
    // re-registration at the same offset (eg. when a helper
    // re-emits into a still-live position). The flat index has
    // to honor the same semantics — never silently overwriting
    // an existing slot with a fresh one.
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    let slot_before = chunk.inline_cache_slot(0).expect("first registration");
    // Manually re-register the same offset to confirm dedup.
    chunk.register_inline_cache(0);
    let slot_after = chunk.inline_cache_slot(0).expect("re-registration");
    assert_eq!(slot_before, slot_after);
}

#[test]
fn inline_cache_index_round_trips_through_cached_chunk() {
    // The cache freeze drops the flat index (it's derived from
    // the BTreeMap that *is* serialized). On thaw, the flat
    // index must be rebuilt so the first hot dispatch of a
    // cached chunk doesn't fall off the IC-slot cliff (which
    // would silently disable shape specialization until the
    // chunk is recompiled from source).
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetLocalSlot, 0, 1);
    chunk.emit_u16(Op::Constant, 0, 1);
    chunk.emit(Op::Add, 1);
    chunk.emit(Op::Sub, 1);
    chunk.emit_method_call(0, 1, 1);
    chunk.emit_u8(Op::Call, 1, 1);
    let live_slots: Vec<(usize, Option<usize>)> = (0..chunk.code.len())
        .map(|o| (o, chunk.inline_cache_slot(o)))
        .collect();
    let frozen = chunk.freeze_for_cache();
    let thawed = Chunk::from_cached(frozen);
    let thawed_slots: Vec<(usize, Option<usize>)> = (0..thawed.code.len())
        .map(|o| (o, thawed.inline_cache_slot(o)))
        .collect();
    assert_eq!(live_slots, thawed_slots);
}

#[test]
fn inline_cache_index_agrees_with_btreemap_view() {
    // Authoritative parity check: for every code offset, the
    // flat-index `inline_cache_slot` must return exactly what
    // the underlying BTreeMap would (mod the `Option` boxing).
    // Catches any future emit path that grows `inline_cache_slots`
    // without going through `register_inline_cache`.
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    chunk.emit_u16(Op::GetVar, 0, 1);
    chunk.emit(Op::LessInt, 1);
    chunk.emit_u8(Op::Call, 2, 1);
    chunk.emit(Op::Equal, 1);
    chunk.emit_u16(Op::GetProperty, 0, 1);
    chunk.emit_method_call_opt(0, 0, 1);
    for offset in 0..chunk.code.len() {
        let from_map = chunk.inline_cache_slots.get(&offset).copied();
        let from_index = chunk.inline_cache_slot(offset);
        assert_eq!(from_index, from_map, "parity broken at offset {offset}");
    }
}

// --- peek_adaptive_binary_cache contract ---
//
// The peek replaces the per-dispatch `InlineCacheEntry::clone` on the
// hottest opcode class (Add / Sub / Mul / Div / Mod / Eq / Neq /
// Less / Greater / LessEq / GreaterEq). It must return None for
// unrelated IC variants — silently mis-extracting a `Property` /
// `DirectCall` / `Method` slot as `AdaptiveBinary` would feed
// garbage into `try_specialized_binary` and either spec-mis-fire or
// crash. These tests pin the variant gate.

#[test]
fn peek_adaptive_binary_returns_none_for_empty_slot() {
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    let slot = chunk.inline_cache_slot(0).expect("Add registers a slot");
    // Default state of a freshly-emitted slot is Empty.
    assert!(chunk.peek_adaptive_binary_cache(slot).is_none());
}

#[test]
fn peek_adaptive_binary_returns_op_and_state_after_warmup() {
    use super::{AdaptiveBinaryOp, AdaptiveBinaryState, BinaryShape, InlineCacheEntry};
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    let slot = chunk.inline_cache_slot(0).expect("Add registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::AdaptiveBinary {
            op: AdaptiveBinaryOp::Add,
            state: AdaptiveBinaryState::Warmup {
                shape: BinaryShape::Int,
                hits: 2,
            },
        },
    );
    let (op, state) = chunk
        .peek_adaptive_binary_cache(slot)
        .expect("warmed slot peek");
    assert_eq!(op, AdaptiveBinaryOp::Add);
    assert!(matches!(
        state,
        AdaptiveBinaryState::Warmup {
            shape: BinaryShape::Int,
            hits: 2
        }
    ));
}

#[test]
fn peek_adaptive_binary_returns_none_for_non_binary_variants() {
    // The cache slot may legitimately hold a `Property`, `Method`,
    // or `DirectCall` entry (eg. a Property slot at the offset
    // sequence happens to alias an Add slot during a code rewrite —
    // currently this cannot happen, but the peek must defensively
    // refuse non-AdaptiveBinary variants regardless).
    use super::{InlineCacheEntry, PropertyCacheTarget};
    let mut chunk = Chunk::new();
    chunk.emit(Op::Add, 1);
    let slot = chunk.inline_cache_slot(0).expect("Add registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Property {
            name_idx: 0,
            target: PropertyCacheTarget::ListCount,
        },
    );
    assert!(chunk.peek_adaptive_binary_cache(slot).is_none());
}

#[test]
fn peek_adaptive_binary_returns_none_for_out_of_bounds_slot() {
    // Defensive: `execute_adaptive_binary` filters its `slot`
    // through `inline_cache_slot` first, but
    // `peek_adaptive_binary_cache` should still return None for an
    // unmapped slot rather than panicking.
    let chunk = Chunk::new();
    assert!(chunk.peek_adaptive_binary_cache(0).is_none());
    assert!(chunk.peek_adaptive_binary_cache(usize::MAX).is_none());
}

#[test]
fn peek_adaptive_binary_state_is_copy() {
    // Compile-time assertion: `AdaptiveBinaryState: Copy` is the
    // whole point of this optimization — if a future variant adds
    // a non-Copy field, the static check below will fail at compile
    // time before the dispatcher silently regresses to the heavy
    // `InlineCacheEntry::clone` path.
    fn assert_copy<T: Copy>() {}
    assert_copy::<super::AdaptiveBinaryState>();
    assert_copy::<super::AdaptiveBinaryOp>();
    assert_copy::<super::BinaryShape>();
}

// --- peek_method_cache contract ---
//
// The peek replaces the per-dispatch `InlineCacheEntry::clone` on the
// method-call dispatch sites (`Op::MethodCall`, `Op::MethodCallOpt`,
// `Op::MethodCallSpread`). It must return None for unrelated IC variants
// — silently mis-extracting a `Property` / `DirectCall` / `AdaptiveBinary`
// slot as `Method` would feed garbage into `try_cached_method` and either
// spec-mis-fire (wrong target/argc) or skip the cache entirely on a real
// hit. These tests pin the variant gate.

#[test]
fn peek_method_cache_returns_none_for_empty_slot() {
    let mut chunk = Chunk::new();
    chunk.emit_method_call(0, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("MethodCall registers a slot");
    assert!(chunk.peek_method_cache(slot).is_none());
}

#[test]
fn peek_method_cache_returns_triple_after_warmup() {
    let mut chunk = Chunk::new();
    chunk.emit_method_call(7, 2, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("MethodCall registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Method {
            name_idx: 7,
            argc: 2,
            target: MethodCacheTarget::ListContains,
        },
    );
    let (name_idx, argc, target) = chunk.peek_method_cache(slot).expect("warmed slot peek");
    assert_eq!(name_idx, 7);
    assert_eq!(argc, 2);
    assert_eq!(target, MethodCacheTarget::ListContains);
}

#[test]
fn peek_method_cache_returns_none_for_non_method_variants() {
    // The cache slot may legitimately hold an `AdaptiveBinary`,
    // `Property`, or `DirectCall` entry. The peek must defensively
    // refuse non-Method variants regardless.
    let mut chunk = Chunk::new();
    chunk.emit_method_call(0, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("MethodCall registers a slot");

    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Property {
            name_idx: 0,
            target: PropertyCacheTarget::ListCount,
        },
    );
    assert!(chunk.peek_method_cache(slot).is_none());
}

#[test]
fn peek_method_cache_returns_none_for_out_of_bounds_slot() {
    let chunk = Chunk::new();
    assert!(chunk.peek_method_cache(0).is_none());
    assert!(chunk.peek_method_cache(usize::MAX).is_none());
}

#[test]
fn peek_method_cache_target_is_copy() {
    // Compile-time assertion: `MethodCacheTarget: Copy` is the whole
    // point of this peek path — if a future variant adds a non-Copy
    // field (eg. an `Arc<str>` for a dynamic method name), the static
    // check below will fail at compile time before the dispatcher
    // silently regresses to the heavy `InlineCacheEntry::clone` path.
    fn assert_copy<T: Copy>() {}
    assert_copy::<super::MethodCacheTarget>();
}

// --- peek_property_cache contract ---
//
// The peek replaces the per-dispatch `InlineCacheEntry::clone` on the
// property-read path (`Op::GetProperty` / `Op::GetPropertyOpt`). It
// must return None for unrelated IC variants — silently mis-extracting
// a `Method` / `DirectCall` / `AdaptiveBinary` slot as `Property` would
// feed garbage into `try_cached_property` (wrong target match, possibly
// a panic on the field-name lookup). These tests pin the variant gate.

#[test]
fn peek_property_cache_returns_none_for_empty_slot() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetProperty, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("GetProperty registers a slot");
    assert!(chunk.peek_property_cache(slot).is_none());
}

#[test]
fn peek_property_cache_returns_pair_after_warmup_for_dict_field() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetProperty, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("GetProperty registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Property {
            name_idx: 11,
            target: PropertyCacheTarget::DictField(Arc::from("count")),
        },
    );
    let (name_idx, target) = chunk
        .peek_property_cache(slot)
        .expect("warmed property slot peek");
    assert_eq!(name_idx, 11);
    match target {
        PropertyCacheTarget::DictField(field) => assert_eq!(field.as_ref(), "count"),
        other => panic!("expected DictField, got {other:?}"),
    }
}

#[test]
fn peek_property_cache_returns_pair_for_unit_target() {
    // Unit targets (eg. ListCount, ListEmpty, PairFirst) carry no Arc,
    // so the cloned PropertyCacheTarget is a pure scalar move at the
    // peek boundary. The hottest case in practice.
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetProperty, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("GetProperty registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Property {
            name_idx: 3,
            target: PropertyCacheTarget::ListCount,
        },
    );
    let (name_idx, target) = chunk
        .peek_property_cache(slot)
        .expect("warmed property slot peek");
    assert_eq!(name_idx, 3);
    assert_eq!(target, PropertyCacheTarget::ListCount);
}

#[test]
fn peek_property_cache_returns_none_for_non_property_variants() {
    let mut chunk = Chunk::new();
    chunk.emit_u16(Op::GetProperty, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("GetProperty registers a slot");
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Method {
            name_idx: 0,
            argc: 0,
            target: MethodCacheTarget::ListCount,
        },
    );
    assert!(chunk.peek_property_cache(slot).is_none());
}

#[test]
fn peek_property_cache_returns_none_for_out_of_bounds_slot() {
    let chunk = Chunk::new();
    assert!(chunk.peek_property_cache(0).is_none());
    assert!(chunk.peek_property_cache(usize::MAX).is_none());
}

// --- peek_direct_call_state contract ---
//
// Used on both the hot Specialized-hit check path (`try_cached_direct_call`
// / `try_cached_named_direct_call`) and the state-machine write-back
// (`next_direct_call_entry`). Returning None for the non-DirectCall slot
// shapes is critical: a mis-extracted Method/Property/AdaptiveBinary slot
// would have the dispatcher attempt a closure call with the wrong argc
// or Arc::ptr_eq against an unrelated closure.

#[test]
fn add_constant_keeps_signed_zero_and_nan_distinct() {
    let mut chunk = Chunk::new();
    // +0.0 and -0.0 are `==` under IEEE 754 but must NOT share a pool slot,
    // or the sign of `1.0 / 0.0` vs `1.0 / -0.0` would depend on intern order.
    let pos = chunk.add_constant(Constant::Float(0.0));
    let neg = chunk.add_constant(Constant::Float(-0.0));
    assert_ne!(pos, neg, "+0.0 and -0.0 must get distinct constant slots");
    // Re-adding the identical bit pattern still dedups.
    assert_eq!(pos, chunk.add_constant(Constant::Float(0.0)));
    assert_eq!(neg, chunk.add_constant(Constant::Float(-0.0)));
    // Ordinary floats still dedup by value.
    let a = chunk.add_constant(Constant::Float(1.5));
    assert_eq!(a, chunk.add_constant(Constant::Float(1.5)));
    let nan_a = chunk.add_constant(Constant::Float(f64::from_bits(0x7ff8_0000_0000_0001)));
    let nan_b = chunk.add_constant(Constant::Float(f64::from_bits(0x7ff8_0000_0000_0002)));
    assert_ne!(
        nan_a, nan_b,
        "distinct NaN payloads must get distinct constant slots"
    );
    assert_eq!(
        nan_a,
        chunk.add_constant(Constant::Float(f64::from_bits(0x7ff8_0000_0000_0001)))
    );
    // Non-float constants are unaffected.
    let s = chunk.add_constant(Constant::Int(7));
    assert_eq!(s, chunk.add_constant(Constant::Int(7)));
}

#[test]
fn add_constant_uses_first_slot_after_many_unique_constants() {
    let mut chunk = Chunk::new();
    let first = chunk.add_constant(Constant::String("shared".to_string()));
    for index in 0..10_000 {
        let slot = chunk.add_constant(Constant::String(format!("unique_{index}")));
        assert_eq!(slot as usize, index + 1);
    }
    assert_eq!(
        first,
        chunk.add_constant(Constant::String("shared".to_string())),
        "duplicate lookup must return the original slot after index growth"
    );
}

#[test]
fn constant_index_round_trips_through_cached_chunk() {
    let mut chunk = Chunk::new();
    let shared = chunk.add_constant(Constant::String("shared".to_string()));
    for index in 0..128 {
        chunk.add_constant(Constant::Int(index));
    }

    let frozen = chunk.freeze_for_cache();
    let constant_count = frozen.constants.len();
    let mut thawed = Chunk::from_cached(frozen);
    assert_eq!(
        shared,
        thawed.add_constant(Constant::String("shared".to_string())),
        "cache thaw must rebuild the constant side index"
    );
    let next = thawed.add_constant(Constant::String("new".to_string()));
    assert_eq!(next as usize, constant_count);
}

#[test]
fn peek_direct_call_state_returns_none_for_empty_slot() {
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("Op::Call registers a slot");
    assert!(chunk.peek_direct_call_state(slot).is_none());
}

#[test]
fn peek_direct_call_state_returns_warmup_state() {
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("Op::Call registers a slot");
    let target = synthetic_direct_call_target();
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::DirectCall {
            state: DirectCallState::Warmup {
                argc: 2,
                target: target.clone(),
                hits: 1,
            },
        },
    );
    let state = chunk
        .peek_direct_call_state(slot)
        .expect("warmed direct-call slot peek");
    match state {
        DirectCallState::Warmup {
            argc,
            target: peeked_target,
            hits,
        } => {
            assert_eq!(argc, 2);
            assert_eq!(hits, 1);
            assert_eq!(peeked_target, target);
        }
        other => panic!("expected Warmup, got {other:?}"),
    }
}

#[test]
fn peek_direct_call_state_returns_specialized_state() {
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("Op::Call registers a slot");
    let target = synthetic_direct_call_target();
    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::DirectCall {
            state: DirectCallState::Specialized {
                argc: 3,
                target: target.clone(),
                hits: 100,
                misses: 0,
            },
        },
    );
    let state = chunk
        .peek_direct_call_state(slot)
        .expect("warmed direct-call slot peek");
    match state {
        DirectCallState::Specialized {
            argc,
            target: peeked_target,
            hits,
            misses,
        } => {
            assert_eq!(argc, 3);
            assert_eq!(hits, 100);
            assert_eq!(misses, 0);
            assert_eq!(peeked_target, target);
        }
        other => panic!("expected Specialized, got {other:?}"),
    }
}

#[test]
fn peek_direct_call_state_returns_none_for_non_direct_call_variants() {
    let mut chunk = Chunk::new();
    chunk.emit_u8(Op::Call, 0, 1);
    let slot = chunk
        .inline_cache_slot(0)
        .expect("Op::Call registers a slot");

    chunk.set_inline_cache_entry(
        slot,
        InlineCacheEntry::Property {
            name_idx: 0,
            target: PropertyCacheTarget::ListCount,
        },
    );
    assert!(chunk.peek_direct_call_state(slot).is_none());
}

#[test]
fn peek_direct_call_state_returns_none_for_out_of_bounds_slot() {
    let chunk = Chunk::new();
    assert!(chunk.peek_direct_call_state(0).is_none());
    assert!(chunk.peek_direct_call_state(usize::MAX).is_none());
}

/// Build a synthetic `DirectCallTarget::Closure` for direct-call peek
/// tests. The closure has an empty body — the IC peek only inspects
/// the wrapping `Arc`, not the closure internals.
fn synthetic_direct_call_target() -> DirectCallTarget {
    use crate::value::VmClosure;
    use crate::{CompiledFunction, VmEnv};
    let func = CompiledFunction {
        name: "synthetic".to_string(),
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
    DirectCallTarget::Closure(Arc::new(VmClosure {
        func: Arc::new(func),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    }))
}
