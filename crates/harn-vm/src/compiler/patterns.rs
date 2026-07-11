use harn_parser::{is_discard_name, BindingPattern, Node, SNode};

use crate::chunk::{Constant, Op};

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    fn emit_binding_target(&mut self, name: &str, mutable: bool) {
        if is_discard_name(name) {
            self.chunk.emit(Op::Pop, self.line);
            return;
        }
        self.emit_define_binding(name, mutable);
    }

    /// Compile a destructuring binding pattern.
    /// Expects the RHS value to already be on the stack.
    /// After this, the value is consumed (popped) and each binding is defined.
    pub(super) fn compile_destructuring(
        &mut self,
        pattern: &BindingPattern,
        is_mutable: bool,
    ) -> Result<(), CompileError> {
        match pattern {
            BindingPattern::Identifier(name) => {
                self.emit_binding_target(name, is_mutable);
            }
            BindingPattern::Dict(fields) => {
                // Runtime `__assert_dict(value)` type check on the RHS.
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self.string_constant("__assert_dict");
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line);

                let non_rest: Vec<_> = fields.iter().filter(|f| !f.is_rest).collect();
                let rest_field = fields.iter().find(|f| f.is_rest);

                for field in &non_rest {
                    self.chunk.emit(Op::Dup, self.line);
                    let key_idx = self.string_constant(&field.key);
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
                    self.emit_binding_target(binding_name, is_mutable);
                }

                if let Some(rest) = rest_field {
                    // `__dict_rest(dict, [keys_to_exclude])`.
                    let fn_idx = self.string_constant("__dict_rest");
                    self.chunk.emit_u16(Op::Constant, fn_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    for field in &non_rest {
                        let key_idx = self.string_constant(&field.key);
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                    }
                    self.chunk
                        .emit_u16(Op::BuildList, non_rest.len() as u16, self.line);
                    self.chunk.emit_u8(Op::Call, 2, self.line);
                    let rest_name = &rest.key;
                    self.emit_binding_target(rest_name, is_mutable);
                } else {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
            BindingPattern::Pair(first_name, second_name) => {
                // Runtime `__assert_pair(value)` guard on the RHS. Without it,
                // extracting `.first`/`.second` from a non-Pair (a
                // `{index, value}` dict from `list.enumerate()`, an `[a, b]`
                // list from `list.zip(...)`) silently yields nil for both
                // names instead of failing loudly.
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self.string_constant("__assert_pair");
                self.chunk.emit_u16(Op::Constant, assert_idx, self.line);
                self.chunk.emit(Op::Swap, self.line);
                self.chunk.emit_u8(Op::Call, 1, self.line);
                self.chunk.emit(Op::Pop, self.line);

                self.chunk.emit(Op::Dup, self.line);
                let first_key_idx = self.string_constant("first");
                self.chunk
                    .emit_u16(Op::GetProperty, first_key_idx, self.line);
                self.emit_binding_target(first_name, is_mutable);

                let second_key_idx = self.string_constant("second");
                self.chunk
                    .emit_u16(Op::GetProperty, second_key_idx, self.line);
                self.emit_binding_target(second_name, is_mutable);
                // No trailing Pop: GetProperty consumed the source pair.
            }
            BindingPattern::List(elements) => {
                // Runtime `__assert_list(value)` type check on the RHS.
                self.chunk.emit(Op::Dup, self.line);
                let assert_idx = self.string_constant("__assert_list");
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
                    self.emit_binding_target(&elem.name, is_mutable);
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
                    self.emit_binding_target(&rest.name, is_mutable);
                } else {
                    self.chunk.emit(Op::Pop, self.line);
                }
            }
        }
        Ok(())
    }

    /// Split a `match` list-pattern's elements into the leading element
    /// patterns and an optional trailing `...rest` binding name (`None` for a
    /// `..._` discard). Rejects a spread that is not last, or more than one.
    fn split_match_list_pattern<'a>(
        &self,
        elements: &'a [SNode],
    ) -> Result<(&'a [SNode], Option<String>), CompileError> {
        let spread_count = elements
            .iter()
            .filter(|e| matches!(e.node, Node::Spread(_)))
            .count();
        if spread_count == 0 {
            return Ok((elements, None));
        }
        let last_is_spread = matches!(elements.last().map(|e| &e.node), Some(Node::Spread(_)));
        if spread_count > 1 || !last_is_spread {
            return Err(CompileError {
                message:
                    "`...rest` must be the last element of a list pattern, and only one is allowed"
                        .into(),
                line: self.line,
            });
        }
        let Some(Node::Spread(inner)) = elements.last().map(|e| &e.node) else {
            unreachable!("checked last is a spread above")
        };
        let rest_bind = match &inner.node {
            Node::Identifier(name) if name == "_" => None,
            Node::Identifier(name) => Some(name.clone()),
            _ => {
                return Err(CompileError {
                    message: "`...rest` in a list pattern must bind an identifier".into(),
                    line: self.line,
                });
            }
        };
        Ok((&elements[..elements.len() - 1], rest_bind))
    }

    /// Compile a `match` expression (`Node::MatchExpr`).
    /// Reject a compound sub-pattern (a nested list/dict literal) appearing as
    /// an element of a list pattern or the value of a dict pattern.
    ///
    /// The list/dict pattern compilers only destructure *flat* sub-patterns
    /// (a bare identifier binding, `_`, or a literal equality constraint). A
    /// nested `[..]`/`{..}` sub-pattern silently fell through to the equality
    /// catch-all and was compiled as a *value expression* — so `match xs {
    /// [[a, b], ...] -> ... }` compared against a freshly-built list using `a`
    /// and `b` as variables (binding nothing, throwing "undefined variable" or,
    /// if those names happened to exist, matching by structural equality and
    /// binding the wrong values). The sibling `let`-destructure surface rejects
    /// nested patterns at parse time; mirror that here with a clear compile
    /// error instead of the silent miscompile.
    fn reject_nested_match_subpattern(&self, node: &SNode) -> Result<(), CompileError> {
        if matches!(&node.node, Node::ListLiteral(_) | Node::DictLiteral(_)) {
            return Err(CompileError {
                message: "nested list/dict patterns are not supported in match arms; \
                          bind the element with an identifier and match it in a nested `match`"
                    .to_string(),
                line: node.span.line as u32,
            });
        }
        Ok(())
    }

    /// Resolve a bare call-shaped pattern name (`Ok(v)`) to its owning enum.
    /// `Some(enum_name)` when exactly one visible enum declares the variant;
    /// an error when several do (the pattern must be qualified); `None` when
    /// no enum declares it (the pattern falls through to expression-equality
    /// compilation, preserving `match x { compute() -> ... }`).
    fn resolve_bare_variant_enum(
        &self,
        variant: &str,
        line: u32,
    ) -> Result<Option<String>, CompileError> {
        match self.enum_variant_owners.get(variant) {
            None => Ok(None),
            Some(owners) if owners.len() == 1 => Ok(Some(owners[0].clone())),
            Some(owners) => Err(CompileError {
                message: format!(
                    "match pattern `{variant}(...)` is ambiguous: variant `{variant}` is declared by enums {}; qualify it as `{}.{variant}(...)`",
                    owners.join(", "),
                    owners[0],
                ),
                line,
            }),
        }
    }

    /// Shared codegen for an enum-variant match arm (`Result.Ok(v)`,
    /// `EnumName.Variant(x)`, `Ok(v)`): MatchEnum test, payload bindings,
    /// optional guard, body, and the fail path. Expects the match value on
    /// the stack; leaves it there for the next arm on failure.
    fn compile_enum_variant_arm(
        &mut self,
        enum_name: &str,
        variant: &str,
        pat_args: &[SNode],
        arm: &harn_parser::MatchArm,
        end_jumps: &mut Vec<usize>,
    ) -> Result<(), CompileError> {
        self.chunk.emit(Op::Dup, self.line);
        let en_idx = self.string_constant(enum_name);
        let vn_idx = self.string_constant(variant);
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

        // Bind field variables from the enum's fields; the match value
        // stays on the stack for extraction.
        for (i, pat_arg) in pat_args.iter().enumerate() {
            if let Node::Identifier(binding_name) = &pat_arg.node {
                self.chunk.emit(Op::Dup, self.line);
                let fields_idx = self.string_constant("fields");
                self.chunk.emit_u16(Op::GetProperty, fields_idx, self.line);
                let idx_const = self.chunk.add_constant(Constant::Int(i as i64));
                self.chunk.emit_u16(Op::Constant, idx_const, self.line);
                self.chunk.emit(Op::Subscript, self.line);
                self.emit_binding_target(binding_name, false);
            }
        }

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
        Ok(())
    }

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
                    self.compile_enum_variant_arm(
                        enum_name,
                        variant,
                        pat_args,
                        arm,
                        &mut end_jumps,
                    )?;
                }
                // Bare call-shaped variant pattern: `Ok(v)`, `Some(x)` —
                // resolved to its owning enum when the variant name is
                // unambiguous; otherwise compiled as an expression-equality
                // pattern like any other call expression.
                Node::FunctionCall {
                    name,
                    args: pat_args,
                    ..
                } if self.resolve_bare_variant_enum(name, self.line)?.is_some() => {
                    let enum_name = self
                        .resolve_bare_variant_enum(name, self.line)?
                        .expect("guard checked Some");
                    self.compile_enum_variant_arm(&enum_name, name, pat_args, arm, &mut end_jumps)?;
                }
                // Enum variant without args: PropertyAccess(EnumName, Variant)
                Node::PropertyAccess { object, property } if matches!(&object.node, Node::Identifier(n) if self.enum_names.contains(n)) =>
                {
                    let enum_name = if let Node::Identifier(n) = &object.node {
                        n.as_str()
                    } else {
                        unreachable!()
                    };
                    self.chunk.emit(Op::Dup, self.line);
                    let en_idx = self.string_constant(enum_name);
                    let vn_idx = self.string_constant(property);
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
                    self.compile_enum_variant_arm(
                        &enum_name,
                        method,
                        pat_args,
                        arm,
                        &mut end_jumps,
                    )?;
                }
                // Binding pattern: bare identifier always matches.
                Node::Identifier(name) => {
                    self.begin_scope();
                    self.chunk.emit(Op::Dup, self.line);
                    self.emit_binding_target(name, false);
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
                    let typeof_idx = self.string_constant("type_of");
                    self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let dict_str = self.string_constant("dict");
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
                                    let key_idx = self.string_constant(key);
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
                                    self.reject_nested_match_subpattern(&entry.value)?;
                                    // Complex expression constraint: dict[key] == expr.
                                    self.chunk.emit(Op::Dup, self.line);
                                    let key_idx = self.string_constant(key);
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
                        let key_idx = self.string_constant(key);
                        self.chunk.emit_u16(Op::Constant, key_idx, self.line);
                        self.chunk.emit(Op::Subscript, self.line);
                        self.emit_binding_target(binding, false);
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
                // List pattern: `[p0, p1, ...]` matches a list of EXACTLY that
                // length; `[p0, ...rest]` matches a list of AT LEAST the leading
                // arity and binds the remainder. The trailing `...rest` is the
                // only spread allowed, mirroring `let`-destructuring.
                Node::ListLiteral(elements) => {
                    let (leading, rest_bind) = self.split_match_list_pattern(elements)?;
                    let has_rest = leading.len() != elements.len();

                    self.chunk.emit(Op::Dup, self.line);
                    let typeof_idx = self.string_constant("type_of");
                    self.chunk.emit_u16(Op::Constant, typeof_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let list_str = self.string_constant("list");
                    self.chunk.emit_u16(Op::Constant, list_str, self.line);
                    self.chunk.emit(Op::Equal, self.line);
                    let skip_type = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);

                    self.chunk.emit(Op::Dup, self.line);
                    let len_idx = self.string_constant("len");
                    self.chunk.emit_u16(Op::Constant, len_idx, self.line);
                    self.chunk.emit(Op::Swap, self.line);
                    self.chunk.emit_u8(Op::Call, 1, self.line);
                    let count = self.chunk.add_constant(Constant::Int(leading.len() as i64));
                    self.chunk.emit_u16(Op::Constant, count, self.line);
                    // Exact length with no rest; at-least length with `...rest`.
                    let length_op = if has_rest {
                        Op::GreaterEqual
                    } else {
                        Op::Equal
                    };
                    self.chunk.emit(length_op, self.line);
                    let skip_len = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                    self.chunk.emit(Op::Pop, self.line);

                    let mut constraint_skips = Vec::new();
                    let mut bindings = Vec::new();
                    self.begin_scope();
                    for (i, elem) in leading.iter().enumerate() {
                        match &elem.node {
                            Node::Identifier(name) if name != "_" => {
                                bindings.push((i, name.clone()));
                            }
                            Node::Identifier(_) => {} // wildcard `_`
                            _ => {
                                self.reject_nested_match_subpattern(elem)?;
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
                        self.emit_binding_target(name, false);
                    }

                    // `...rest` binds the tail `list[leading..]`. `Dup` first so
                    // the match value stays on the stack for the fail/Pop path.
                    if let Some(rest_name) = &rest_bind {
                        self.chunk.emit(Op::Dup, self.line);
                        let start_idx =
                            self.chunk.add_constant(Constant::Int(leading.len() as i64));
                        self.chunk.emit_u16(Op::Constant, start_idx, self.line);
                        self.chunk.emit(Op::Nil, self.line);
                        self.chunk.emit(Op::Slice, self.line);
                        self.emit_binding_target(rest_name, false);
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
        let msg_idx = self.string_constant("No match arm matched the value");
        self.chunk.emit(Op::Pop, self.line);
        self.chunk.emit_u16(Op::Constant, msg_idx, self.line);
        self.chunk.emit(Op::Throw, self.line);
        for j in end_jumps {
            self.chunk.patch_jump(j);
        }
        Ok(())
    }
}
