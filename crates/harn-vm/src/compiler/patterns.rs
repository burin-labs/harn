use harn_parser::{BindingPattern, Node, SNode};

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    /// Compile a destructuring binding pattern.
    /// Expects the RHS value to already be on the stack.
    /// After this, the value is consumed (popped) and each binding is defined.
    pub(super) fn compile_destructuring(
        &mut self,
        pattern: &BindingPattern,
        is_mutable: bool,
    ) -> Result<(), CompileError> {
        let def_op = if is_mutable { Op::DefVar } else { Op::DefLet };
        match pattern {
            BindingPattern::Identifier(name) => {
                let idx = self.chunk.add_constant(Constant::String(name.clone()));
                self.chunk.emit_u16(def_op, idx, self.line);
            }
            BindingPattern::Dict(fields) => {
                // Runtime `__assert_dict(value)` type check on the RHS.
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self
                    .chunk
                    .add_constant(Constant::String("__assert_dict".into()));
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line);

                let non_rest: Vec<_> = fields.iter().filter(|f| !f.is_rest).collect();
                let rest_field = fields.iter().find(|f| f.is_rest);

                for field in &non_rest {
                    self.chunk.emit(Op::Dup, self.line);
                    let key_idx = self.chunk.add_constant(Constant::String(field.key.clone()));
                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    self.chunk.emit(Op::Subscript, self.line);
                    if let Some(default_expr) = &field.default_value {
                        // Nil-coalescing: use default when the field was nil.
                        self.chunk.emit(Op::Dup, self.line);
                        self.chunk.emit(Op::Nil, self.line);
                        self.chunk.emit(Op::NotEqual, self.line);
                        let skip_default = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_node(default_expr)?;
                        let end = self.chunk.emit_jump(Op::Jump, self.line);
                        self.chunk.patch_jump(skip_default);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.patch_jump(end);
                    }
                    let binding_name = field.alias.as_deref().unwrap_or(&field.key);
                    let name_idx = self
                        .chunk
                        .add_constant(Constant::String(binding_name.to_string()));
                    self.chunk.emit_u16(def_op, name_idx, self.line);
                }

                if let Some(rest) = rest_field {
                    // `__dict_rest(dict, [keys_to_exclude])`.
                    let fn_idx = self
                        .chunk
                        .add_constant(Constant::String("__dict_rest".into()));
                    self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    for field in &non_rest {
                        let key_idx = self.chunk.add_constant(Constant::String(field.key.clone()));
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    }
                    self.chunk
                        .emit_u16(Op::BuildList, non_rest.len() as u16, self.line);
                    self.chunk.emit_u8(Op::Call, 2, self.line);
                    let rest_name = &rest.key;
                    let rest_idx = self.chunk.add_constant(Constant::String(rest_name.clone()));
                    self.chunk.emit_u16(def_op, rest_idx, self.line);
                } else {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
            BindingPattern::Pair(first_name, second_name) => {
                self.chunk.emit(Op::Dup, self.line);
                let first_key_idx = self
                    .chunk
                    .add_constant(Constant::String("first".to_string()));
                self.chunk
                    .emit_u16(Op::GetProperty, first_key_idx, self.line);
                let first_name_idx = self
                    .chunk
                    .add_constant(Constant::String(first_name.clone()));
                self.chunk.emit_u16(def_op, first_name_idx, self.line);

                let second_key_idx = self
                    .chunk
                    .add_constant(Constant::String("second".to_string()));
                self.chunk
                    .emit_u16(Op::GetProperty, second_key_idx, self.line);
                let second_name_idx = self
                    .chunk
                    .add_constant(Constant::String(second_name.clone()));
                self.chunk.emit_u16(def_op, second_name_idx, self.line);
                // No trailing Pop: GetProperty consumed the source pair.
            }
            BindingPattern::List(elements) => {
                // Runtime `__assert_list(value)` type check on the RHS.
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self
                    .chunk
                    .add_constant(Constant::String("__assert_list".into()));
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line);

                let non_rest: Vec<_> = elements.iter().filter(|e| !e.is_rest).collect();
                let rest_elem = elements.iter().find(|e| e.is_rest);

                for (i, elem) in non_rest.iter().enumerate() {
                    self.chunk.emit(Op::Dup, self.line);
                    let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                    self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                    self.chunk.emit(Op::Subscript, self.line);
                    if let Some(default_expr) = &elem.default_value {
                        // Nil-coalescing: use default when the slot was nil.
                        self.chunk.emit(Op::Dup, self.line);
                        self.chunk.emit(Op::Nil, self.line);
                        self.chunk.emit(Op::NotEqual, self.line);
                        let skip_default = self.chunk.emit_jump(Op::JumpIfTrue, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_node(default_expr)?;
                        let end = self.chunk.emit_jump(Op::Jump, self.line);
                        self.chunk.patch_jump(skip_default);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.patch_jump(end);
                    }
                    let name_idx = self.chunk.add_constant(Constant::String(elem.name.clone()));
                    self.chunk.emit_u16(def_op, name_idx, self.line);
                }

                if let Some(rest) = rest_elem {
                    // Slice list[n..] where n = non_rest.len(); Slice expects
                    // object, start, end on the stack.
                    let start_idx = self
                        .chunk
                        .add_constant(Constant::Int(non_rest.len() as i64));
                    self.chunk.emit_u16(Op::Constant, start_idx, self.line);
                    self.chunk.emit(Op::Nil, self.line);
                    self.chunk.emit(Op::Slice, self.line);
                    let rest_name_idx =
                        self.chunk.add_constant(Constant::String(rest.name.clone()));
                    self.chunk.emit_u16(def_op, rest_name_idx, self.line);
                } else {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
        Ok(())
    }

    /// Compile a `match` expression (`Node::MatchExpr`).
    pub(super) fn compile_match_expr(
        &mut self,
        value: &SNode,
        arms: &[harn_parser::MatchArm],
    ) -> Result<(), CompileError> {
        self.compile_node(value)?;
        let mut end_jumps = Vec::new();
        for arm in arms {
            match &arm.pattern.node {
                // Wildcard `_` — always matches (unless guarded)
                Node::Identifier(name) if name == "_" => {
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                    } else {
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                }
                // Enum destructuring: EnumConstruct pattern
                Node::EnumConstruct {
                    enum_name,
                    variant,
                    args: pat_args,
                } => {
                    self.chunk.emit(Op::Dup, self.line);
                    let en_idx = self.chunk.add_constant(Constant::String(enum_name.clone()));
                    let vn_idx = self.chunk.add_constant(Constant::String(variant.clone()));
                    self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                    let hi = (vn_idx >> 8) as u8;
                    let lo = vn_idx as u8;
                    self.chunk.code.push(hi);
                    self.chunk.code.push(lo);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    self.begin_scope();

                    // Bind field variables from the enum's fields; the
                    // match value stays on the stack for extraction.
                    for (i, pat_arg) in pat_args.iter().enumerate() {
                        if let Node::Identifier(binding_name) = &pat_arg.node {
                            self.chunk.emit(Op::Dup, self.line);
                            let fields_idx = self
                                .chunk
                                .add_constant(Constant::String("fields".to_string()));
                            self.chunk.emit_u16(Op::GetProperty, fields_idx, self.line);
                            let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                            self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                            self.chunk.emit(Op::Subscript, self.line);
                            let name_idx = self
                                .chunk
                                .add_constant(Constant::String(binding_name.clone()));
                            self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                        }
                    }

                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                        self.end_scope();
                    } else {
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line);
                }
                // Enum variant without args: PropertyAccess(EnumName, Variant)
                Node::PropertyAccess { object, property } if matches!(&object.node, Node::Identifier(n) if self.enum_names.contains(n)) =>
                {
                    let enum_name = if let Node::Identifier(n) = &object.node {
                        n.clone()
                    } else {
                        unreachable!()
                    };
                    self.chunk.emit(Op::Dup, self.line);
                    let en_idx = self.chunk.add_constant(Constant::String(enum_name));
                    let vn_idx = self.chunk.add_constant(Constant::String(property.clone()));
                    self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                    let hi = (vn_idx >> 8) as u8;
                    let lo = vn_idx as u8;
                    self.chunk.code.push(hi);
                    self.chunk.code.push(lo);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                    } else {
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line);
                }
                // Enum destructuring via MethodCall: EnumName.Variant(bindings...)
                // Parser produces MethodCall for EnumName.Variant(x) patterns
                Node::MethodCall {
                    object,
                    method,
                    args: pat_args,
                } if matches!(&object.node, Node::Identifier(n) if self.enum_names.contains(n)) => {
                    let enum_name = if let Node::Identifier(n) = &object.node {
                        n.clone()
                    } else {
                        unreachable!()
                    };
                    self.chunk.emit(Op::Dup, self.line);
                    let en_idx = self.chunk.add_constant(Constant::String(enum_name));
                    let vn_idx = self.chunk.add_constant(Constant::String(method.clone()));
                    self.chunk.emit_u16(Op::MatchEnum, en_idx, self.line);
                    let hi = (vn_idx >> 8) as u8;
                    let lo = vn_idx as u8;
                    self.chunk.code.push(hi);
                    self.chunk.code.push(lo);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    self.chunk.lines.push(self.line);
                    self.chunk.columns.push(self.column);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    self.begin_scope();

                    for (i, pat_arg) in pat_args.iter().enumerate() {
                        if let Node::Identifier(binding_name) = &pat_arg.node {
                            self.chunk.emit(Op::Dup, self.line);
                            let fields_idx = self
                                .chunk
                                .add_constant(Constant::String("fields".to_string()));
                            self.chunk.emit_u16(Op::GetProperty, fields_idx, self.line);
                            let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                            self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                            self.chunk.emit(Op::Subscript, self.line);
                            let name_idx = self
                                .chunk
                                .add_constant(Constant::String(binding_name.clone()));
                            self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                        }
                    }

                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                        self.end_scope();
                    } else {
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line);
                }
                // Binding pattern: bare identifier always matches.
                Node::Identifier(name) => {
                    self.begin_scope();
                    self.chunk.emit(Op::Dup, self.line);
                    let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                    self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                        self.end_scope();
                    } else {
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                }
                // Dict pattern: {key: literal, key: binding, ...}
                Node::DictLiteral(entries)
                    if entries
                        .iter()
                        .all(|e| matches!(&e.key.node, Node::StringLiteral(_))) =>
                {
                    self.chunk.emit(Op::Dup, self.line);
                    let typeof_idx = self.chunk.add_constant(Constant::String("type_of".into()));
                    self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let dict_str = self.chunk.add_constant(Constant::String("dict".into()));
                    self.chunk.emit_u16(Op::Constant, dict_str, self.line);
                    self.chunk.emit(Op::Equal, self.line);
                    let skip_type = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);

                    let mut constraint_skips = Vec::new();
                    let mut bindings = Vec::new();
                    self.begin_scope();
                    for entry in entries {
                        if let Node::StringLiteral(key) = &entry.key.node {
                            match &entry.value.node {
                                Node::StringLiteral(_)
                                | Node::IntLiteral(_)
                                | Node::FloatLiteral(_)
                                | Node::BoolLiteral(_)
                                | Node::NilLiteral => {
                                    self.chunk.emit(Op::Dup, self.line);
                                    let key_idx =
                                        self.chunk.add_constant(Constant::String(key.clone()));
                                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                                    self.chunk.emit(Op::Subscript, self.line);
                                    self.compile_node(&entry.value)?;
                                    self.chunk.emit(Op::Equal, self.line);
                                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                    self.chunk.emit(Op::Pop, self.line);
                                    constraint_skips.push(skip);
                                }
                                Node::Identifier(binding) => {
                                    bindings.push((key.clone(), binding.clone()));
                                }
                                _ => {
                                    // Complex expression constraint: dict[key] == expr.
                                    self.chunk.emit(Op::Dup, self.line);
                                    let key_idx =
                                        self.chunk.add_constant(Constant::String(key.clone()));
                                    self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                                    self.chunk.emit(Op::Subscript, self.line);
                                    self.compile_node(&entry.value)?;
                                    self.chunk.emit(Op::Equal, self.line);
                                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                    self.chunk.emit(Op::Pop, self.line);
                                    constraint_skips.push(skip);
                                }
                            }
                        }
                    }

                    for (key, binding) in &bindings {
                        self.chunk.emit(Op::Dup, self.line);
                        let key_idx = self.chunk.add_constant(Constant::String(key.clone()));
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                        self.chunk.emit(Op::Subscript, self.line);
                        let name_idx = self.chunk.add_constant(Constant::String(binding.clone()));
                        self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                    }

                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        // Guard failed: pop guard bool, fall through to scope cleanup below.
                        self.chunk.emit(Op::Pop, self.line);
                    } else {
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }

                    let type_fail_target = self.chunk.code.len();
                    self.chunk.emit(Op::Pop, self.line);
                    let next_arm_jump = self.chunk.emit_jump(Op::Jump, self.line);
                    let scoped_fail_target = self.chunk.code.len();
                    self.chunk.emit(Op::PopScope, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    let next_arm_target = self.chunk.code.len();

                    for skip in constraint_skips {
                        self.chunk.patch_jump_to(skip, scoped_fail_target);
                    }
                    self.chunk.patch_jump_to(skip_type, type_fail_target);
                    self.chunk.patch_jump_to(next_arm_jump, next_arm_target);
                }
                // List pattern: [literal, binding, ...]
                Node::ListLiteral(elements) => {
                    self.chunk.emit(Op::Dup, self.line);
                    let typeof_idx = self.chunk.add_constant(Constant::String("type_of".into()));
                    self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let list_str = self.chunk.add_constant(Constant::String("list".into()));
                    self.chunk.emit_u16(Op::Constant, list_str, self.line);
                    self.chunk.emit(Op::Equal, self.line);
                    let skip_type = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);

                    self.chunk.emit(Op::Dup, self.line);
                    let len_idx = self.chunk.add_constant(Constant::String("len".into()));
                    self.chunk.emit_u16(Op::Constant, len_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let count = self
                        .chunk
                        .add_constant(Constant::Int(elements.len() as i64));
                    self.chunk.emit_u16(Op::Constant, count, self.line);
                    self.chunk.emit(Op::GreaterEqual, self.line);
                    let skip_len = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);

                    let mut constraint_skips = Vec::new();
                    let mut bindings = Vec::new();
                    self.begin_scope();
                    for (i, elem) in elements.iter().enumerate() {
                        match &elem.node {
                            Node::Identifier(name) if name != "_" => {
                                bindings.push((i, name.clone()));
                            }
                            Node::Identifier(_) => {} // wildcard `_`
                            _ => {
                                self.chunk.emit(Op::Dup, self.line);
                                let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                                self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                                self.chunk.emit(Op::Subscript, self.line);
                                self.compile_node(elem)?;
                                self.chunk.emit(Op::Equal, self.line);
                                let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                                self.chunk.emit(Op::Pop, self.line);
                                constraint_skips.push(skip);
                            }
                        }
                    }

                    for (i, name) in &bindings {
                        self.chunk.emit(Op::Dup, self.line);
                        let idx_const = self.chunk.add_constant(Constant::Int(*i as i64));
                        self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                        self.chunk.emit(Op::Subscript, self.line);
                        let name_idx = self.chunk.add_constant(Constant::String(name.clone()));
                        self.chunk.emit_u16(Op::DefLet, name_idx, self.line);
                    }

                    // Optional guard
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        self.chunk.patch_jump(guard_skip);
                        self.chunk.emit(Op::Pop, self.line);
                    } else {
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }

                    let pre_scope_fail_target = self.chunk.code.len();
                    self.chunk.emit(Op::Pop, self.line);
                    let next_arm_jump = self.chunk.emit_jump(Op::Jump, self.line);
                    let scoped_fail_target = self.chunk.code.len();
                    self.chunk.emit(Op::PopScope, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    let next_arm_target = self.chunk.code.len();
                    for skip in constraint_skips {
                        self.chunk.patch_jump_to(skip, scoped_fail_target);
                    }
                    self.chunk.patch_jump_to(skip_len, pre_scope_fail_target);
                    self.chunk.patch_jump_to(skip_type, pre_scope_fail_target);
                    self.chunk.patch_jump_to(next_arm_jump, next_arm_target);
                }
                // Or-pattern: `p1 | p2 | ... | pN -> body`. Each
                // alternative is compared to the match value via
                // `Dup; compile(pi); Equal`. A hit on any alternative
                // (JumpIfTrue) threads into the shared body; only when
                // every alternative fails does the arm fall through to
                // the next one via the final `JumpIfFalse`.
                //
                // Stack discipline mirrors the literal-pattern case:
                // `match_val` stays on the stack throughout the arm,
                // and both the match-fail and guard-fail paths converge
                // on a single trailing `Pop` that removes whichever
                // false bool is on top.
                Node::OrPattern(alternatives) if !alternatives.is_empty() => {
                    let mut success_jumps = Vec::new();
                    let last = alternatives.len() - 1;
                    let mut final_skip: Option<usize> = None;
                    for (i, alt) in alternatives.iter().enumerate() {
                        self.chunk.emit(Op::Dup, self.line);
                        self.compile_node(alt)?;
                        self.chunk.emit(Op::Equal, self.line);
                        if i < last {
                            success_jumps.push(self.chunk.emit_jump(Op::JumpIfTrue, self.line));
                            self.chunk.emit(Op::Pop, self.line);
                        } else {
                            final_skip = Some(self.chunk.emit_jump(Op::JumpIfFalse, self.line));
                        }
                    }
                    for j in success_jumps {
                        self.chunk.patch_jump(j);
                    }
                    // Shared success entry: true bool sits atop match_val.
                    self.chunk.emit(Op::Pop, self.line);
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        // Guard fail: the false guard bool sits on top
                        // of match_val. Fall through to the trailing
                        // Pop (shared with match-fail) — do NOT emit an
                        // extra Pop here, or match_val gets consumed.
                        self.chunk.patch_jump(guard_skip);
                    } else {
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                    if let Some(skip) = final_skip {
                        self.chunk.patch_jump(skip);
                    }
                    self.chunk.emit(Op::Pop, self.line);
                }
                // Literal/expression pattern — compare with Equal.
                _ => {
                    self.chunk.emit(Op::Dup, self.line);
                    self.compile_node(&arm.pattern)?;
                    self.chunk.emit(Op::Equal, self.line);
                    let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);
                    if let Some(ref guard) = arm.guard {
                        self.compile_node(guard)?;
                        let guard_skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                        self.chunk.emit(Op::Pop, self.line);
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                        // Guard fail: fall through to the shared trailing
                        // Pop (same as match-fail). Emitting an extra
                        // Pop here would consume match_val and break the
                        // next arm.
                        self.chunk.patch_jump(guard_skip);
                    } else {
                        self.begin_scope();
                        self.chunk.emit(Op::Pop, self.line);
                        self.compile_match_body(&arm.body)?;
                        self.end_scope();
                        end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
                    }
                    self.chunk.patch_jump(skip);
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
        let msg_idx = self.chunk.add_constant(Constant::String(
            "No match arm matched the value".to_string(),
        ));
        self.chunk.emit(Op::Pop, self.line);
        self.chunk.emit_u16(Op::Constant, msg_idx, self.line);
        self.chunk.emit(Op::Throw, self.line);
        for j in end_jumps {
            self.chunk.patch_jump(j);
        }
        Ok(())
    }
}
