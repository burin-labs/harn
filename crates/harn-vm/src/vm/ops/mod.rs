//! VM opcode dispatch.
//!
//! The `Op` enum, the byte-to-variant mapping, the sync and async
//! dispatch tables, the disassembly renderer, and the per-opcode
//! classification helpers (`op_reads_outer_name`, `is_adaptive_binary_op`)
//! are all emitted by `harn_opcode_macros::define_opcodes!` from the
//! single declarative table below. Adding or renaming an opcode is now a
//! one-line edit instead of paired edits to four hand-maintained match
//! tables — see issue #2584 for the migration that collapsed them.
//!
//! Each entry follows the shape
//!
//! ```ignore
//! VariantName { <kind>(<expr>...), disasm: <helper>("<LABEL>") [, flags: [...]] };
//! ```
//!
//! where `<kind>` selects the dispatch shape (`sync`, `sync_void`,
//! `sync_return`, `split`, `async_op`) and the `<helper>` resolves to a
//! `disasm_<helper>` free fn in `crate::chunk`.

mod arithmetic;
mod call;
mod collections;
mod comparison;
mod control_flow;
mod exception;
mod imports;
mod iter;
mod logical;
mod misc;
mod parallel;
mod stack;

use crate::value::{VmError, VmValue};

harn_opcode_macros::define_opcodes! {
    // === Constants & nil ===
    Constant { sync(self.execute_constant()), disasm: const_pool_u16("CONSTANT") };
    Nil { sync_void(self.execute_nil()), disasm: bare("NIL") };
    True { sync_void(self.execute_true()), disasm: bare("TRUE") };
    False { sync_void(self.execute_false()), disasm: bare("FALSE") };

    // === Variables ===
    GetVar { sync(self.execute_get_var()), disasm: const_pool_u16("GET_VAR"), flags: [reads_outer_name] };
    DefLet { sync(self.execute_def_let()), disasm: const_pool_u16("DEF_LET") };
    DefVar { sync(self.execute_def_var()), disasm: const_pool_u16("DEF_VAR") };
    SetVar { sync(self.execute_set_var()), disasm: const_pool_u16("SET_VAR"), flags: [reads_outer_name] };
    PushScope { sync_void(self.execute_push_scope()), disasm: bare("PUSH_SCOPE") };
    PopScope { sync_void(self.execute_pop_scope()), disasm: bare("POP_SCOPE") };

    // === Generic arithmetic (adaptive binary IC) ===
    Add { sync(self.execute_add()), disasm: bare("ADD"), flags: [adaptive_binary] };
    Sub { sync(self.execute_sub()), disasm: bare("SUB"), flags: [adaptive_binary] };
    Mul { sync(self.execute_mul()), disasm: bare("MUL"), flags: [adaptive_binary] };
    Div { sync(self.execute_div()), disasm: bare("DIV"), flags: [adaptive_binary] };
    Mod { sync(self.execute_mod()), disasm: bare("MOD"), flags: [adaptive_binary] };
    Pow { sync(self.execute_pow()), disasm: bare("POW") };
    Negate { sync(self.execute_negate()), disasm: bare("NEGATE") };

    // === Generic comparison (adaptive binary IC) ===
    Equal { sync(self.execute_equal()), disasm: bare("EQUAL"), flags: [adaptive_binary] };
    NotEqual { sync(self.execute_not_equal()), disasm: bare("NOT_EQUAL"), flags: [adaptive_binary] };
    Less { sync(self.execute_less()), disasm: bare("LESS"), flags: [adaptive_binary] };
    Greater { sync(self.execute_greater()), disasm: bare("GREATER"), flags: [adaptive_binary] };
    LessEqual { sync(self.execute_less_equal()), disasm: bare("LESS_EQUAL"), flags: [adaptive_binary] };
    GreaterEqual { sync(self.execute_greater_equal()), disasm: bare("GREATER_EQUAL"), flags: [adaptive_binary] };

    // === Logical ===
    Not { sync(self.execute_not()), disasm: bare("NOT") };

    // === Control flow ===
    Jump { sync_void(self.execute_jump()), disasm: u16("JUMP") };
    JumpIfFalse { sync(self.execute_jump_if_false()), disasm: u16("JUMP_IF_FALSE") };
    JumpIfTrue { sync(self.execute_jump_if_true()), disasm: u16("JUMP_IF_TRUE") };
    Pop { sync(self.execute_pop()), disasm: bare("POP") };

    // === Functions ===
    Call { split(self.execute_call_sync(), self.execute_call_async().await), disasm: u8("CALL"), flags: [reads_outer_name] };
    TailCall { split(self.execute_tail_call_sync(), self.execute_tail_call_async().await), disasm: u8("TAIL_CALL"), flags: [reads_outer_name] };
    Return { sync_return(self.execute_return()), disasm: bare("RETURN") };
    Closure { sync_void(self.execute_closure()), disasm: u16("CLOSURE") };

    // === Collections ===
    BuildList { sync_void(self.execute_build_list()), disasm: u16("BUILD_LIST") };
    BuildDict { sync_void(self.execute_build_dict()), disasm: u16("BUILD_DICT") };
    Subscript { sync(self.execute_subscript(false)), disasm: bare("SUBSCRIPT") };
    SubscriptOpt { sync(self.execute_subscript(true)), disasm: bare("SUBSCRIPT_OPT") };
    Slice { sync(self.execute_slice()), disasm: bare("SLICE") };

    // === Object operations ===
    GetProperty { sync(self.execute_get_property(false)), disasm: const_pool_u16("GET_PROPERTY") };
    GetPropertyOpt { sync(self.execute_get_property(true)), disasm: const_pool_u16("GET_PROPERTY_OPT") };
    SetProperty { sync(self.execute_set_property()), disasm: const_pool_u16("SET_PROPERTY") };
    SetSubscript { sync(self.execute_set_subscript()), disasm: const_pool_u16("SET_SUBSCRIPT") };
    MethodCall { split(self.execute_method_call_sync(false), self.execute_method_call(false).await), disasm: method_call("METHOD_CALL") };
    MethodCallOpt { split(self.execute_method_call_sync(true), self.execute_method_call(true).await), disasm: method_call("METHOD_CALL_OPT") };

    // === Strings ===
    Concat { sync_void(self.execute_concat()), disasm: u16("CONCAT") };

    // === Iteration ===
    IterInit { sync(self.execute_iter_init()), disasm: bare("ITER_INIT") };
    IterNext { split(self.execute_iter_next_sync(), self.execute_iter_next_async().await), disasm: u16("ITER_NEXT") };

    // === Pipe (async) ===
    Pipe { async_op(self.execute_pipe().await), disasm: bare("PIPE"), flags: [reads_outer_name] };

    // === Error handling ===
    Throw { sync(self.execute_throw()), disasm: bare("THROW") };
    TryCatchSetup { sync_void(self.execute_try_catch_setup()), disasm: u16("TRY_CATCH_SETUP") };
    PopHandler { sync_void(self.execute_pop_handler()), disasm: bare("POP_HANDLER") };

    // === Concurrency ===
    Parallel { async_op(self.execute_parallel().await), disasm: bare("PARALLEL") };
    ParallelMap { async_op(self.execute_parallel_map().await), disasm: bare("PARALLEL_MAP") };
    ParallelMapStream { async_op(self.execute_parallel_map_stream().await), disasm: bare("PARALLEL_MAP_STREAM") };
    ParallelSettle { async_op(self.execute_parallel_settle().await), disasm: bare("PARALLEL_SETTLE") };
    Spawn { sync(self.execute_spawn()), disasm: bare("SPAWN") };
    SyncMutexEnter { async_op(self.execute_sync_mutex_enter().await), disasm: bare("SYNC_MUTEX_ENTER") };
    SyncMutexEnterKeyed { async_op(self.execute_sync_mutex_enter_keyed().await), disasm: bare("SYNC_MUTEX_ENTER_KEYED") };
    TaskScopeEnter { sync_void(self.execute_task_scope_enter()), disasm: bare("TASK_SCOPE_ENTER") };
    TaskScopeExit { async_op(self.execute_task_scope_exit().await), disasm: bare("TASK_SCOPE_EXIT") };

    // === Imports (async) ===
    Import { async_op(self.execute_import_op().await), disasm: const_pool_u16("IMPORT") };
    SelectiveImport { async_op(self.execute_selective_import().await), disasm: selective_import("SELECTIVE_IMPORT") };

    // === Deadline ===
    DeadlineSetup { sync(self.execute_deadline_setup()), disasm: bare("DEADLINE_SETUP") };
    DeadlineEnd { sync_void(self.execute_deadline_end()), disasm: bare("DEADLINE_END") };

    // === Enum ===
    BuildEnum { sync(self.execute_build_enum()), disasm: build_enum("BUILD_ENUM") };
    MatchEnum { sync(self.execute_match_enum()), disasm: match_enum("MATCH_ENUM") };

    // === Loop control ===
    PopIterator { sync_void(self.execute_pop_iterator()), disasm: bare("POP_ITERATOR") };

    // === Defaults ===
    GetArgc { sync_void(self.execute_get_argc()), disasm: bare("GET_ARGC") };

    // === Type checking ===
    CheckType { sync(self.execute_check_type()), disasm: check_type("CHECK_TYPE"), flags: [reads_outer_name] };

    // === Result try operator ===
    TryUnwrap { sync(self.execute_try_unwrap()), disasm: bare("TRY_UNWRAP") };
    TryWrapOk { sync(self.execute_try_wrap_ok()), disasm: bare("TRY_WRAP_OK") };

    // === Spread calls ===
    CallSpread { async_op(self.execute_call_spread().await), disasm: bare("CALL_SPREAD"), flags: [reads_outer_name] };
    CallBuiltin { split(self.execute_call_builtin_sync(), self.execute_call_builtin_async().await), disasm: call_builtin("CALL_BUILTIN"), flags: [reads_outer_name] };
    CallBuiltinSpread { async_op(self.execute_call_builtin_spread().await), disasm: call_builtin_spread("CALL_BUILTIN_SPREAD"), flags: [reads_outer_name] };
    MethodCallSpread { async_op(self.execute_method_call_spread().await), disasm: method_call_spread("METHOD_CALL_SPREAD") };

    // === Misc ===
    Dup { sync(self.execute_dup()), disasm: bare("DUP") };
    Swap { sync_void(self.execute_swap()), disasm: bare("SWAP") };
    Contains { sync(self.execute_contains()), disasm: bare("CONTAINS") };

    // === Typed arithmetic fast paths ===
    AddInt { sync(self.execute_add_int()), disasm: bare("ADD_INT") };
    SubInt { sync(self.execute_sub_int()), disasm: bare("SUB_INT") };
    MulInt { sync(self.execute_mul_int()), disasm: bare("MUL_INT") };
    DivInt { sync(self.execute_div_int()), disasm: bare("DIV_INT") };
    ModInt { sync(self.execute_mod_int()), disasm: bare("MOD_INT") };
    AddFloat { sync(self.execute_add_float()), disasm: bare("ADD_FLOAT") };
    SubFloat { sync(self.execute_sub_float()), disasm: bare("SUB_FLOAT") };
    MulFloat { sync(self.execute_mul_float()), disasm: bare("MUL_FLOAT") };
    DivFloat { sync(self.execute_div_float()), disasm: bare("DIV_FLOAT") };
    ModFloat { sync(self.execute_mod_float()), disasm: bare("MOD_FLOAT") };

    // === Typed comparison fast paths ===
    EqualInt { sync(self.execute_equal_int()), disasm: bare("EQUAL_INT") };
    NotEqualInt { sync(self.execute_not_equal_int()), disasm: bare("NOT_EQUAL_INT") };
    LessInt { sync(self.execute_less_int()), disasm: bare("LESS_INT") };
    GreaterInt { sync(self.execute_greater_int()), disasm: bare("GREATER_INT") };
    LessEqualInt { sync(self.execute_less_equal_int()), disasm: bare("LESS_EQUAL_INT") };
    GreaterEqualInt { sync(self.execute_greater_equal_int()), disasm: bare("GREATER_EQUAL_INT") };
    EqualFloat { sync(self.execute_equal_float()), disasm: bare("EQUAL_FLOAT") };
    NotEqualFloat { sync(self.execute_not_equal_float()), disasm: bare("NOT_EQUAL_FLOAT") };
    LessFloat { sync(self.execute_less_float()), disasm: bare("LESS_FLOAT") };
    GreaterFloat { sync(self.execute_greater_float()), disasm: bare("GREATER_FLOAT") };
    LessEqualFloat { sync(self.execute_less_equal_float()), disasm: bare("LESS_EQUAL_FLOAT") };
    GreaterEqualFloat { sync(self.execute_greater_equal_float()), disasm: bare("GREATER_EQUAL_FLOAT") };
    EqualBool { sync(self.execute_equal_bool()), disasm: bare("EQUAL_BOOL") };
    NotEqualBool { sync(self.execute_not_equal_bool()), disasm: bare("NOT_EQUAL_BOOL") };
    EqualString { sync(self.execute_equal_string()), disasm: bare("EQUAL_STRING") };
    NotEqualString { sync(self.execute_not_equal_string()), disasm: bare("NOT_EQUAL_STRING") };

    // === Generators (async) ===
    Yield { async_op(self.execute_yield().await), disasm: bare("YIELD") };

    // === Slot-indexed locals ===
    GetLocalSlot { sync(self.execute_get_local_slot()), disasm: local_slot_u16("GET_LOCAL_SLOT") };
    DefLocalSlot { sync(self.execute_def_local_slot()), disasm: local_slot_u16("DEF_LOCAL_SLOT") };
    SetLocalSlot { sync(self.execute_set_local_slot()), disasm: local_slot_u16("SET_LOCAL_SLOT") };
}

impl super::Vm {
    /// Execute a single opcode. Used by the scope-interrupt wrapper that
    /// drives cancellable / deadlined execution; the hot interpreter loop
    /// in `run_chunk_ref` bypasses this and calls `execute_op_sync` /
    /// `execute_op_async` directly when no interrupt machinery is armed.
    pub(super) async fn execute_op(&mut self, op_byte: u8) -> Result<Option<VmValue>, VmError> {
        let op = Op::from_byte(op_byte).ok_or(VmError::InvalidInstruction(op_byte))?;
        if let Some(result) = self.execute_op_sync(op) {
            result?;
            return Ok(None);
        }
        self.execute_op_async(op).await?;
        Ok(None)
    }
}
