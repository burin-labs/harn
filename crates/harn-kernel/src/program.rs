use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use harn_parser::TypeExpr;
use serde::{Deserialize, Serialize};

use crate::{BuiltinId, Op};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Nil,
    Duration(i64),
}

impl fmt::Display for Constant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::String(value) => write!(formatter, "\"{value}\""),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Nil => formatter.write_str("nil"),
            Self::Duration(value) => write!(formatter, "{value}ms"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSlotInfo {
    pub name: String,
    pub mutable: bool,
    pub scope_depth: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSlot {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub has_default: bool,
}

impl ParamSlot {
    pub fn from_typed_param(param: &harn_parser::TypedParam) -> Self {
        Self::from_typed_param_with_type(param, param.type_expr.clone())
    }

    pub(crate) fn from_typed_param_with_type(
        param: &harn_parser::TypedParam,
        type_expr: Option<TypeExpr>,
    ) -> Self {
        Self {
            name: param.name.clone(),
            type_expr,
            has_default: param.default_value.is_some(),
        }
    }

    pub fn vec_from_typed(params: &[harn_parser::TypedParam]) -> Vec<Self> {
        params.iter().map(Self::from_typed_param).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledFunction {
    pub name: String,
    pub type_params: Vec<String>,
    pub nominal_type_names: Vec<String>,
    pub params: Vec<ParamSlot>,
    pub default_start: Option<usize>,
    pub chunk: Arc<Chunk>,
    pub is_generator: bool,
    pub is_stream: bool,
    pub has_rest_param: bool,
    pub has_runtime_type_checks: bool,
}

impl CompiledFunction {
    pub(crate) fn has_runtime_type_checks_for_params(params: &[ParamSlot]) -> bool {
        params.iter().any(|param| param.type_expr.is_some())
    }

    pub fn param_names(&self) -> impl Iterator<Item = &str> {
        self.params.iter().map(|param| param.name.as_str())
    }

    pub fn required_param_count(&self) -> usize {
        self.default_start.unwrap_or(self.params.len())
    }

    pub fn declares_type_param(&self, name: &str) -> bool {
        self.type_params.iter().any(|param| param == name)
    }

    pub fn has_nominal_type(&self, name: &str) -> bool {
        self.nominal_type_names.iter().any(|ty| ty == name)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Constant>,
    #[serde(skip)]
    constant_index: Option<HashMap<ConstantKey, u16>>,
    pub lines: Vec<u32>,
    pub columns: Vec<u32>,
    pub source_file: Option<String>,
    #[doc(hidden)]
    pub current_col: u32,
    pub functions: Vec<Arc<CompiledFunction>>,
    #[doc(hidden)]
    pub local_slots: Vec<LocalSlotInfo>,
    #[doc(hidden)]
    pub references_outer_names: bool,
    #[cfg(debug_assertions)]
    #[serde(skip)]
    balance_depth: i32,
    #[cfg(debug_assertions)]
    #[serde(skip)]
    balance_nonlinear: u32,
}

impl Clone for Chunk {
    fn clone(&self) -> Self {
        Self {
            code: self.code.clone(),
            constants: self.constants.clone(),
            constant_index: self.constant_index.clone(),
            lines: self.lines.clone(),
            columns: self.columns.clone(),
            source_file: self.source_file.clone(),
            current_col: self.current_col,
            functions: self.functions.clone(),
            local_slots: self.local_slots.clone(),
            references_outer_names: self.references_outer_names,
            #[cfg(debug_assertions)]
            balance_depth: self.balance_depth,
            #[cfg(debug_assertions)]
            balance_nonlinear: self.balance_nonlinear,
        }
    }
}

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
    fn from(value: &Constant) -> Self {
        match value {
            Constant::Int(value) => Self::Int(*value),
            Constant::Float(value) => Self::Float(value.to_bits()),
            Constant::String(value) => Self::String(value.clone()),
            Constant::Bool(value) => Self::Bool(*value),
            Constant::Nil => Self::Nil,
            Constant::Duration(value) => Self::Duration(*value),
        }
    }
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
pub(crate) struct BalanceProbe {
    depth: i32,
    nonlinear: u32,
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunk {
    pub(crate) fn from_artifact_parts(
        code: Vec<u8>,
        constants: Vec<Constant>,
        lines: Vec<u32>,
        columns: Vec<u32>,
        source_file: Option<String>,
        functions: Vec<Arc<CompiledFunction>>,
        local_slots: Vec<LocalSlotInfo>,
        references_outer_names: bool,
    ) -> Self {
        Self {
            code,
            constants,
            constant_index: None,
            lines,
            columns,
            source_file,
            current_col: 0,
            functions,
            local_slots,
            references_outer_names,
            #[cfg(debug_assertions)]
            balance_depth: 0,
            #[cfg(debug_assertions)]
            balance_nonlinear: 0,
        }
    }

    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            constant_index: Some(HashMap::new()),
            lines: Vec::new(),
            columns: Vec::new(),
            source_file: None,
            current_col: 0,
            functions: Vec::new(),
            local_slots: Vec::new(),
            references_outer_names: false,
            #[cfg(debug_assertions)]
            balance_depth: 0,
            #[cfg(debug_assertions)]
            balance_nonlinear: 0,
        }
    }

    pub fn set_column(&mut self, column: u32) {
        self.current_col = column;
    }

    pub fn add_constant(&mut self, constant: Constant) -> u16 {
        let index = self.constant_index.get_or_insert_with(|| {
            self.constants
                .iter()
                .enumerate()
                .filter_map(|(index, value)| {
                    u16::try_from(index)
                        .ok()
                        .map(|index| (ConstantKey::from(value), index))
                })
                .collect()
        });
        let key = ConstantKey::from(&constant);
        if let Some(existing) = index.get(&key) {
            return *existing;
        }
        let slot =
            u16::try_from(self.constants.len()).expect("constant pool exceeded u16 operand space");
        self.constants.push(constant);
        index.insert(key, slot);
        slot
    }

    pub fn emit(&mut self, op: Op, line: u32) {
        self.note_balance(op, 0);
        self.push_bytes(&[op as u8], line);
        if reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    pub fn emit_u16(&mut self, op: Op, value: u16, line: u32) {
        self.note_balance(op, value);
        self.push_bytes(&[op as u8, (value >> 8) as u8, value as u8], line);
        if reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    /// Emit an instruction whose complete operand list consists of `u16`
    /// values. The opcode schema remains the authority for arity and width.
    pub fn emit_u16_operands(&mut self, op: Op, values: &[u16], line: u32) {
        debug_assert_eq!(op.operands().len(), values.len());
        debug_assert!(op.operands().iter().all(|operand| operand.width() == 2));
        self.note_balance(op, values.first().copied().unwrap_or_default());
        let mut bytes = Vec::with_capacity(op.instruction_len());
        bytes.push(op as u8);
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        self.push_bytes(&bytes, line);
        if reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    pub fn emit_u8(&mut self, op: Op, value: u8, line: u32) {
        self.note_balance(op, u16::from(value));
        self.push_bytes(&[op as u8, value], line);
        if reads_outer_name(op) {
            self.references_outer_names = true;
        }
    }

    pub fn emit_call_builtin(&mut self, id: BuiltinId, name: u16, argc: u8, line: u32) {
        self.note_balance(Op::CallBuiltin, u16::from(argc));
        let mut bytes = vec![Op::CallBuiltin as u8];
        bytes.extend_from_slice(&id.raw().to_be_bytes());
        bytes.extend_from_slice(&name.to_be_bytes());
        bytes.push(argc);
        self.push_bytes(&bytes, line);
        self.references_outer_names = true;
    }

    pub fn emit_call_builtin_spread(&mut self, id: BuiltinId, name: u16, line: u32) {
        let mut bytes = vec![Op::CallBuiltinSpread as u8];
        bytes.extend_from_slice(&id.raw().to_be_bytes());
        bytes.extend_from_slice(&name.to_be_bytes());
        self.push_bytes(&bytes, line);
        self.references_outer_names = true;
    }

    pub fn emit_method_call(&mut self, name: u16, argc: u8, line: u32) {
        self.emit_method_call_inner(Op::MethodCall, name, argc, line);
    }

    pub fn emit_method_call_opt(&mut self, name: u16, argc: u8, line: u32) {
        self.emit_method_call_inner(Op::MethodCallOpt, name, argc, line);
    }

    fn emit_method_call_inner(&mut self, op: Op, name: u16, argc: u8, line: u32) {
        self.note_balance(op, u16::from(argc));
        self.push_bytes(&[op as u8, (name >> 8) as u8, name as u8, argc], line);
    }

    pub fn emit_set_local_slot_property(&mut self, property: u16, slot: u16, line: u32) {
        self.note_balance(Op::SetLocalSlotProperty, 0);
        self.push_bytes(
            &[
                Op::SetLocalSlotProperty as u8,
                (property >> 8) as u8,
                property as u8,
                (slot >> 8) as u8,
                slot as u8,
            ],
            line,
        );
    }

    fn push_bytes(&mut self, bytes: &[u8], line: u32) {
        self.code.extend_from_slice(bytes);
        self.lines.extend(std::iter::repeat_n(line, bytes.len()));
        self.columns
            .extend(std::iter::repeat_n(self.current_col, bytes.len()));
    }

    pub fn current_offset(&self) -> usize {
        self.code.len()
    }

    pub fn emit_jump(&mut self, op: Op, line: u32) -> usize {
        self.note_balance(op, 0);
        let patch = self.code.len() + 1;
        self.push_bytes(&[op as u8, 0xff, 0xff], line);
        patch
    }

    pub fn patch_jump(&mut self, patch: usize) {
        self.patch_jump_to(patch, self.code.len());
    }

    pub fn patch_jump_to(&mut self, patch: usize, target: usize) {
        // The compiler's final addressability guard turns this provisional
        // truncation into a structured compile error before the image escapes.
        let target = target as u16;
        self.code[patch..patch + 2].copy_from_slice(&target.to_be_bytes());
    }

    pub fn read_u16(&self, position: usize) -> u16 {
        u16::from_be_bytes([self.code[position], self.code[position + 1]])
    }

    pub(crate) fn add_local_slot(
        &mut self,
        name: String,
        mutable: bool,
        scope_depth: usize,
    ) -> u16 {
        let slot = u16::try_from(self.local_slots.len()).expect("local slot count exceeded u16");
        self.local_slots.push(LocalSlotInfo {
            name,
            mutable,
            scope_depth,
        });
        slot
    }

    #[cfg(debug_assertions)]
    pub(crate) fn balance_probe(&self) -> BalanceProbe {
        BalanceProbe {
            depth: self.balance_depth,
            nonlinear: self.balance_nonlinear,
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn balance_delta_since(&self, probe: BalanceProbe) -> Option<i32> {
        (self.balance_nonlinear == probe.nonlinear).then_some(self.balance_depth - probe.depth)
    }

    #[cfg(debug_assertions)]
    fn note_balance(&mut self, op: Op, count: u16) {
        match stack_delta(op, count) {
            Some(delta) => self.balance_depth += delta,
            None => self.balance_nonlinear += 1,
        }
    }

    #[cfg(not(debug_assertions))]
    fn note_balance(&mut self, _op: Op, _count: u16) {}

    pub fn disassemble(&self, name: &str) -> String {
        let mut output = format!("== {name} ==\n");
        let mut ip = 0;
        while let Some(byte) = self.code.get(ip).copied() {
            let offset = ip;
            let line = self.lines.get(ip).copied().unwrap_or(0);
            let Some(op) = Op::from_byte(byte) else { break };
            ip += 1;
            let rendered = self.disassemble_instruction(op, &mut ip);
            output.push_str(&format!("{offset:04} [{line:>4}] {rendered}\n"));
        }
        output
    }

    fn disassemble_instruction(&self, op: Op, ip: &mut usize) -> String {
        let label = opcode_label(op.name());
        let read_u16 = |position: usize| {
            self.code
                .get(position..position + 2)
                .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        };
        if matches!(
            op,
            Op::Constant
                | Op::GetVar
                | Op::DefLet
                | Op::DefVar
                | Op::DefCell
                | Op::SetVar
                | Op::GetProperty
                | Op::GetPropertyOpt
                | Op::SetProperty
                | Op::Import
        ) {
            let Some(index) = read_u16(*ip) else {
                return label;
            };
            *ip += 2;
            return match self.constants.get(index as usize) {
                Some(value) => format!("{label} {index:>4} ({value})"),
                None => format!("{label} {index:>4}"),
            };
        }
        if matches!(
            op,
            Op::GetLocalSlot
                | Op::DefLocalSlot
                | Op::SetLocalSlot
                | Op::SetLocalSlotSubscript
                | Op::ConcatAssignLocal
        ) {
            let Some(slot) = read_u16(*ip) else {
                return label;
            };
            *ip += 2;
            return match self.local_slots.get(slot as usize) {
                Some(info) => format!("{label} {slot:>4} ({})", info.name),
                None => format!("{label} {slot:>4}"),
            };
        }
        if op == Op::SetLocalSlotProperty {
            let Some(property) = read_u16(*ip) else {
                return label;
            };
            let Some(slot) = read_u16(*ip + 2) else {
                return label;
            };
            *ip += 4;
            let property = self
                .constants
                .get(property as usize)
                .map(ToString::to_string)
                .unwrap_or_default();
            return format!("{label} {slot:>4} {property}");
        }
        if matches!(op, Op::MethodCall | Op::MethodCallOpt) {
            let Some(name) = read_u16(*ip) else {
                return label;
            };
            let Some(argc) = self.code.get(*ip + 2).copied() else {
                return label;
            };
            *ip += 3;
            let name = self
                .constants
                .get(name as usize)
                .map(ToString::to_string)
                .unwrap_or_default();
            return format!("{label} {argc:>4} ({name})");
        }
        if matches!(op, Op::Call | Op::TailCall) {
            let Some(argc) = self.code.get(*ip).copied() else {
                return label;
            };
            *ip += 1;
            return format!("{label} {argc:>4}");
        }
        let width = instruction_len(op, &self.code[(*ip).saturating_sub(1)..]).unwrap_or(1);
        if width == 3 {
            let Some(value) = read_u16(*ip) else {
                return label;
            };
            *ip += 2;
            return format!("{label} {value:>4}");
        }
        if width > 1 {
            *ip = (*ip).saturating_add(width - 1).min(self.code.len());
        }
        label
    }
}

fn opcode_label(name: &str) -> String {
    let mut output = String::new();
    for (index, ch) in name.chars().enumerate() {
        if index > 0 && ch.is_ascii_uppercase() {
            output.push('_');
        }
        output.push(ch.to_ascii_uppercase());
    }
    output
}

fn reads_outer_name(op: Op) -> bool {
    matches!(
        op,
        Op::GetVar
            | Op::SetVar
            | Op::Call
            | Op::TailCall
            | Op::Pipe
            | Op::CheckType
            | Op::CallSpread
            | Op::CallBuiltin
            | Op::CallBuiltinSpread
    )
}

pub fn instruction_len(op: Op, _remaining: &[u8]) -> Option<usize> {
    Some(op.instruction_len())
}

#[cfg(debug_assertions)]
fn stack_delta(op: Op, count: u16) -> Option<i32> {
    use Op::*;
    let count = i32::from(count);
    Some(match op {
        Constant | Nil | True | False | RootHarness | GetVar | GetArgc | GetLocalSlot | Closure
        | Dup => 1,
        DefLet | DefVar | DefCell | SetVar | DefLocalSlot | SetLocalSlot | SetProperty
        | SetLocalSlotProperty | ConcatAssignLocal | Pop => -1,
        Negate | Not | GetProperty | GetPropertyOpt | CheckType | TryUnwrap | TryWrapOk | Swap
        | PushScope | PopScope | PopIterator | PopHandler => 0,
        Add | Sub | Mul | Div | Mod | Pow | AddInt | SubInt | MulInt | DivInt | ModInt
        | AddFloat | SubFloat | MulFloat | DivFloat | ModFloat | Equal | NotEqual | Less
        | Greater | LessEqual | GreaterEqual | EqualInt | NotEqualInt | LessInt | GreaterInt
        | LessEqualInt | GreaterEqualInt | EqualFloat | NotEqualFloat | LessFloat
        | GreaterFloat | LessEqualFloat | GreaterEqualFloat | EqualBool | NotEqualBool
        | EqualString | NotEqualString | Contains | Subscript | SubscriptOpt => -1,
        IterInit => -1,
        Slice | SetSubscript | SetLocalSlotSubscript => -2,
        BuildList | Concat | CallBuiltin => 1 - count,
        BuildDict => 1 - 2 * count,
        Call | MethodCall | MethodCallOpt => -count,
        Jump
        | JumpIfFalse
        | JumpIfTrue
        | IterNext
        | Return
        | TailCall
        | Throw
        | TryCatchSetup
        | Spawn
        | Pipe
        | Parallel
        | ParallelMap
        | ParallelMapStream
        | ParallelSettle
        | SyncMutexEnter
        | SyncMutexEnterKeyed
        | TaskScopeEnter
        | TaskScopeExit
        | Import
        | SelectiveImport
        | NamespaceImport
        | NamespaceImportMembers
        | DeadlineSetup
        | DeadlineEnd
        | BuildEnum
        | MatchEnum
        | Yield
        | CallSpread
        | CallBuiltinSpread
        | MethodCallSpread => return None,
    })
}
