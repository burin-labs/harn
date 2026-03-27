use std::fmt;

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

    // --- Arithmetic ---
    Add,
    Sub,
    Mul,
    Div,
    Mod,
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

    // --- Object operations ---
    /// Property access. arg: u16 = constant index (property name).
    GetProperty,
    /// Property assignment. arg: u16 = constant index (property name).
    /// Stack: [value] → assigns to the named variable's property.
    SetProperty,
    /// Subscript assignment. arg: u16 = constant index (variable name).
    /// Stack: [index, value] → assigns to variable[index] = value.
    SetSubscript,
    /// Method call. arg1: u16 = constant index (method name), arg2: u8 = arg count.
    MethodCall,

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
    /// Store closure for deferred execution, push TaskHandle.
    /// Stack: closure → TaskHandle
    Spawn,

    // --- Deadline ---
    /// Pop duration value, push deadline onto internal deadline stack.
    DeadlineSetup,
    /// Pop deadline from internal deadline stack.
    DeadlineEnd,

    // --- Misc ---
    /// Duplicate top of stack.
    Dup,
    /// Swap top two stack values.
    Swap,
}

/// A constant value in the constant pool.
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Nil,
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Int(n) => write!(f, "{n}"),
            Constant::Float(n) => write!(f, "{n}"),
            Constant::String(s) => write!(f, "\"{s}\""),
            Constant::Bool(b) => write!(f, "{b}"),
            Constant::Nil => write!(f, "nil"),
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
    /// Compiled function bodies (for closures).
    pub functions: Vec<CompiledFunction>,
}

/// A compiled function (closure body).
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    pub name: String,
    pub params: Vec<String>,
    pub chunk: Chunk,
}

impl Chunk {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            lines: Vec::new(),
            functions: Vec::new(),
        }
    }

    /// Add a constant and return its index.
    pub fn add_constant(&mut self, constant: Constant) -> u16 {
        // Reuse existing constant if possible
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
        self.code.push(op as u8);
        self.lines.push(line);
    }

    /// Emit an instruction with a u16 argument.
    pub fn emit_u16(&mut self, op: Op, arg: u16, line: u32) {
        self.code.push(op as u8);
        self.code.push((arg >> 8) as u8);
        self.code.push((arg & 0xFF) as u8);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Emit an instruction with a u8 argument.
    pub fn emit_u8(&mut self, op: Op, arg: u8, line: u32) {
        self.code.push(op as u8);
        self.code.push(arg);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Emit a method call: op + u16 (method name) + u8 (arg count).
    pub fn emit_method_call(&mut self, name_idx: u16, arg_count: u8, line: u32) {
        self.code.push(Op::MethodCall as u8);
        self.code.push((name_idx >> 8) as u8);
        self.code.push((name_idx & 0xFF) as u8);
        self.code.push(arg_count);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Current code offset (for jump patching).
    pub fn current_offset(&self) -> usize {
        self.code.len()
    }

    /// Emit a jump instruction with a placeholder offset. Returns the position to patch.
    pub fn emit_jump(&mut self, op: Op, line: u32) -> usize {
        self.code.push(op as u8);
        let patch_pos = self.code.len();
        self.code.push(0xFF);
        self.code.push(0xFF);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
        patch_pos
    }

    /// Patch a jump instruction at the given position to jump to the current offset.
    pub fn patch_jump(&mut self, patch_pos: usize) {
        let target = self.code.len() as u16;
        self.code[patch_pos] = (target >> 8) as u8;
        self.code[patch_pos + 1] = (target & 0xFF) as u8;
    }

    /// Read a u16 argument at the given position.
    pub fn read_u16(&self, pos: usize) -> u16 {
        ((self.code[pos] as u16) << 8) | (self.code[pos + 1] as u16)
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
                x if x == Op::Add as u8 => out.push_str("ADD\n"),
                x if x == Op::Sub as u8 => out.push_str("SUB\n"),
                x if x == Op::Mul as u8 => out.push_str("MUL\n"),
                x if x == Op::Div as u8 => out.push_str("DIV\n"),
                x if x == Op::Mod as u8 => out.push_str("MOD\n"),
                x if x == Op::Negate as u8 => out.push_str("NEGATE\n"),
                x if x == Op::Equal as u8 => out.push_str("EQUAL\n"),
                x if x == Op::NotEqual as u8 => out.push_str("NOT_EQUAL\n"),
                x if x == Op::Less as u8 => out.push_str("LESS\n"),
                x if x == Op::Greater as u8 => out.push_str("GREATER\n"),
                x if x == Op::LessEqual as u8 => out.push_str("LESS_EQUAL\n"),
                x if x == Op::GreaterEqual as u8 => out.push_str("GREATER_EQUAL\n"),
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
                x if x == Op::GetProperty as u8 => {
                    let idx = self.read_u16(ip);
                    ip += 2;
                    out.push_str(&format!(
                        "GET_PROPERTY {:>4} ({})\n",
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
                x if x == Op::Spawn as u8 => out.push_str("SPAWN\n"),
                x if x == Op::DeadlineSetup as u8 => out.push_str("DEADLINE_SETUP\n"),
                x if x == Op::DeadlineEnd as u8 => out.push_str("DEADLINE_END\n"),
                x if x == Op::Dup as u8 => out.push_str("DUP\n"),
                x if x == Op::Swap as u8 => out.push_str("SWAP\n"),
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
