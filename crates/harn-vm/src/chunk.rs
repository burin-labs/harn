use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use harn_parser::TypeExpr;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::harness::HarnessKind;
use crate::runtime_guards::RuntimeParamGuard;

/// Sentinel value stored in [`Chunk::inline_cache_index`] for code offsets
/// that have no inline-cache slot registered. Chosen as `u32::MAX` so the
/// hot dispatch path can treat the side-table as a flat `Vec<u32>` without
/// an `Option` wrapper — the comparison against the sentinel collapses to a
/// single integer compare. The compile-time max useful slot count is bounded
/// by code length (one slot per cacheable opcode), so `u32::MAX` is safely
/// out of the addressable slot range.
pub(crate) const NO_INLINE_CACHE_SLOT: u32 = u32::MAX;
static NEXT_CHUNK_CACHE_ID: AtomicU64 = AtomicU64::new(1);

fn next_chunk_cache_id() -> u64 {
    NEXT_CHUNK_CACHE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Bytecode opcodes for the Harn VM. The enum, the byte-to-variant
/// mapping, the sync and async dispatch tables, the disassembly
/// renderer, and the per-opcode classification helpers are all emitted
/// by `harn_opcode_macros::define_opcodes!` in [`crate::vm::ops`].
/// Re-exported here so callers that import `crate::chunk::Op` need no
/// awareness of the macro layout.
pub use crate::vm::ops::Op;
pub(crate) use crate::vm::ops::{is_adaptive_binary_op, op_reads_outer_name};

/// A constant value in the constant pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Nil,
    Duration(i64),
}

/// Identity used for constant-pool deduplication.
///
/// This is stricter than `PartialEq` for floats: it compares `Constant::Float`
/// operands by their raw bits, so `+0.0` and `-0.0` (which are `==` under IEEE
/// 754) get distinct pool slots, and each distinct NaN bit-pattern is preserved.
/// Collapsing `+0.0`/`-0.0` onto one slot makes signed zero — and therefore the
/// sign of `1.0 / 0.0` vs `1.0 / -0.0` — depend on which literal happened to be
/// interned first. The derived `PartialEq` is left intact for all other uses.
fn constants_identical(a: &Constant, b: &Constant) -> bool {
    match (a, b) {
        (Constant::Float(x), Constant::Float(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// Hashable identity for constant-pool deduplication.
///
/// Mirrors [`constants_identical`] exactly, including bitwise float identity,
/// so the compiler can replace the previous linear scan with an amortized O(1)
/// side index without changing bytecode-visible constant slots.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConstantKey {
    Int(i64),
    Float(u64),
    String(String),
    Bool(bool),
    Nil,
    Duration(i64),
}

impl From<&Constant> for ConstantKey {
    fn from(constant: &Constant) -> Self {
        match constant {
            Constant::Int(value) => Self::Int(*value),
            Constant::Float(value) => Self::Float(value.to_bits()),
            Constant::String(value) => Self::String(value.clone()),
            Constant::Bool(value) => Self::Bool(*value),
            Constant::Nil => Self::Nil,
            Constant::Duration(value) => Self::Duration(*value),
        }
    }
}

fn build_constant_index(constants: &[Constant]) -> HashMap<ConstantKey, u16> {
    let mut index = HashMap::with_capacity(constants.len());
    for (slot, constant) in constants.iter().enumerate() {
        if let Ok(slot) = u16::try_from(slot) {
            index.entry(ConstantKey::from(constant)).or_insert(slot);
        }
    }
    index
}

/// Runtime-only inline-cache state for bytecode instructions that repeatedly
/// see the same dynamic shape. Lookup caches stay monomorphic on a name and
/// receiver shape. Adaptive caches warm on a stable operand or call target,
/// then fall back through the generic opcode and replace or reset state when
/// the observed shape changes.
///
/// This vector is intentionally excluded from [`CachedChunk`]: bytecode cache
/// artifacts keep the slot layout but start with empty runtime feedback in each
/// process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InlineCacheEntry {
    Empty,
    Property {
        name_idx: u16,
        target: PropertyCacheTarget,
    },
    Method {
        name_idx: u16,
        argc: usize,
        target: MethodCacheTarget,
    },
    AdaptiveBinary {
        op: AdaptiveBinaryOp,
        state: AdaptiveBinaryState,
    },
    DirectCall {
        state: DirectCallState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdaptiveBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

/// Adaptive-binary IC state. All fields are scalar `Copy` (shape is a
/// `Copy` enum, hit/miss counters are integers), so the struct as a whole
/// is `Copy`. This lets `execute_adaptive_binary` extract the cached state
/// by value for the specialization check without cloning the wrapping
/// `InlineCacheEntry` on every dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdaptiveBinaryState {
    Warmup {
        shape: BinaryShape,
        hits: u8,
    },
    Specialized {
        shape: BinaryShape,
        hits: u64,
        misses: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryShape {
    Int,
    Float,
    Bool,
    String,
}

#[derive(Debug, Clone)]
pub(crate) enum DirectCallState {
    Warmup {
        argc: usize,
        target: DirectCallTarget,
        hits: u8,
    },
    Specialized {
        argc: usize,
        target: DirectCallTarget,
        hits: u64,
        misses: u64,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum DirectCallTarget {
    Closure(Arc<crate::value::VmClosure>),
}

impl PartialEq for DirectCallTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Closure(left), Self::Closure(right)) => Arc::ptr_eq(left, right),
        }
    }
}

impl Eq for DirectCallTarget {}

impl PartialEq for DirectCallState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Warmup {
                    argc: left_argc,
                    target: left_target,
                    hits: left_hits,
                },
                Self::Warmup {
                    argc: right_argc,
                    target: right_target,
                    hits: right_hits,
                },
            ) => left_argc == right_argc && left_target == right_target && left_hits == right_hits,
            (
                Self::Specialized {
                    argc: left_argc,
                    target: left_target,
                    hits: left_hits,
                    misses: left_misses,
                },
                Self::Specialized {
                    argc: right_argc,
                    target: right_target,
                    hits: right_hits,
                    misses: right_misses,
                },
            ) => {
                left_argc == right_argc
                    && left_target == right_target
                    && left_hits == right_hits
                    && left_misses == right_misses
            }
            _ => false,
        }
    }
}

impl Eq for DirectCallState {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PropertyCacheTarget {
    DictField(Arc<str>),
    StructField { field_name: Arc<str>, index: usize },
    HarnessSubHandle(HarnessKind),
    ListCount,
    ListEmpty,
    ListFirst,
    ListLast,
    StringCount,
    StringEmpty,
    PairFirst,
    PairSecond,
    EnumVariant,
    EnumFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MethodCacheTarget {
    Harness(HarnessKind),
    ListCount,
    ListEmpty,
    ListContains,
    StringCount,
    StringEmpty,
    StringContains,
    DictCount,
    DictHas,
    RangeCount,
    RangeLen,
    RangeEmpty,
    RangeFirst,
    RangeLast,
    SetCount,
    SetLen,
    SetEmpty,
    SetContains,
}

/// Debug metadata for a slot-indexed local in a compiled chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSlotInfo {
    pub name: String,
    pub mutable: bool,
    pub scope_depth: usize,
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Int(n) => write!(f, "{n}"),
            Constant::Float(n) => write!(f, "{n}"),
            Constant::String(s) => write!(f, "\"{s}\""),
            Constant::Bool(b) => write!(f, "{b}"),
            Constant::Nil => write!(f, "nil"),
            Constant::Duration(ms) => write!(f, "{ms}ms"),
        }
    }
}

/// A compiled chunk of bytecode.
#[derive(Debug)]
pub struct Chunk {
    /// Runtime-only identity for VM-local inline cache storage. It is not
    /// serialized; freshly compiled or loaded chunks get new ids, while clones
    /// keep the same id because they represent the same bytecode object.
    cache_id: u64,
    /// The bytecode instructions.
    pub code: Vec<u8>,
    /// Constant pool.
    pub constants: Vec<Constant>,
    /// Compile-time constant-pool side index. Derived from [`Chunk::constants`]
    /// and intentionally omitted from [`CachedChunk`]; bytecode-cache loads
    /// rebuild it from the serialized constant vector.
    constant_index: HashMap<ConstantKey, u16>,
    /// Source line numbers for each instruction (for error reporting).
    pub lines: Vec<u32>,
    /// Source column numbers for each instruction (for error reporting).
    /// Parallel to `lines`; 0 means no column info available.
    pub columns: Vec<u32>,
    /// Source file that this chunk was compiled from, when known. Set for
    /// chunks compiled from imported modules so runtime errors can report
    /// the correct file path for each frame instead of always pointing at
    /// the entry-point pipeline.
    pub source_file: Option<String>,
    /// Current column to use when emitting instructions (set by compiler).
    current_col: u32,
    /// Compiled function bodies (for closures).
    pub functions: Vec<CompiledFunctionRef>,
    /// Instruction offset to inline-cache slot. Slots are assigned at emit time
    /// for cacheable instructions while bytecode bytes remain immutable.
    /// Preserved as the serialization-stable representation that round-trips
    /// through [`CachedChunk`]; the runtime hot path reads
    /// [`Chunk::inline_cache_index`] instead.
    inline_cache_slots: BTreeMap<usize, usize>,
    /// Flat side-table indexed by code offset that returns the inline-cache
    /// slot index (or [`NO_INLINE_CACHE_SLOT`] for "no slot at this offset").
    /// Built alongside [`Chunk::inline_cache_slots`] at emit/load time so the
    /// per-dispatch lookup that fires on every adaptive binary op, `Op::Call`,
    /// `Op::MethodCall`, and `Op::GetProperty` is one cache-friendly `Vec`
    /// index instead of a `BTreeMap::get` (O(1) vs O(log n) with the
    /// associated pointer chasing). Derived; intentionally not serialized.
    inline_cache_index: Vec<u32>,
    /// Test/bench scratch entries for validating inline-cache transitions.
    /// Runtime execution keeps live cache entries on each `Vm` isolate so
    /// parallel workers do not contend on shared compiled chunks.
    inline_caches: Arc<Mutex<Vec<InlineCacheEntry>>>,
    /// Lazily-materialized shared string cache for `Constant::String` entries,
    /// parallel to `constants`. String constants are materialized once per
    /// unique constant; subsequent pushes are a [`HarnStr`] refcount bump.
    constant_strings: Arc<Mutex<Vec<Option<crate::value::HarnStr>>>>,
    /// Source-name metadata for slot-indexed locals in this chunk.
    pub(crate) local_slots: Vec<LocalSlotInfo>,
    /// True when this chunk's bytecode emits an opcode that resolves a
    /// name through the runtime env (`GetVar`, `SetVar`, `CallBuiltin`,
    /// `CallBuiltinSpread`, `CheckType`). The closure-call hot path uses
    /// this as a cheap static guard: if a closure body never reads
    /// outer names by name, the caller-scope late-bind walks in
    /// [`Vm::closure_call_env`] and
    /// [`Vm::closure_call_env_for_current_frame`] are pure overhead and
    /// can be skipped, leaving the closure's captured env as-is.
    ///
    /// Walks exist to inject late-bound closure-typed names — typically
    /// for self/mutually-recursive local fns and for fns whose captured
    /// env predates a sibling definition. Inline arithmetic / comparison
    /// callbacks (the `.map(x -> x * 2)` / `.filter(x -> x % 2 == 0)`
    /// shape) emit none of the flagged opcodes, so the walk is wasted
    /// work on every invocation.
    pub(crate) references_outer_names: bool,
    /// Compile-time operand-stack-depth tracking for the debug-build
    /// balance assertion (issue #2622). `balance_depth` is the running net
    /// effect of every *linearly-modeled* opcode emitted so far;
    /// `balance_nonlinear` counts emits whose effect can't be tracked by a
    /// straight-line sum (jumps, `return`, async/handler ops, variadic ops
    /// whose count isn't an emit argument). A statement is "balance-exact"
    /// only when `balance_nonlinear` is unchanged across its compilation,
    /// at which point `balance_depth`'s delta is its true net stack effect.
    /// Transient compile-time state: reset by [`Chunk::new`], never
    /// serialized into [`CachedChunk`], and read only by debug assertions —
    /// so a wrong absolute value (which a non-exact statement can leave
    /// behind) is harmless; only per-statement *deltas over exact spans*
    /// are ever trusted.
    #[cfg(debug_assertions)]
    balance_depth: i32,
    #[cfg(debug_assertions)]
    balance_nonlinear: u32,
}

pub type ChunkRef = Arc<Chunk>;
pub type CompiledFunctionRef = Arc<CompiledFunction>;

impl Clone for Chunk {
    fn clone(&self) -> Self {
        Self {
            cache_id: self.cache_id,
            code: self.code.clone(),
            constants: self.constants.clone(),
            constant_index: self.constant_index.clone(),
            lines: self.lines.clone(),
            columns: self.columns.clone(),
            source_file: self.source_file.clone(),
            current_col: self.current_col,
            functions: self.functions.clone(),
            inline_cache_slots: self.inline_cache_slots.clone(),
            inline_cache_index: self.inline_cache_index.clone(),
            inline_caches: Arc::new(Mutex::new(vec![
                InlineCacheEntry::Empty;
                self.inline_cache_slot_count()
            ])),
            constant_strings: Arc::new(Mutex::new(vec![None; self.constants.len()])),
            local_slots: self.local_slots.clone(),
            references_outer_names: self.references_outer_names,
            #[cfg(debug_assertions)]
            balance_depth: self.balance_depth,
            #[cfg(debug_assertions)]
            balance_nonlinear: self.balance_nonlinear,
        }
    }
}

/// Serializable snapshot of a [`Chunk`] suitable for the on-disk bytecode
/// cache and for in-memory stdlib artifact caches. Inline-cache state is
/// dropped at freeze time because it warms at runtime per VM isolate; the
/// rest of the chunk round-trips byte-identically.
#[derive(Debug, Serialize, Deserialize)]
pub struct CachedChunk {
    pub(crate) code: Vec<u8>,
    pub(crate) constants: Vec<Constant>,
    pub(crate) lines: Vec<u32>,
    pub(crate) columns: Vec<u32>,
    pub(crate) source_file: Option<String>,
    pub(crate) current_col: u32,
    pub(crate) functions: Vec<CachedCompiledFunction>,
    pub(crate) inline_cache_slots: BTreeMap<usize, usize>,
    pub(crate) local_slots: Vec<LocalSlotInfo>,
    #[serde(default)]
    pub(crate) references_outer_names: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedCompiledFunction {
    pub(crate) name: String,
    pub(crate) type_params: Vec<String>,
    pub(crate) nominal_type_names: Vec<String>,
    pub(crate) params: Vec<CachedParamSlot>,
    pub(crate) default_start: Option<usize>,
    pub(crate) chunk: CachedChunk,
    pub(crate) is_generator: bool,
    pub(crate) is_stream: bool,
    pub(crate) has_rest_param: bool,
    pub(crate) has_runtime_type_checks: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CachedParamSlot {
    pub(crate) name: String,
    pub(crate) type_expr: Option<TypeExpr>,
    pub(crate) has_default: bool,
}

impl CachedParamSlot {
    fn thaw(self) -> ParamSlot {
        let runtime_guard = self
            .type_expr
            .as_ref()
            .map(RuntimeParamGuard::from_type_expr);
        ParamSlot {
            name: self.name,
            type_expr: self.type_expr,
            runtime_guard,
            has_default: self.has_default,
        }
    }
}

/// One parameter slot of a compiled user-defined function. Carries the
/// declared name, the (optional) declared type expression, and a flag
/// for whether a default value was provided. The runtime consults the
/// type expression in `bind_param_slots` to enforce declared types
/// against the values supplied at the call site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSlot {
    pub name: String,
    /// Declared parameter type. `None` for untyped parameters (gradual
    /// typing); the runtime skips type assertion when absent.
    pub type_expr: Option<TypeExpr>,
    /// Precomputed runtime validation metadata derived from `type_expr`.
    /// Bytecode-cache artifacts omit this field and rebuild it at load time.
    #[serde(skip)]
    pub(crate) runtime_guard: Option<RuntimeParamGuard>,
    /// True when the parameter has a default-value clause. Diagnostic
    /// only — the canonical authority for arity ranges is
    /// [`CompiledFunction::default_start`].
    pub has_default: bool,
}

impl ParamSlot {
    /// Build a [`ParamSlot`] from a parser-side [`harn_parser::TypedParam`].
    /// Centralizes the conversion so every compile path stays in lockstep.
    pub fn from_typed_param(param: &harn_parser::TypedParam) -> Self {
        Self::from_typed_param_with_type(param, param.type_expr.clone())
    }

    pub(crate) fn from_typed_param_with_type(
        param: &harn_parser::TypedParam,
        type_expr: Option<TypeExpr>,
    ) -> Self {
        let runtime_guard = type_expr.as_ref().map(RuntimeParamGuard::from_type_expr);
        Self {
            name: param.name.clone(),
            type_expr,
            runtime_guard,
            has_default: param.default_value.is_some(),
        }
    }

    fn freeze_for_cache(&self) -> CachedParamSlot {
        CachedParamSlot {
            name: self.name.clone(),
            type_expr: self.type_expr.clone(),
            has_default: self.has_default,
        }
    }

    /// Build a `Vec<ParamSlot>` from a slice of parser-side typed
    /// parameters. Used pervasively at compile sites instead of
    /// `TypedParam::names` (which discarded the type info we now need
    /// at runtime).
    pub fn vec_from_typed(params: &[harn_parser::TypedParam]) -> Vec<Self> {
        params.iter().map(Self::from_typed_param).collect()
    }
}

/// A compiled function (closure body).
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    pub name: String,
    /// Generic type parameters declared by this function. Runtime
    /// validation treats these as static-only constraints because the VM
    /// does not monomorphize function bodies.
    pub type_params: Vec<String>,
    /// User-defined struct and enum names visible when this function was
    /// compiled. These are the only non-primitive named types with runtime
    /// nominal identity; aliases and interfaces remain static-only.
    pub nominal_type_names: Vec<String>,
    pub params: Vec<ParamSlot>,
    /// Index of the first parameter with a default value, or None if all required.
    pub default_start: Option<usize>,
    pub chunk: ChunkRef,
    /// True if the function body contains `yield` expressions (generator function).
    pub is_generator: bool,
    /// True if the function was declared as `gen fn` and should return Stream.
    pub is_stream: bool,
    /// True if the last parameter is a rest parameter (`...name`).
    pub has_rest_param: bool,
    /// True when at least one parameter has a runtime-visible type
    /// assertion. Untyped closures dominate collection callback hot paths,
    /// so this lets the VM skip the per-argument metadata walk after the
    /// arity check.
    pub has_runtime_type_checks: bool,
}

impl CompiledFunction {
    pub(crate) fn has_runtime_type_checks_for_params(params: &[ParamSlot]) -> bool {
        params.iter().any(|param| param.type_expr.is_some())
    }

    /// Returns just the parameter names — convenience for code paths that
    /// don't care about types or defaults.
    pub fn param_names(&self) -> impl Iterator<Item = &str> {
        self.params.iter().map(|p| p.name.as_str())
    }

    /// Number of required parameters (those before `default_start`).
    pub fn required_param_count(&self) -> usize {
        self.default_start.unwrap_or(self.params.len())
    }

    /// Minimum number of caller-supplied arguments needed to enter the function.
    pub(crate) fn minimum_arg_count(&self) -> usize {
        if self.has_rest_param {
            self.required_param_count()
                .min(self.params.len().saturating_sub(1))
        } else {
            self.required_param_count()
        }
    }

    /// Argument count visible to callee bytecode via `GetArgc`.
    pub(crate) fn callee_arg_count(&self, supplied: usize) -> usize {
        if self.has_rest_param {
            supplied
        } else {
            supplied.min(self.params.len())
        }
    }

    pub fn declares_type_param(&self, name: &str) -> bool {
        self.type_params.iter().any(|param| param == name)
    }

    pub fn has_nominal_type(&self, name: &str) -> bool {
        self.nominal_type_names.iter().any(|ty| ty == name)
    }

    pub(crate) fn freeze_for_cache(&self) -> CachedCompiledFunction {
        CachedCompiledFunction {
            name: self.name.clone(),
            type_params: self.type_params.clone(),
            nominal_type_names: self.nominal_type_names.clone(),
            params: self
                .params
                .iter()
                .map(ParamSlot::freeze_for_cache)
                .collect(),
            default_start: self.default_start,
            chunk: self.chunk.freeze_for_cache(),
            is_generator: self.is_generator,
            is_stream: self.is_stream,
            has_rest_param: self.has_rest_param,
            has_runtime_type_checks: self.has_runtime_type_checks,
        }
    }

    pub(crate) fn from_cached(cached: CachedCompiledFunction) -> Self {
        Self {
            name: cached.name,
            type_params: cached.type_params,
            nominal_type_names: cached.nominal_type_names,
            params: cached
                .params
                .into_iter()
                .map(CachedParamSlot::thaw)
                .collect(),
            default_start: cached.default_start,
            chunk: Arc::new(Chunk::from_cached(cached.chunk)),
            is_generator: cached.is_generator,
            is_stream: cached.is_stream,
            has_rest_param: cached.has_rest_param,
            has_runtime_type_checks: cached.has_runtime_type_checks,
        }
    }
}

/// A snapshot of [`Chunk`]'s compile-time balance model, returned by
/// [`Chunk::balance_probe`] and consumed by [`Chunk::balance_delta_since`].
#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
pub(crate) struct BalanceProbe {
    depth: i32,
    nonlinear: u32,
}

/// Net operand-stack effect (`pushes - pops`) of one emitted opcode, for
/// the debug-build balance assertion (issue #2622). `count` is the opcode's
/// variadic arity when that arity is the emit-call argument (`BuildList`
/// length, `Call` argc, …) and `0` otherwise.
///
/// `Some(delta)` means the effect is exactly modeled. `None` marks an
/// opcode a straight-line running sum can't track — control flow that
/// branches or terminates (`Jump*`, `Return`, `Throw`, `TailCall`),
/// async/handler ops, and variadic ops whose arity rides in a raw operand
/// byte rather than the emit argument (`BuildEnum`, `MatchEnum`). Such an
/// opcode taints its enclosing statement as non-exact, so the assertion
/// skips it instead of risking a false trip.
///
/// The `match` is intentionally exhaustive with no `_` arm: adding an
/// opcode forces a classification here (a compile error otherwise), so the
/// balance model can't silently drift out of sync with the instruction set.
#[cfg(debug_assertions)]
fn op_stack_delta(op: Op, count: u16) -> Option<i32> {
    use Op::*;
    let count = count as i32;
    Some(match op {
        // Push one value.
        Constant | Nil | True | False | GetVar | GetArgc | GetLocalSlot | Closure | Dup => 1,
        // Consume one value (into a binding / property / discard). `SetVar`,
        // `SetProperty` and the local-slot stores read their target by name
        // or slot index, so they only pop the value being stored.
        DefLet | DefVar | DefCell | SetVar | DefLocalSlot | SetLocalSlot | SetProperty
        | SetLocalSlotProperty | ConcatAssignLocal | Pop => -1,
        // Value-preserving: unary ops, by-name lookups/checks, and scope /
        // iterator / exception-handler bookkeeping (the last three touch
        // side stacks, not the operand stack).
        Negate | Not | GetProperty | GetPropertyOpt | CheckType | TryUnwrap | TryWrapOk | Swap
        | PushScope | PopScope | PopIterator | PopHandler => 0,
        // Pop two, push one.
        Add | Sub | Mul | Div | Mod | Pow | AddInt | SubInt | MulInt | DivInt | ModInt
        | AddFloat | SubFloat | MulFloat | DivFloat | ModFloat | Equal | NotEqual | Less
        | Greater | LessEqual | GreaterEqual | EqualInt | NotEqualInt | LessInt | GreaterInt
        | LessEqualInt | GreaterEqualInt | EqualFloat | NotEqualFloat | LessFloat
        | GreaterFloat | LessEqualFloat | GreaterEqualFloat | EqualBool | NotEqualBool
        | EqualString | NotEqualString | Contains | Subscript | SubscriptOpt => -1,
        // `IterInit` consumes the iterable and pushes nothing (the iterator
        // lives on a side stack).
        IterInit => -1,
        // Net -2: `Slice` pops object/start/end and pushes one value;
        // subscript stores pop value/index and read the target from bytecode.
        Slice | SetSubscript | SetLocalSlotSubscript => -2,
        // Variadic whose arity is the emit argument: pop `count`, push one.
        BuildList | Concat | CallBuiltin => 1 - count,
        BuildDict => 1 - 2 * count,
        // Calls also pop the callee/receiver beneath the args.
        Call | MethodCall | MethodCallOpt => -count,
        // Non-linear (see doc comment): branches, terminators, async/handler
        // ops, and variadic ops whose arity isn't the emit argument.
        Jump | JumpIfFalse | JumpIfTrue | IterNext | Return | TailCall | Throw | TryCatchSetup
        | Spawn | Pipe | Parallel | ParallelMap | ParallelMapStream | ParallelSettle
        | SyncMutexEnter | SyncMutexEnterKeyed | TaskScopeEnter | TaskScopeExit | Import
        | SelectiveImport | NamespaceImport | DeadlineSetup | DeadlineEnd | BuildEnum
        | MatchEnum | Yield | CallSpread | CallBuiltinSpread | MethodCallSpread => return None,
    })
}

impl Chunk {
    pub fn new() -> Self {
        Self {
            cache_id: next_chunk_cache_id(),
            code: Vec::new(),
            constants: Vec::new(),
            constant_index: HashMap::new(),
            lines: Vec::new(),
            columns: Vec::new(),
            source_file: None,
            current_col: 0,
            functions: Vec::new(),
            inline_cache_slots: BTreeMap::new(),
            inline_cache_index: Vec::new(),
            inline_caches: Arc::new(Mutex::new(Vec::new())),
            constant_strings: Arc::new(Mutex::new(Vec::new())),
            local_slots: Vec::new(),
            references_outer_names: false,
            #[cfg(debug_assertions)]
            balance_depth: 0,
            #[cfg(debug_assertions)]
            balance_nonlinear: 0,
        }
    }

    /// Set the current column for subsequent emit calls.
    pub fn set_column(&mut self, col: u32) {
        self.current_col = col;
    }

    /// Add a constant and return its index.
    pub fn add_constant(&mut self, constant: Constant) -> u16 {
        debug_assert!(
            self.constant_index.len() <= self.constants.len(),
            "constant side index cannot outgrow the constant pool"
        );
        let key = ConstantKey::from(&constant);
        if let Some(index) = self.constant_index.get(&key) {
            debug_assert!(
                self.constants
                    .get(*index as usize)
                    .is_some_and(|existing| constants_identical(existing, &constant)),
                "constant side index drifted from the constant pool"
            );
            return *index;
        }
        let idx = self.constants.len();
        let idx = u16::try_from(idx).expect("constant pool exceeded u16 operand space");
        self.constants.push(constant);
        self.constant_index.insert(key, idx);
        idx
    }

    /// Emit a single-byte instruction.
    pub fn emit(&mut self, op: Op, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(op, 0);
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(op as u8);
        self.lines.push(line);
        self.columns.push(col);
        if is_adaptive_binary_op(op) {
            self.register_inline_cache(op_offset);
        }
        if op_reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit an instruction with a u16 argument.
    pub fn emit_u16(&mut self, op: Op, arg: u16, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(op, arg);
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(op as u8);
        self.code.push((arg >> 8) as u8);
        self.code.push((arg & 0xFF) as u8);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        self.columns.push(col);
        self.columns.push(col);
        self.columns.push(col);
        if matches!(
            op,
            Op::GetProperty | Op::GetPropertyOpt | Op::MethodCallSpread | Op::ConcatAssignLocal
        ) {
            self.register_inline_cache(op_offset);
        }
        if op_reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit a local-slot property assignment:
    /// opcode + u16 property constant index + u16 local slot index.
    pub fn emit_set_local_slot_property(&mut self, prop_idx: u16, slot: u16, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(Op::SetLocalSlotProperty, 0);
        let col = self.current_col;
        self.code.push(Op::SetLocalSlotProperty as u8);
        self.code.push((prop_idx >> 8) as u8);
        self.code.push((prop_idx & 0xFF) as u8);
        self.code.push((slot >> 8) as u8);
        self.code.push((slot & 0xFF) as u8);
        for _ in 0..5 {
            self.lines.push(line);
            self.columns.push(col);
        }
    }

    /// Emit an instruction with a u8 argument.
    pub fn emit_u8(&mut self, op: Op, arg: u8, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(op, arg as u16);
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(op as u8);
        self.code.push(arg);
        self.lines.push(line);
        self.lines.push(line);
        self.columns.push(col);
        self.columns.push(col);
        if matches!(op, Op::Call) {
            self.register_inline_cache(op_offset);
        }
        if op_reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit a direct builtin call.
    pub fn emit_call_builtin(
        &mut self,
        id: crate::BuiltinId,
        name_idx: u16,
        arg_count: u8,
        line: u32,
    ) {
        #[cfg(debug_assertions)]
        self.note_balance(Op::CallBuiltin, arg_count as u16);
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(Op::CallBuiltin as u8);
        self.code.extend_from_slice(&id.raw().to_be_bytes());
        self.code.push((name_idx >> 8) as u8);
        self.code.push((name_idx & 0xFF) as u8);
        self.code.push(arg_count);
        for _ in 0..12 {
            self.lines.push(line);
            self.columns.push(col);
        }
        self.register_inline_cache(op_offset);
        self.references_outer_names = true;
    }

    /// Emit a direct builtin spread call.
    pub fn emit_call_builtin_spread(&mut self, id: crate::BuiltinId, name_idx: u16, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(Op::CallBuiltinSpread, 0);
        let col = self.current_col;
        self.code.push(Op::CallBuiltinSpread as u8);
        self.code.extend_from_slice(&id.raw().to_be_bytes());
        self.code.push((name_idx >> 8) as u8);
        self.code.push((name_idx & 0xFF) as u8);
        for _ in 0..11 {
            self.lines.push(line);
            self.columns.push(col);
        }
        self.references_outer_names = true;
    }

    /// Emit a method call: op + u16 (method name) + u8 (arg count).
    pub fn emit_method_call(&mut self, name_idx: u16, arg_count: u8, line: u32) {
        self.emit_method_call_inner(Op::MethodCall, name_idx, arg_count, line);
    }

    /// Emit an optional method call (?.) — returns nil if receiver is nil.
    pub fn emit_method_call_opt(&mut self, name_idx: u16, arg_count: u8, line: u32) {
        self.emit_method_call_inner(Op::MethodCallOpt, name_idx, arg_count, line);
    }

    fn emit_method_call_inner(&mut self, op: Op, name_idx: u16, arg_count: u8, line: u32) {
        #[cfg(debug_assertions)]
        self.note_balance(op, arg_count as u16);
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(op as u8);
        self.code.push((name_idx >> 8) as u8);
        self.code.push((name_idx & 0xFF) as u8);
        self.code.push(arg_count);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        self.columns.push(col);
        self.columns.push(col);
        self.columns.push(col);
        self.columns.push(col);
        self.register_inline_cache(op_offset);
    }

    /// Current code offset (for jump patching).
    pub fn current_offset(&self) -> usize {
        self.code.len()
    }

    /// Emit a jump instruction with a placeholder offset. Returns the position to patch.
    pub fn emit_jump(&mut self, op: Op, line: u32) -> usize {
        #[cfg(debug_assertions)]
        self.note_balance(op, 0);
        let col = self.current_col;
        self.code.push(op as u8);
        let patch_pos = self.code.len();
        self.code.push(0xFF);
        self.code.push(0xFF);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        self.columns.push(col);
        self.columns.push(col);
        self.columns.push(col);
        patch_pos
    }

    /// Patch a jump instruction at the given position to jump to the current offset.
    pub fn patch_jump(&mut self, patch_pos: usize) {
        let target = self.code.len() as u16;
        self.code[patch_pos] = (target >> 8) as u8;
        self.code[patch_pos + 1] = (target & 0xFF) as u8;
    }

    /// Patch a jump to a specific target position.
    pub fn patch_jump_to(&mut self, patch_pos: usize, target: usize) {
        let target = target as u16;
        self.code[patch_pos] = (target >> 8) as u8;
        self.code[patch_pos + 1] = (target & 0xFF) as u8;
    }

    /// Read a u16 argument at the given position.
    pub fn read_u16(&self, pos: usize) -> u16 {
        ((self.code[pos] as u16) << 8) | (self.code[pos + 1] as u16)
    }

    /// Fold one just-emitted opcode into the compile-time operand-stack
    /// balance model (issue #2622). See [`op_stack_delta`] for the
    /// linear-vs-non-linear classification.
    #[cfg(debug_assertions)]
    fn note_balance(&mut self, op: Op, count: u16) {
        match op_stack_delta(op, count) {
            Some(delta) => self.balance_depth += delta,
            None => self.balance_nonlinear += 1,
        }
    }

    /// Snapshot the balance model before compiling a statement; pair with
    /// [`Chunk::balance_delta_since`].
    #[cfg(debug_assertions)]
    pub(crate) fn balance_probe(&self) -> BalanceProbe {
        BalanceProbe {
            depth: self.balance_depth,
            nonlinear: self.balance_nonlinear,
        }
    }

    /// Net operand-stack effect emitted since `probe`, or `None` when any
    /// non-linearly-modeled opcode was emitted in that span (which makes
    /// the running sum untrustworthy, so callers must not assert on it).
    /// The absolute `balance_depth` may be meaningless after a non-exact
    /// span — only deltas over a fully-exact span are valid.
    #[cfg(debug_assertions)]
    pub(crate) fn balance_delta_since(&self, probe: BalanceProbe) -> Option<i32> {
        if self.balance_nonlinear == probe.nonlinear {
            Some(self.balance_depth - probe.depth)
        } else {
            None
        }
    }

    fn register_inline_cache(&mut self, op_offset: usize) {
        if self.inline_cache_slots.contains_key(&op_offset) {
            return;
        }
        let mut entries = self.inline_caches.lock();
        let slot = entries.len();
        entries.push(InlineCacheEntry::Empty);
        self.inline_cache_slots.insert(op_offset, slot);
        Self::write_inline_cache_index(&mut self.inline_cache_index, op_offset, slot);
    }

    /// Fast-path side-table writer. Pulled out as an associated fn so both
    /// the live emit path and [`Chunk::from_cached`] share the same growth
    /// strategy. Cache slots fit comfortably in `u32` because the slot count
    /// is bounded by the cacheable-opcode count in `code`.
    fn write_inline_cache_index(index: &mut Vec<u32>, op_offset: usize, slot: usize) {
        if op_offset >= index.len() {
            index.resize(op_offset + 1, NO_INLINE_CACHE_SLOT);
        }
        index[op_offset] = slot as u32;
    }

    /// Look up the inline-cache slot for the opcode at `op_offset`. This is
    /// called on every dispatch of an adaptive binary op (Add/Sub/Mul/Div/
    /// Mod/Eq/Neq/Less/Greater/LessEq/GreaterEq), `Op::Call`, `Op::MethodCall`
    /// (and `MethodCallOpt`/`MethodCallSpread`), and `Op::GetProperty`
    /// (`GetPropertyOpt`). Backed by [`Chunk::inline_cache_index`] — a flat
    /// `Vec<u32>` indexed by code offset — so the lookup is a single bounds-
    /// checked array read instead of the prior `BTreeMap::get` which walked
    /// internal nodes for every dispatched op.
    #[inline]
    pub(crate) fn inline_cache_slot(&self, op_offset: usize) -> Option<usize> {
        match self.inline_cache_index.get(op_offset).copied() {
            None | Some(NO_INLINE_CACHE_SLOT) => None,
            Some(slot) => Some(slot as usize),
        }
    }

    pub(crate) fn inline_cache_slot_count(&self) -> usize {
        self.inline_cache_slots.len()
    }

    pub(crate) fn cache_id(&self) -> u64 {
        self.cache_id
    }

    /// Pre-optimization control path: the `BTreeMap`-backed lookup the
    /// dispatcher used before the flat `Vec<u32>` side-table. Exposed
    /// only behind the `vm-bench-internals` feature so the criterion
    /// microbench can A/B the two paths inside one binary on identical
    /// hardware. The production hot path must keep using
    /// [`Chunk::inline_cache_slot`].
    #[cfg(feature = "vm-bench-internals")]
    pub fn inline_cache_slot_via_btreemap_for_bench(&self, op_offset: usize) -> Option<usize> {
        self.inline_cache_slots.get(&op_offset).copied()
    }

    /// Returns a shared string for a `Constant::String` at the given pool
    /// index, materializing it on first access and caching for reuse.
    /// Returns `None` when the constant at `idx` is not a string (the
    /// caller should fall back to the regular `Constant` match).
    pub(crate) fn constant_string_rc(&self, idx: usize) -> Option<crate::value::HarnStr> {
        // Borrow the side table mutably so we can lazily extend / fill
        // entries. The borrow is scope-confined to this function; the
        // VM never re-enters constant_string_rc for the same chunk
        // during a single materialization, so no nested-borrow risk.
        let mut entries = self.constant_strings.lock();
        if entries.len() < self.constants.len() {
            entries.resize(self.constants.len(), None);
        }
        if let Some(Some(existing)) = entries.get(idx) {
            return Some(existing.clone());
        }
        let materialized = match self.constants.get(idx)? {
            Constant::String(s) => crate::value::HarnStr::from(s.as_str()),
            _ => return None,
        };
        entries[idx] = Some(materialized.clone());
        Some(materialized)
    }

    /// Test helper for the chunk-local scratch inline cache. Production
    /// dispatch reads VM-local cache sets through `Vm`.
    #[inline]
    #[cfg(test)]
    pub(crate) fn peek_adaptive_binary_cache(
        &self,
        slot: usize,
    ) -> Option<(AdaptiveBinaryOp, AdaptiveBinaryState)> {
        match self.inline_caches.lock().get(slot)? {
            &InlineCacheEntry::AdaptiveBinary { op, state } => Some((op, state)),
            _ => None,
        }
    }

    /// Test helper for the chunk-local scratch inline cache. Production
    /// dispatch reads VM-local cache sets through `Vm`.
    #[inline]
    #[cfg(test)]
    pub(crate) fn peek_method_cache(&self, slot: usize) -> Option<(u16, usize, MethodCacheTarget)> {
        match self.inline_caches.lock().get(slot)? {
            &InlineCacheEntry::Method {
                name_idx,
                argc,
                target,
            } => Some((name_idx, argc, target)),
            _ => None,
        }
    }

    /// Test helper for the chunk-local scratch inline cache. Production
    /// dispatch reads VM-local cache sets through `Vm`.
    #[inline]
    #[cfg(test)]
    pub(crate) fn peek_property_cache(&self, slot: usize) -> Option<(u16, PropertyCacheTarget)> {
        match self.inline_caches.lock().get(slot)? {
            InlineCacheEntry::Property { name_idx, target } => Some((*name_idx, target.clone())),
            _ => None,
        }
    }

    /// Test helper for the chunk-local scratch inline cache. Production
    /// dispatch reads VM-local cache sets through `Vm`.
    #[inline]
    #[cfg(test)]
    pub(crate) fn peek_direct_call_state(&self, slot: usize) -> Option<DirectCallState> {
        match self.inline_caches.lock().get(slot)? {
            InlineCacheEntry::DirectCall { state } => Some(state.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_inline_cache_entry(&self, slot: usize, entry: InlineCacheEntry) {
        if let Some(existing) = self.inline_caches.lock().get_mut(slot) {
            *existing = entry;
        }
    }

    pub fn freeze_for_cache(&self) -> CachedChunk {
        CachedChunk {
            code: self.code.clone(),
            constants: self.constants.clone(),
            lines: self.lines.clone(),
            columns: self.columns.clone(),
            source_file: self.source_file.clone(),
            current_col: self.current_col,
            functions: self
                .functions
                .iter()
                .map(|function| function.freeze_for_cache())
                .collect(),
            inline_cache_slots: self.inline_cache_slots.clone(),
            local_slots: self.local_slots.clone(),
            references_outer_names: self.references_outer_names,
        }
    }

    pub fn from_cached(cached: CachedChunk) -> Self {
        let CachedChunk {
            code,
            constants,
            lines,
            columns,
            source_file,
            current_col,
            functions,
            inline_cache_slots,
            local_slots,
            references_outer_names,
        } = cached;
        let inline_cache_count = inline_cache_slots.len();
        let constants_count = constants.len();
        // Project the cached `BTreeMap<op_offset, slot>` into the flat
        // dispatch-side lookup table. Sized to `code.len()` so the hottest
        // hot opcodes (binary ops at the end of a long chunk) still hit the
        // fast-path bounds check rather than falling through to the
        // none-found branch. The size is bounded by code length, so the
        // memory footprint is tiny — a few KB for typical chunks.
        let mut inline_cache_index = Vec::new();
        inline_cache_index.resize(code.len(), NO_INLINE_CACHE_SLOT);
        for (&op_offset, &slot) in &inline_cache_slots {
            if op_offset < inline_cache_index.len() {
                inline_cache_index[op_offset] = slot as u32;
            }
        }
        let constant_index = build_constant_index(&constants);
        Self {
            cache_id: next_chunk_cache_id(),
            code,
            constants,
            constant_index,
            lines,
            columns,
            source_file,
            current_col,
            functions: functions
                .into_iter()
                .map(|function| Arc::new(CompiledFunction::from_cached(function)))
                .collect(),
            inline_cache_slots,
            inline_cache_index,
            inline_caches: Arc::new(Mutex::new(vec![
                InlineCacheEntry::Empty;
                inline_cache_count
            ])),
            constant_strings: Arc::new(Mutex::new(vec![None; constants_count])),
            local_slots,
            references_outer_names,
            #[cfg(debug_assertions)]
            balance_depth: 0,
            #[cfg(debug_assertions)]
            balance_nonlinear: 0,
        }
    }

    pub(crate) fn add_local_slot(
        &mut self,
        name: String,
        mutable: bool,
        scope_depth: usize,
    ) -> u16 {
        let idx = self.local_slots.len();
        self.local_slots.push(LocalSlotInfo {
            name,
            mutable,
            scope_depth,
        });
        idx as u16
    }

    /// Read a u64 argument at the given position.
    pub fn read_u64(&self, pos: usize) -> u64 {
        u64::from_be_bytes([
            self.code[pos],
            self.code[pos + 1],
            self.code[pos + 2],
            self.code[pos + 3],
            self.code[pos + 4],
            self.code[pos + 5],
            self.code[pos + 6],
            self.code[pos + 7],
        ])
    }

    /// Disassemble the chunk for debugging. The per-opcode rendering is
    /// macro-generated alongside the dispatch tables in
    /// `crate::vm::ops` — see [`Self::disassemble_op`].
    pub fn disassemble(&self, name: &str) -> String {
        let mut out = format!("== {name} ==\n");
        let mut ip = 0;
        while ip < self.code.len() {
            let op_byte = self.code[ip];
            let line = self.lines.get(ip).copied().unwrap_or(0);
            out.push_str(&format!("{ip:04} [{line:>4}] "));
            ip += 1;

            if let Some(op) = Op::from_byte(op_byte) {
                self.disassemble_op(op, &mut ip, &mut out);
            } else {
                out.push_str(&format!("UNKNOWN(0x{op_byte:02x})\n"));
            }
        }
        out
    }
}

/// Disassembly helpers consumed by the macro-generated
/// [`Chunk::disassemble_op`]. Each helper takes the current code position
/// (already advanced past the opcode byte), advances it over the operand
/// bytes the opcode carries, and renders one human-readable line without
/// a trailing newline (the dispatcher appends it).
///
/// Defining one helper per operand layout — and not one per opcode —
/// keeps adding an opcode a one-line edit in the `define_opcodes!` table
/// rather than a paired edit here. New layouts live with the helpers;
/// new opcodes live with the dispatch.
pub(crate) fn disasm_bare(_chunk: &Chunk, _ip: &mut usize, label: &str) -> String {
    label.to_string()
}

pub(crate) fn disasm_u8(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let arg = chunk.code[*ip];
    *ip += 1;
    format!("{label} {arg:>4}")
}

pub(crate) fn disasm_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let arg = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {arg:>4}")
}

pub(crate) fn disasm_try_catch_setup(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let catch_offset = chunk.read_u16(*ip);
    *ip += 2;
    let type_idx = chunk.read_u16(*ip);
    *ip += 2;
    if let Some(type_name) = chunk.constants.get(type_idx as usize) {
        format!("{label} {catch_offset:>4} type {type_idx:>4} ({type_name})")
    } else {
        format!("{label} {catch_offset:>4} type {type_idx:>4}")
    }
}

pub(crate) fn disasm_const_pool_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {idx:>4} ({})", chunk.constants[idx as usize])
}

pub(crate) fn disasm_local_slot_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let slot = chunk.read_u16(*ip);
    *ip += 2;
    let mut out = format!("{label} {slot:>4}");
    if let Some(info) = chunk.local_slots.get(slot as usize) {
        out.push_str(&format!(" ({})", info.name));
    }
    out
}

pub(crate) fn disasm_const_pool_local_slot(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let prop = chunk.read_u16(*ip);
    *ip += 2;
    let slot = chunk.read_u16(*ip);
    *ip += 2;
    let mut out = format!(
        "{label} prop {prop:>4} ({}) slot {slot:>4}",
        chunk.constants[prop as usize]
    );
    if let Some(info) = chunk.local_slots.get(slot as usize) {
        out.push_str(&format!(" ({})", info.name));
    }
    out
}

pub(crate) fn disasm_method_call(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    let argc = chunk.code[*ip];
    *ip += 1;
    format!(
        "{label} {idx:>4} ({}) argc={argc}",
        chunk.constants[idx as usize]
    )
}

pub(crate) fn disasm_match_enum(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let enum_idx = chunk.read_u16(*ip);
    *ip += 2;
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {enum_idx:>4} ({}) {var_idx:>4} ({})",
        chunk.constants[enum_idx as usize], chunk.constants[var_idx as usize],
    )
}

pub(crate) fn disasm_build_enum(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let enum_idx = chunk.read_u16(*ip);
    *ip += 2;
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    let field_count = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {enum_idx:>4} ({}) {var_idx:>4} ({}) fields={field_count}",
        chunk.constants[enum_idx as usize], chunk.constants[var_idx as usize],
    )
}

pub(crate) fn disasm_selective_import(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let path_idx = chunk.read_u16(*ip);
    *ip += 2;
    let names_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {path_idx:>4} ({}) names: {names_idx:>4} ({})",
        chunk.constants[path_idx as usize], chunk.constants[names_idx as usize],
    )
}

pub(crate) fn disasm_check_type(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    let type_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {var_idx:>4} ({}) -> {type_idx:>4} ({})",
        chunk.constants[var_idx as usize], chunk.constants[type_idx as usize],
    )
}

pub(crate) fn disasm_call_builtin(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let id = chunk.read_u64(*ip);
    *ip += 8;
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    let argc = chunk.code[*ip];
    *ip += 1;
    format!(
        "{label} {id:#018x} {idx:>4} ({}) argc={argc}",
        chunk.constants[idx as usize],
    )
}

pub(crate) fn disasm_call_builtin_spread(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let id = chunk.read_u64(*ip);
    *ip += 8;
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {id:#018x} {idx:>4} ({})",
        chunk.constants[idx as usize],
    )
}

pub(crate) fn disasm_method_call_spread(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    // emit_u16(Op::MethodCallSpread, name_idx, ...) writes opcode + 2
    // bytes of u16 name_idx, so the operand is read at *ip with the
    // usual `read_u16`. The previous hand-written disasm read at
    // `ip + 1`, which displayed the wrong constant index — silently
    // corrupting any disassembly that hit a `MethodCallSpread` opcode.
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {idx:>4} ({})", chunk.constants[idx as usize])
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "chunk_tests.rs"]
mod tests;
