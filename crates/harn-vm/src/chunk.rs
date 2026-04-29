use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

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
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Nil,
    Duration(i64),
}

/// Monomorphic inline-cache state for bytecode instructions that repeatedly
/// resolve the same property or builtin method. Cache guards are intentionally
/// conservative: each entry is tied to the instruction's name constant index
/// and a single receiver variant. Harn collection values are immutable or
/// copy-on-write at the VM level, so list/string/pair/range/set/dict receiver
/// kind caches do not need invalidation; dynamic dict fields and struct fields
/// are left on the generic path until they have stable layout metadata.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyCacheTarget {
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
    StringCount,
    StringEmpty,
    DictCount,
    RangeCount,
    RangeLen,
    RangeEmpty,
    RangeFirst,
    RangeLast,
    SetCount,
    SetLen,
    SetEmpty,
}

/// Debug metadata for a slot-indexed local in a compiled chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    inline_cache_slots: BTreeMap<usize, usize>,
    /// Shared cache entries so cloned chunks in call frames warm the same side
    /// table as the compiled chunk used by tests/debugging.
    inline_caches: Rc<RefCell<Vec<InlineCacheEntry>>>,
    /// Source-name metadata for slot-indexed locals in this chunk.
    pub(crate) local_slots: Vec<LocalSlotInfo>,
}

pub type ChunkRef = Rc<Chunk>;
pub type CompiledFunctionRef = Rc<CompiledFunction>;

/// A compiled function (closure body).
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    pub name: String,
    pub params: Vec<String>,
    /// Index of the first parameter with a default value, or None if all required.
    pub default_start: Option<usize>,
    pub chunk: ChunkRef,
    /// True if the function body contains `yield` expressions (generator function).
    pub is_generator: bool,
    /// True if the function was declared as `gen fn` and should return Stream.
    pub is_stream: bool,
    /// True if the last parameter is a rest parameter (`...name`).
    pub has_rest_param: bool,
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
            inline_caches: Rc::new(RefCell::new(Vec::new())),
            local_slots: Vec::new(),
        }
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
        self.code.push(op as u8);
        self.lines.push(line);
        self.columns.push(col);
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
    }

    /// Emit an instruction with a u8 argument.
    pub fn emit_u8(&mut self, op: Op, arg: u8, line: u32) {
        let col = self.current_col;
        self.code.push(op as u8);
        self.code.push(arg);
        self.lines.push(line);
        self.lines.push(line);
        self.columns.push(col);
        self.columns.push(col);
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
        self.code.push(Op::CallBuiltin as u8);
        self.code.extend_from_slice(&id.raw().to_be_bytes());
        self.code.push((name_idx >> 8) as u8);
        self.code.push((name_idx & 0xFF) as u8);
        self.code.push(arg_count);
        for _ in 0..12 {
            self.lines.push(line);
            self.columns.push(col);
        }
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
    }

    pub(crate) fn inline_cache_slot(&self, op_offset: usize) -> Option<usize> {
        self.inline_cache_slots.get(&op_offset).copied()
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
            out.push_str(&format!("{:04} [{:>4}] ", ip, line));
            ip += 1;

            match op {
                x if x == Op::Constant as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    let val = &self.constants[idx as usize];
                    out.push_str(&format!("CONSTANT {:>4} ({})\n", idx, val));
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
                    out.push_str(&format!("GET_LOCAL_SLOT {:>4}", slot));
                    if let Some(info) = self.local_slots.get(slot as usize) {
                        out.push_str(&format!(" ({})", info.name));
                    }
                    out.push('\n');
                }
                x if x == Op::DefLocalSlot as u8 => {
                    let slot = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("DEF_LOCAL_SLOT {:>4}", slot));
                    if let Some(info) = self.local_slots.get(slot as usize) {
                        out.push_str(&format!(" ({})", info.name));
                    }
                    out.push('\n');
                }
                x if x == Op::SetLocalSlot as u8 => {
                    let slot = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("SET_LOCAL_SLOT {:>4}", slot));
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
                    out.push_str(&format!("JUMP {:>4}\n", target));
                }
                x if x == Op::JumpIfFalse as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("JUMP_IF_FALSE {:>4}\n", target));
                }
                x if x == Op::JumpIfTrue as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("JUMP_IF_TRUE {:>4}\n", target));
                }
                x if x == Op::Pop as u8 => out.push_str("POP\n"),
                x if x == Op::Call as u8 => {
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!("CALL {:>4}\n", argc));
                }
                x if x == Op::TailCall as u8 => {
                    let argc = self.code[ip];
                    ip += 1;
                    out.push_str(&format!("TAIL_CALL {:>4}\n", argc));
                }
                x if x == Op::Return as u8 => out.push_str("RETURN\n"),
                x if x == Op::Closure as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("CLOSURE {:>4}\n", idx));
                }
                x if x == Op::BuildList as u8 => {
                    let count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("BUILD_LIST {:>4}\n", count));
                }
                x if x == Op::BuildDict as u8 => {
                    let count = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("BUILD_DICT {:>4}\n", count));
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
                    out.push_str(&format!("CONCAT {:>4}\n", count));
                }
                x if x == Op::IterInit as u8 => out.push_str("ITER_INIT\n"),
                x if x == Op::IterNext as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("ITER_NEXT {:>4}\n", target));
                }
                x if x == Op::Throw as u8 => out.push_str("THROW\n"),
                x if x == Op::TryCatchSetup as u8 => {
                    let target = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!("TRY_CATCH_SETUP {:>4}\n", target));
                }
                x if x == Op::PopHandler as u8 => out.push_str("POP_HANDLER\n"),
                x if x == Op::Pipe as u8 => out.push_str("PIPE\n"),
                x if x == Op::Parallel as u8 => out.push_str("PARALLEL\n"),
                x if x == Op::ParallelMap as u8 => out.push_str("PARALLEL_MAP\n"),
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
                    out.push_str(&format!("UNKNOWN(0x{:02x})\n", op));
                }
            }
        }
        out
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Op;

    #[test]
    fn op_from_byte_matches_repr_order() {
        for (byte, op) in Op::ALL.iter().copied().enumerate() {
            assert_eq!(byte as u8, op as u8);
            assert_eq!(Op::from_byte(byte as u8), Some(op));
        }
        assert_eq!(Op::from_byte(Op::ALL.len() as u8), None);
    }
}
