use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use harn_parser::TypeExpr;
use serde::{Deserialize, Serialize};

use crate::runtime_guards::RuntimeParamGuard;

/// Sentinel value stored in [`Chunk::inline_cache_index`] for code offsets
/// that have no inline-cache slot registered. Chosen as `u32::MAX` so the
/// hot dispatch path can treat the side-table as a flat `Vec<u32>` without
/// an `Option` wrapper — the comparison against the sentinel collapses to a
/// single integer compare. The compile-time max useful slot count is bounded
/// by code length (one slot per cacheable opcode), so `u32::MAX` is safely
/// out of the addressable slot range.
pub(crate) const NO_INLINE_CACHE_SLOT: u32 = u32::MAX;

/// Bytecode opcodes for the Harn VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Push a constant from the constant pool onto the stack.
    Constant, // arg: u16 constant index
    /// Push nil onto the stack.
    Nil,
    /// Push true onto the stack.
    True,
    /// Push false onto the stack.
    False,

    // --- Variable operations ---
    /// Get a variable by name (from constant pool).
    GetVar, // arg: u16 constant index (name)
    /// Define a new immutable variable. Pops value from stack.
    DefLet, // arg: u16 constant index (name)
    /// Define a new mutable variable. Pops value from stack.
    DefVar, // arg: u16 constant index (name)
    /// Assign to an existing mutable variable. Pops value from stack.
    SetVar, // arg: u16 constant index (name)
    /// Push a new lexical scope onto the environment stack.
    PushScope,
    /// Pop the current lexical scope from the environment stack.
    PopScope,

    // --- Arithmetic ---
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Negate,

    // --- Comparison ---
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,

    // --- Logical ---
    Not,

    // --- Control flow ---
    /// Jump unconditionally. arg: u16 offset.
    Jump,
    /// Jump if top of stack is falsy. Does not pop. arg: u16 offset.
    JumpIfFalse,
    /// Jump if top of stack is truthy. Does not pop. arg: u16 offset.
    JumpIfTrue,
    /// Pop top of stack (discard).
    Pop,

    // --- Functions ---
    /// Call a function/builtin. arg: u8 = arg count. Name is on stack below args.
    Call,
    /// Tail call: like Call, but replaces the current frame instead of pushing
    /// a new one. Used for `return f(x)` to enable tail call optimization.
    /// For builtins, behaves like a regular Call (no frame to replace).
    TailCall,
    /// Return from current function. Pops return value.
    Return,
    /// Create a closure. arg: u16 = chunk index in function table.
    Closure,

    // --- Collections ---
    /// Build a list. arg: u16 = element count. Elements are on stack.
    BuildList,
    /// Build a dict. arg: u16 = entry count. Key-value pairs on stack.
    BuildDict,
    /// Subscript access: stack has [object, index]. Pushes result.
    Subscript,
    /// Optional subscript (`obj?[index]`). Like `Subscript` but pushes nil
    /// instead of indexing when the object is nil.
    SubscriptOpt,
    /// Slice access: stack has [object, start_or_nil, end_or_nil]. Pushes sublist/substring.
    Slice,

    // --- Object operations ---
    /// Property access. arg: u16 = constant index (property name).
    GetProperty,
    /// Optional property access (?.). Like GetProperty but returns nil
    /// instead of erroring when the object is nil. arg: u16 = constant index.
    GetPropertyOpt,
    /// Property assignment. arg: u16 = constant index (property name).
    /// Stack: [value] → assigns to the named variable's property.
    SetProperty,
    /// Subscript assignment. arg: u16 = constant index (variable name).
    /// Stack: [index, value] → assigns to variable[index] = value.
    SetSubscript,
    /// Method call. arg1: u16 = constant index (method name), arg2: u8 = arg count.
    MethodCall,
    /// Optional method call (?.). Like MethodCall but returns nil if the
    /// receiver is nil instead of dispatching. arg1: u16, arg2: u8.
    MethodCallOpt,

    // --- String ---
    /// String concatenation of N parts. arg: u16 = part count.
    Concat,

    // --- Iteration ---
    /// Set up a for-in loop. Expects iterable on stack. Pushes iterator state.
    IterInit,
    /// Advance iterator. If exhausted, jumps. arg: u16 = jump offset.
    /// Pushes next value and the variable name is set via DefVar before the loop.
    IterNext,

    // --- Pipe ---
    /// Pipe: pops [value, callable], invokes callable(value).
    Pipe,

    // --- Error handling ---
    /// Pop value, raise as error.
    Throw,
    /// Push exception handler. arg: u16 = offset to catch handler.
    TryCatchSetup,
    /// Remove top exception handler (end of try body).
    PopHandler,

    // --- Concurrency ---
    /// Execute closure N times sequentially, push results as list.
    /// Stack: count, closure → result_list
    Parallel,
    /// Execute closure for each item in list, push results as list.
    /// Stack: list, closure → result_list
    ParallelMap,
    /// Execute closure for each item in list, push a stream that emits in completion order.
    /// Stack: list, closure → stream
    ParallelMapStream,
    /// Like ParallelMap but wraps each result in Result.Ok/Err, never fails.
    /// Stack: list, closure → {results: [Result], succeeded: int, failed: int}
    ParallelSettle,
    /// Store closure for deferred execution, push TaskHandle.
    /// Stack: closure → TaskHandle
    Spawn,
    /// Acquire a process-local mutex for the current lexical scope.
    /// arg: u16 constant index (key string).
    SyncMutexEnter,

    // --- Imports ---
    /// Import a file. arg: u16 = constant index (path string).
    Import,
    /// Selective import. arg1: u16 = path string, arg2: u16 = names list constant.
    SelectiveImport,

    // --- Deadline ---
    /// Pop duration value, push deadline onto internal deadline stack.
    DeadlineSetup,
    /// Pop deadline from internal deadline stack.
    DeadlineEnd,

    // --- Enum ---
    /// Build an enum variant value.
    /// arg1: u16 = constant index (enum name), arg2: u16 = constant index (variant name),
    /// arg3: u16 = field count. Fields are on stack.
    BuildEnum,

    // --- Match ---
    /// Match an enum pattern. Checks enum_name + variant on the top of stack (dup'd match value).
    /// arg1: u16 = constant index (enum name), arg2: u16 = constant index (variant name).
    /// If match succeeds, pushes true; else pushes false.
    MatchEnum,

    // --- Loop control ---
    /// Pop the top iterator from the iterator stack (cleanup on break from for-in).
    PopIterator,

    // --- Defaults ---
    /// Push the number of arguments passed to the current function call.
    GetArgc,

    // --- Type checking ---
    /// Runtime type check on a variable.
    /// arg1: u16 = constant index (variable name),
    /// arg2: u16 = constant index (expected type name).
    /// Throws a TypeError if the variable's type doesn't match.
    CheckType,

    // --- Result try operator ---
    /// Try-unwrap: if top is Result.Ok(v), replace with v. If Result.Err(e), return it.
    TryUnwrap,
    /// Wrap top of stack in Result.Ok unless it is already a Result.
    TryWrapOk,

    // --- Spread call ---
    /// Call with spread arguments. Stack: [callee, args_list] -> result.
    CallSpread,
    /// Direct builtin call. Followed by u64 builtin ID, u16 name constant, u8 arg count.
    /// Runtime still checks closure shadowing before using the ID.
    CallBuiltin,
    /// Direct builtin spread call. Followed by u64 builtin ID and u16 name constant.
    /// Stack: [args_list] -> result.
    CallBuiltinSpread,
    /// Method call with spread arguments. Stack: [object, args_list] -> result.
    /// Followed by 2 bytes for method name constant index.
    MethodCallSpread,

    // --- Misc ---
    /// Duplicate top of stack.
    Dup,
    /// Swap top two stack values.
    Swap,
    /// Membership test: stack has [item, collection]. Pushes bool.
    /// Works for lists (item in list), dicts (key in dict), strings (substr in string), and sets.
    Contains,

    // --- Typed arithmetic/comparison fast paths ---
    AddInt,
    SubInt,
    MulInt,
    DivInt,
    ModInt,
    AddFloat,
    SubFloat,
    MulFloat,
    DivFloat,
    ModFloat,
    EqualInt,
    NotEqualInt,
    LessInt,
    GreaterInt,
    LessEqualInt,
    GreaterEqualInt,
    EqualFloat,
    NotEqualFloat,
    LessFloat,
    GreaterFloat,
    LessEqualFloat,
    GreaterEqualFloat,
    EqualBool,
    NotEqualBool,
    EqualString,
    NotEqualString,

    /// Yield a value from a generator. Pops value, sends through channel, suspends.
    Yield,

    // --- Slot-indexed locals ---
    /// Get a frame-local slot. arg: u16 slot index.
    GetLocalSlot,
    /// Define or initialize a frame-local slot. Pops value from stack.
    DefLocalSlot,
    /// Assign an existing frame-local slot. Pops value from stack.
    SetLocalSlot,
}

impl Op {
    pub(crate) const ALL: &'static [Self] = &[
        Op::Constant,
        Op::Nil,
        Op::True,
        Op::False,
        Op::GetVar,
        Op::DefLet,
        Op::DefVar,
        Op::SetVar,
        Op::PushScope,
        Op::PopScope,
        Op::Add,
        Op::Sub,
        Op::Mul,
        Op::Div,
        Op::Mod,
        Op::Pow,
        Op::Negate,
        Op::Equal,
        Op::NotEqual,
        Op::Less,
        Op::Greater,
        Op::LessEqual,
        Op::GreaterEqual,
        Op::Not,
        Op::Jump,
        Op::JumpIfFalse,
        Op::JumpIfTrue,
        Op::Pop,
        Op::Call,
        Op::TailCall,
        Op::Return,
        Op::Closure,
        Op::BuildList,
        Op::BuildDict,
        Op::Subscript,
        Op::SubscriptOpt,
        Op::Slice,
        Op::GetProperty,
        Op::GetPropertyOpt,
        Op::SetProperty,
        Op::SetSubscript,
        Op::MethodCall,
        Op::MethodCallOpt,
        Op::Concat,
        Op::IterInit,
        Op::IterNext,
        Op::Pipe,
        Op::Throw,
        Op::TryCatchSetup,
        Op::PopHandler,
        Op::Parallel,
        Op::ParallelMap,
        Op::ParallelMapStream,
        Op::ParallelSettle,
        Op::Spawn,
        Op::SyncMutexEnter,
        Op::Import,
        Op::SelectiveImport,
        Op::DeadlineSetup,
        Op::DeadlineEnd,
        Op::BuildEnum,
        Op::MatchEnum,
        Op::PopIterator,
        Op::GetArgc,
        Op::CheckType,
        Op::TryUnwrap,
        Op::TryWrapOk,
        Op::CallSpread,
        Op::CallBuiltin,
        Op::CallBuiltinSpread,
        Op::MethodCallSpread,
        Op::Dup,
        Op::Swap,
        Op::Contains,
        Op::AddInt,
        Op::SubInt,
        Op::MulInt,
        Op::DivInt,
        Op::ModInt,
        Op::AddFloat,
        Op::SubFloat,
        Op::MulFloat,
        Op::DivFloat,
        Op::ModFloat,
        Op::EqualInt,
        Op::NotEqualInt,
        Op::LessInt,
        Op::GreaterInt,
        Op::LessEqualInt,
        Op::GreaterEqualInt,
        Op::EqualFloat,
        Op::NotEqualFloat,
        Op::LessFloat,
        Op::GreaterFloat,
        Op::LessEqualFloat,
        Op::GreaterEqualFloat,
        Op::EqualBool,
        Op::NotEqualBool,
        Op::EqualString,
        Op::NotEqualString,
        Op::Yield,
        Op::GetLocalSlot,
        Op::DefLocalSlot,
        Op::SetLocalSlot,
    ];

    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(byte as usize).copied()
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    Closure(Rc<crate::value::VmClosure>),
}

impl PartialEq for DirectCallTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Closure(left), Self::Closure(right)) => Rc::ptr_eq(left, right),
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
    DictField(Rc<str>),
    StructField { field_name: Rc<str>, index: usize },
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
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The bytecode instructions.
    pub code: Vec<u8>,
    /// Constant pool.
    pub constants: Vec<Constant>,
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
    /// Shared cache entries so cloned chunks in call frames warm the same side
    /// table as the compiled chunk used by tests/debugging.
    inline_caches: Rc<RefCell<Vec<InlineCacheEntry>>>,
    /// Lazily-materialized `Rc<str>` cache for `Constant::String` entries,
    /// parallel to `constants`. `Op::Constant` for a string used to run
    /// `Rc::from(s.as_str())` on every execution, allocating a fresh
    /// `Rc<str>` per push — death by a thousand allocations for
    /// string-interpolation-heavy hot paths. With this side table the
    /// allocation happens once per unique constant; subsequent pushes
    /// are an Rc refcount bump.
    constant_strings: Rc<RefCell<Vec<Option<Rc<str>>>>>,
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
}

pub type ChunkRef = Rc<Chunk>;
pub type CompiledFunctionRef = Rc<CompiledFunction>;

/// Serializable snapshot of a [`Chunk`] suitable for the on-disk bytecode
/// cache and for in-memory stdlib artifact caches. Inline-cache state is
/// dropped at freeze time because it warms at runtime per-process; the
/// rest of the chunk round-trips byte-identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedParamSlot {
    pub(crate) name: String,
    pub(crate) type_expr: Option<TypeExpr>,
    pub(crate) has_default: bool,
}

impl CachedParamSlot {
    fn thaw(&self) -> ParamSlot {
        ParamSlot {
            name: self.name.clone(),
            type_expr: self.type_expr.clone(),
            runtime_guard: self
                .type_expr
                .as_ref()
                .map(RuntimeParamGuard::from_type_expr),
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
        Self {
            name: param.name.clone(),
            type_expr: param.type_expr.clone(),
            runtime_guard: param
                .type_expr
                .as_ref()
                .map(RuntimeParamGuard::from_type_expr),
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

    pub(crate) fn from_cached(cached: &CachedCompiledFunction) -> Self {
        Self {
            name: cached.name.clone(),
            type_params: cached.type_params.clone(),
            nominal_type_names: cached.nominal_type_names.clone(),
            params: cached.params.iter().map(CachedParamSlot::thaw).collect(),
            default_start: cached.default_start,
            chunk: Rc::new(Chunk::from_cached(&cached.chunk)),
            is_generator: cached.is_generator,
            is_stream: cached.is_stream,
            has_rest_param: cached.has_rest_param,
            has_runtime_type_checks: cached.has_runtime_type_checks,
        }
    }
}

impl Chunk {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            lines: Vec::new(),
            columns: Vec::new(),
            source_file: None,
            current_col: 0,
            functions: Vec::new(),
            inline_cache_slots: BTreeMap::new(),
            inline_cache_index: Vec::new(),
            inline_caches: Rc::new(RefCell::new(Vec::new())),
            constant_strings: Rc::new(RefCell::new(Vec::new())),
            local_slots: Vec::new(),
            references_outer_names: false,
        }
    }

    /// Opcodes that perform a runtime env-based name lookup or
    /// assignment. Emitting any of these marks the chunk as needing the
    /// caller-scope late-bind walk in [`Vm::closure_call_env`].
    ///
    /// `Op::Call` / `Op::TailCall` / `Op::Pipe` make the list because
    /// the compiler emits `Op::Constant("name") + Op::TailCall` for
    /// `return fn_name(...)` (see `compile_return` in
    /// `compiler/statements.rs`) — the callee is materialized on the
    /// stack as a String and resolved through
    /// [`Vm::resolve_named_closure`] at dispatch time, which is exactly
    /// the path the walk feeds. Excluding them would silently break
    /// mutual recursion across a tail-call boundary.
    #[inline]
    pub(crate) fn op_reads_outer_name(op: Op) -> bool {
        matches!(
            op,
            Op::GetVar
                | Op::SetVar
                | Op::CallBuiltin
                | Op::CallBuiltinSpread
                | Op::CallSpread
                | Op::Call
                | Op::TailCall
                | Op::Pipe
                | Op::CheckType
        )
    }

    /// Set the current column for subsequent emit calls.
    pub fn set_column(&mut self, col: u32) {
        self.current_col = col;
    }

    /// Add a constant and return its index.
    pub fn add_constant(&mut self, constant: Constant) -> u16 {
        for (i, c) in self.constants.iter().enumerate() {
            if c == &constant {
                return i as u16;
            }
        }
        let idx = self.constants.len();
        self.constants.push(constant);
        idx as u16
    }

    /// Emit a single-byte instruction.
    pub fn emit(&mut self, op: Op, line: u32) {
        let col = self.current_col;
        let op_offset = self.code.len();
        self.code.push(op as u8);
        self.lines.push(line);
        self.columns.push(col);
        if is_adaptive_binary_op(op) {
            self.register_inline_cache(op_offset);
        }
        if Self::op_reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit an instruction with a u16 argument.
    pub fn emit_u16(&mut self, op: Op, arg: u16, line: u32) {
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
            Op::GetProperty | Op::GetPropertyOpt | Op::MethodCallSpread
        ) {
            self.register_inline_cache(op_offset);
        }
        if Self::op_reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit an instruction with a u8 argument.
    pub fn emit_u8(&mut self, op: Op, arg: u8, line: u32) {
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
        if Self::op_reads_outer_name(op) {
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

    fn register_inline_cache(&mut self, op_offset: usize) {
        if self.inline_cache_slots.contains_key(&op_offset) {
            return;
        }
        let mut entries = self.inline_caches.borrow_mut();
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

    /// Returns an `Rc<str>` for a `Constant::String` at the given pool
    /// index, materializing it on first access and caching for reuse.
    /// Returns `None` when the constant at `idx` is not a string (the
    /// caller should fall back to the regular `Constant` match).
    pub(crate) fn constant_string_rc(&self, idx: usize) -> Option<Rc<str>> {
        // Borrow the side table mutably so we can lazily extend / fill
        // entries. The borrow is scope-confined to this function; the
        // VM never re-enters constant_string_rc for the same chunk
        // during a single materialization, so no nested-borrow risk.
        let mut entries = self.constant_strings.borrow_mut();
        if entries.len() < self.constants.len() {
            entries.resize(self.constants.len(), None);
        }
        if let Some(Some(existing)) = entries.get(idx) {
            return Some(Rc::clone(existing));
        }
        let materialized = match self.constants.get(idx)? {
            Constant::String(s) => Rc::<str>::from(s.as_str()),
            _ => return None,
        };
        entries[idx] = Some(Rc::clone(&materialized));
        Some(materialized)
    }

    pub(crate) fn inline_cache_entry(&self, slot: usize) -> InlineCacheEntry {
        self.inline_caches
            .borrow()
            .get(slot)
            .cloned()
            .unwrap_or(InlineCacheEntry::Empty)
    }

    pub(crate) fn set_inline_cache_entry(&self, slot: usize, entry: InlineCacheEntry) {
        if let Some(existing) = self.inline_caches.borrow_mut().get_mut(slot) {
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

    pub fn from_cached(cached: &CachedChunk) -> Self {
        let inline_cache_count = cached.inline_cache_slots.len();
        let constants_count = cached.constants.len();
        // Project the cached `BTreeMap<op_offset, slot>` into the flat
        // dispatch-side lookup table. Sized to `code.len()` so the hottest
        // hot opcodes (binary ops at the end of a long chunk) still hit the
        // fast-path bounds check rather than falling through to the
        // none-found branch. The size is bounded by code length, so the
        // memory footprint is tiny — a few KB for typical chunks.
        let mut inline_cache_index = Vec::new();
        inline_cache_index.resize(cached.code.len(), NO_INLINE_CACHE_SLOT);
        for (&op_offset, &slot) in cached.inline_cache_slots.iter() {
            if op_offset < inline_cache_index.len() {
                inline_cache_index[op_offset] = slot as u32;
            }
        }
        Self {
            code: cached.code.clone(),
            constants: cached.constants.clone(),
            lines: cached.lines.clone(),
            columns: cached.columns.clone(),
            source_file: cached.source_file.clone(),
            current_col: cached.current_col,
            functions: cached
                .functions
                .iter()
                .map(|function| Rc::new(CompiledFunction::from_cached(function)))
                .collect(),
            inline_cache_slots: cached.inline_cache_slots.clone(),
            inline_cache_index,
            inline_caches: Rc::new(RefCell::new(vec![
                InlineCacheEntry::Empty;
                inline_cache_count
            ])),
            constant_strings: Rc::new(RefCell::new(vec![None; constants_count])),
            local_slots: cached.local_slots.clone(),
            references_outer_names: cached.references_outer_names,
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

    #[cfg(test)]
    pub(crate) fn inline_cache_entries(&self) -> Vec<InlineCacheEntry> {
        self.inline_caches.borrow().clone()
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

    /// Disassemble for debugging.
    pub fn disassemble(&self, name: &str) -> String {
        let mut out = format!("== {name} ==\n");
        let mut ip = 0;
        while ip < self.code.len() {
            let op = self.code[ip];
            let line = self.lines.get(ip).copied().unwrap_or(0);
            out.push_str(&format!("{ip:04} [{line:>4}] "));
            ip += 1;

            match op {
                x if x == Op::Constant as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    let val = &self.constants[idx as usize];
                    out.push_str(&format!("CONSTANT {idx:>4} ({val})\n"));
                }
                x if x == Op::Nil as u8 => out.push_str("NIL\n"),
                x if x == Op::True as u8 => out.push_str("TRUE\n"),
                x if x == Op::False as u8 => out.push_str("FALSE\n"),
                x if x == Op::GetVar as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "GET_VAR {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::DefLet as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "DEF_LET {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::DefVar as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "DEF_VAR {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::SetVar as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "SET_VAR {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::GetLocalSlot as u8 => {
                    let slot = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("GET_LOCAL_SLOT {slot:>4}"));
                    if let Some(info) = self.local_slots.get(slot as usize) {
                        out.push_str(&format!(" ({})", info.name));
                    }
                    out.push('\n');
                }
                x if x == Op::DefLocalSlot as u8 => {
                    let slot = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("DEF_LOCAL_SLOT {slot:>4}"));
                    if let Some(info) = self.local_slots.get(slot as usize) {
                        out.push_str(&format!(" ({})", info.name));
                    }
                    out.push('\n');
                }
                x if x == Op::SetLocalSlot as u8 => {
                    let slot = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("SET_LOCAL_SLOT {slot:>4}"));
                    if let Some(info) = self.local_slots.get(slot as usize) {
                        out.push_str(&format!(" ({})", info.name));
                    }
                    out.push('\n');
                }
                x if x == Op::PushScope as u8 => out.push_str("PUSH_SCOPE\n"),
                x if x == Op::PopScope as u8 => out.push_str("POP_SCOPE\n"),
                x if x == Op::Add as u8 => out.push_str("ADD\n"),
                x if x == Op::Sub as u8 => out.push_str("SUB\n"),
                x if x == Op::Mul as u8 => out.push_str("MUL\n"),
                x if x == Op::Div as u8 => out.push_str("DIV\n"),
                x if x == Op::Mod as u8 => out.push_str("MOD\n"),
                x if x == Op::Pow as u8 => out.push_str("POW\n"),
                x if x == Op::Negate as u8 => out.push_str("NEGATE\n"),
                x if x == Op::Equal as u8 => out.push_str("EQUAL\n"),
                x if x == Op::NotEqual as u8 => out.push_str("NOT_EQUAL\n"),
                x if x == Op::Less as u8 => out.push_str("LESS\n"),
                x if x == Op::Greater as u8 => out.push_str("GREATER\n"),
                x if x == Op::LessEqual as u8 => out.push_str("LESS_EQUAL\n"),
                x if x == Op::GreaterEqual as u8 => out.push_str("GREATER_EQUAL\n"),
                x if x == Op::Contains as u8 => out.push_str("CONTAINS\n"),
                x if x == Op::Not as u8 => out.push_str("NOT\n"),
                x if x == Op::Jump as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("JUMP {target:>4}\n"));
                }
                x if x == Op::JumpIfFalse as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("JUMP_IF_FALSE {target:>4}\n"));
                }
                x if x == Op::JumpIfTrue as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("JUMP_IF_TRUE {target:>4}\n"));
                }
                x if x == Op::Pop as u8 => out.push_str("POP\n"),
                x if x == Op::Call as u8 => {
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!("CALL {argc:>4}\n"));
                }
                x if x == Op::TailCall as u8 => {
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!("TAIL_CALL {argc:>4}\n"));
                }
                x if x == Op::Return as u8 => out.push_str("RETURN\n"),
                x if x == Op::Closure as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("CLOSURE {idx:>4}\n"));
                }
                x if x == Op::BuildList as u8 => {
                    let count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("BUILD_LIST {count:>4}\n"));
                }
                x if x == Op::BuildDict as u8 => {
                    let count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("BUILD_DICT {count:>4}\n"));
                }
                x if x == Op::Subscript as u8 => out.push_str("SUBSCRIPT\n"),
                x if x == Op::SubscriptOpt as u8 => out.push_str("SUBSCRIPT_OPT\n"),
                x if x == Op::Slice as u8 => out.push_str("SLICE\n"),
                x if x == Op::GetProperty as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "GET_PROPERTY {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::GetPropertyOpt as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "GET_PROPERTY_OPT {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::SetProperty as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "SET_PROPERTY {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::SetSubscript as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "SET_SUBSCRIPT {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::MethodCall as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!(
                        "METHOD_CALL {:>4} ({}) argc={}\n",
                        idx, self.constants[idx as usize], argc
                    ));
                }
                x if x == Op::MethodCallOpt as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!(
                        "METHOD_CALL_OPT {:>4} ({}) argc={}\n",
                        idx, self.constants[idx as usize], argc
                    ));
                }
                x if x == Op::Concat as u8 => {
                    let count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("CONCAT {count:>4}\n"));
                }
                x if x == Op::IterInit as u8 => out.push_str("ITER_INIT\n"),
                x if x == Op::IterNext as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("ITER_NEXT {target:>4}\n"));
                }
                x if x == Op::Throw as u8 => out.push_str("THROW\n"),
                x if x == Op::TryCatchSetup as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("TRY_CATCH_SETUP {target:>4}\n"));
                }
                x if x == Op::PopHandler as u8 => out.push_str("POP_HANDLER\n"),
                x if x == Op::Pipe as u8 => out.push_str("PIPE\n"),
                x if x == Op::Parallel as u8 => out.push_str("PARALLEL\n"),
                x if x == Op::ParallelMap as u8 => out.push_str("PARALLEL_MAP\n"),
                x if x == Op::ParallelMapStream as u8 => out.push_str("PARALLEL_MAP_STREAM\n"),
                x if x == Op::ParallelSettle as u8 => out.push_str("PARALLEL_SETTLE\n"),
                x if x == Op::Spawn as u8 => out.push_str("SPAWN\n"),
                x if x == Op::Import as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "IMPORT {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::SelectiveImport as u8 => {
                    let path_idx = self.read_u16(ip);
                    ip += 2;
                    let names_idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "SELECTIVE_IMPORT {:>4} ({}) names: {:>4} ({})\n",
                        path_idx,
                        self.constants[path_idx as usize],
                        names_idx,
                        self.constants[names_idx as usize]
                    ));
                }
                x if x == Op::SyncMutexEnter as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "SYNC_MUTEX_ENTER {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::DeadlineSetup as u8 => out.push_str("DEADLINE_SETUP\n"),
                x if x == Op::DeadlineEnd as u8 => out.push_str("DEADLINE_END\n"),
                x if x == Op::BuildEnum as u8 => {
                    let enum_idx = self.read_u16(ip);
                    ip += 2;
                    let variant_idx = self.read_u16(ip);
                    ip += 2;
                    let field_count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "BUILD_ENUM {:>4} ({}) {:>4} ({}) fields={}\n",
                        enum_idx,
                        self.constants[enum_idx as usize],
                        variant_idx,
                        self.constants[variant_idx as usize],
                        field_count
                    ));
                }
                x if x == Op::MatchEnum as u8 => {
                    let enum_idx = self.read_u16(ip);
                    ip += 2;
                    let variant_idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "MATCH_ENUM {:>4} ({}) {:>4} ({})\n",
                        enum_idx,
                        self.constants[enum_idx as usize],
                        variant_idx,
                        self.constants[variant_idx as usize]
                    ));
                }
                x if x == Op::PopIterator as u8 => out.push_str("POP_ITERATOR\n"),
                x if x == Op::TryUnwrap as u8 => out.push_str("TRY_UNWRAP\n"),
                x if x == Op::TryWrapOk as u8 => out.push_str("TRY_WRAP_OK\n"),
                x if x == Op::CallSpread as u8 => out.push_str("CALL_SPREAD\n"),
                x if x == Op::CallBuiltin as u8 => {
                    let id = self.read_u64(ip);
                    ip += 8;
                    let idx = self.read_u16(ip);
                    ip += 2;
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!(
                        "CALL_BUILTIN {id:#018x} {:>4} ({}) argc={}\n",
                        idx, self.constants[idx as usize], argc
                    ));
                }
                x if x == Op::CallBuiltinSpread as u8 => {
                    let id = self.read_u64(ip);
                    ip += 8;
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "CALL_BUILTIN_SPREAD {id:#018x} {:>4} ({})\n",
                        idx, self.constants[idx as usize]
                    ));
                }
                x if x == Op::MethodCallSpread as u8 => {
                    let idx = self.read_u16(ip + 1);
                    ip += 2;
                    out.push_str(&format!("METHOD_CALL_SPREAD {idx}\n"));
                }
                x if x == Op::Dup as u8 => out.push_str("DUP\n"),
                x if x == Op::Swap as u8 => out.push_str("SWAP\n"),
                x if x == Op::AddInt as u8 => out.push_str("ADD_INT\n"),
                x if x == Op::SubInt as u8 => out.push_str("SUB_INT\n"),
                x if x == Op::MulInt as u8 => out.push_str("MUL_INT\n"),
                x if x == Op::DivInt as u8 => out.push_str("DIV_INT\n"),
                x if x == Op::ModInt as u8 => out.push_str("MOD_INT\n"),
                x if x == Op::AddFloat as u8 => out.push_str("ADD_FLOAT\n"),
                x if x == Op::SubFloat as u8 => out.push_str("SUB_FLOAT\n"),
                x if x == Op::MulFloat as u8 => out.push_str("MUL_FLOAT\n"),
                x if x == Op::DivFloat as u8 => out.push_str("DIV_FLOAT\n"),
                x if x == Op::ModFloat as u8 => out.push_str("MOD_FLOAT\n"),
                x if x == Op::EqualInt as u8 => out.push_str("EQUAL_INT\n"),
                x if x == Op::NotEqualInt as u8 => out.push_str("NOT_EQUAL_INT\n"),
                x if x == Op::LessInt as u8 => out.push_str("LESS_INT\n"),
                x if x == Op::GreaterInt as u8 => out.push_str("GREATER_INT\n"),
                x if x == Op::LessEqualInt as u8 => out.push_str("LESS_EQUAL_INT\n"),
                x if x == Op::GreaterEqualInt as u8 => out.push_str("GREATER_EQUAL_INT\n"),
                x if x == Op::EqualFloat as u8 => out.push_str("EQUAL_FLOAT\n"),
                x if x == Op::NotEqualFloat as u8 => out.push_str("NOT_EQUAL_FLOAT\n"),
                x if x == Op::LessFloat as u8 => out.push_str("LESS_FLOAT\n"),
                x if x == Op::GreaterFloat as u8 => out.push_str("GREATER_FLOAT\n"),
                x if x == Op::LessEqualFloat as u8 => out.push_str("LESS_EQUAL_FLOAT\n"),
                x if x == Op::GreaterEqualFloat as u8 => out.push_str("GREATER_EQUAL_FLOAT\n"),
                x if x == Op::EqualBool as u8 => out.push_str("EQUAL_BOOL\n"),
                x if x == Op::NotEqualBool as u8 => out.push_str("NOT_EQUAL_BOOL\n"),
                x if x == Op::EqualString as u8 => out.push_str("EQUAL_STRING\n"),
                x if x == Op::NotEqualString as u8 => out.push_str("NOT_EQUAL_STRING\n"),
                x if x == Op::Yield as u8 => out.push_str("YIELD\n"),
                _ => {
                    out.push_str(&format!("UNKNOWN(0x{op:02x})\n"));
                }
            }
        }
        out
    }
}

fn is_adaptive_binary_op(op: Op) -> bool {
    matches!(
        op,
        Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Mod
            | Op::Equal
            | Op::NotEqual
            | Op::Less
            | Op::Greater
            | Op::LessEqual
            | Op::GreaterEqual
    )
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunk, Op};
    use crate::BuiltinId;

    #[test]
    fn op_from_byte_matches_repr_order() {
        for (byte, op) in Op::ALL.iter().copied().enumerate() {
            assert_eq!(byte as u8, op as u8);
            assert_eq!(Op::from_byte(byte as u8), Some(op));
        }
        assert_eq!(Op::from_byte(Op::ALL.len() as u8), None);
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
        let thawed = Chunk::from_cached(&frozen);
        assert!(thawed.references_outer_names);

        let plain = Chunk::new();
        assert!(!plain.references_outer_names);
        let frozen_plain = plain.freeze_for_cache();
        let thawed_plain = Chunk::from_cached(&frozen_plain);
        assert!(!thawed_plain.references_outer_names);
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
        let thawed = Chunk::from_cached(&frozen);
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
}
