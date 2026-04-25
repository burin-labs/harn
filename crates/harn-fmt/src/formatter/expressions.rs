use harn_lexer::StringSegment;
use harn_parser::{Node, ParallelMode, SNode, TypedParam};

use crate::helpers::*;

use super::Formatter;

impl Formatter<'_> {
    pub(crate) fn format_expr(&self, node: &SNode) -> String {
        match &node.node {
            Node::StringLiteral(s) => {
                if node.span.line != node.span.end_line {
                    self.source_slice(node).to_string()
                } else {
                    let escaped = escape_string(s);
                    format!("\"{escaped}\"")
                }
            }
            Node::RawStringLiteral(s) => {
                format!("r\"{s}\"")
            }
            Node::InterpolatedString(segments) => {
                if node.span.line != node.span.end_line {
                    return self.source_slice(node).to_string();
                }
                let mut result = String::from("\"");
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => result.push_str(&escape_string(s)),
                        StringSegment::Expression(e, _, _) => {
                            result.push_str(&format!("${{{e}}}"));
                        }
                    }
                }
                result.push('"');
                result
            }
            Node::IntLiteral(n) => n.to_string(),
            Node::FloatLiteral(f) => format_float(*f),
            Node::BoolLiteral(b) => b.to_string(),
            Node::NilLiteral => "nil".to_string(),
            Node::Identifier(name) => name.clone(),
            Node::DurationLiteral(ms) => format_duration(*ms),
            Node::BinaryOp { op, left, right } => {
                let mut l = self.format_expr(left);
                let mut r = self.format_expr(right);
                let op_str = if op == "not_in" {
                    "not in"
                } else {
                    op.as_str()
                };

                if child_needs_parens(op, &left.node, false) {
                    l = format!("({l})");
                }
                if child_needs_parens(op, &right.node, true) {
                    r = format!("({r})");
                }

                let inline = format!("{l} {op_str} {r}");
                let should_break = left.span.line < right.span.line
                    || self.indent * 2 + inline.len() > self.line_width;

                if should_break {
                    let pad = "  ".repeat(self.indent + 1);
                    if op_safe_after_newline(op_str) {
                        format!("{l}\n{pad}{op_str} {r}")
                    } else {
                        format!("{l} \\\n{pad}{op_str} {r}")
                    }
                } else {
                    inline
                }
            }
            Node::UnaryOp { op, operand } => {
                let expr = self.format_expr(operand);
                if needs_parens_as_unary_operand(&operand.node) {
                    format!("{op}({expr})")
                } else {
                    format!("{op}{expr}")
                }
            }
            Node::TryOperator { operand } => {
                let expr = self.format_expr(operand);
                if needs_parens_as_postfix_object(&operand.node) {
                    format!("({expr})?")
                } else {
                    format!("{expr}?")
                }
            }
            Node::TryStar { operand } => {
                let expr = self.format_expr(operand);
                if needs_parens_as_unary_operand(&operand.node) {
                    format!("try* ({expr})")
                } else {
                    format!("try* {expr}")
                }
            }
            Node::FunctionCall { name, args } => {
                let args_str = self.format_call_args(args, name.len() + 1);
                format!("{name}({args_str})")
            }
            Node::MethodCall {
                object,
                method,
                args,
            } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let args_str = self.format_call_args(args, obj.len() + method.len() + 2);
                if object.span.end_line > 0 && node.span.end_line > object.span.end_line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{obj}\n{pad}.{method}({args_str})")
                } else {
                    format!("{obj}.{method}({args_str})")
                }
            }
            Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let args_str = self.format_call_args(args, obj.len() + method.len() + 3);
                if object.span.end_line > 0 && node.span.end_line > object.span.end_line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{obj}\n{pad}?.{method}({args_str})")
                } else {
                    format!("{obj}?.{method}({args_str})")
                }
            }
            Node::PropertyAccess { object, property } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                format!("{obj}.{property}")
            }
            Node::OptionalPropertyAccess { object, property } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                format!("{obj}?.{property}")
            }
            Node::SubscriptAccess { object, index } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let idx = self.format_expr(index);
                format!("{obj}[{idx}]")
            }
            Node::OptionalSubscriptAccess { object, index } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let idx = self.format_expr(index);
                format!("{obj}?[{idx}]")
            }
            Node::SliceAccess { object, start, end } => {
                let mut obj = self.format_expr(object);
                if needs_parens_as_postfix_object(&object.node) {
                    obj = format!("({obj})");
                }
                let s = start
                    .as_ref()
                    .map(|n| self.format_expr(n))
                    .unwrap_or_default();
                let e = end
                    .as_ref()
                    .map(|n| self.format_expr(n))
                    .unwrap_or_default();
                format!("{obj}[{s}:{e}]")
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond = self.format_expr(condition);
                let t = self.format_expr(true_expr);
                let f = self.format_expr(false_expr);
                format!("{cond} ? {t} : {f}")
            }
            Node::Assignment {
                target, value, op, ..
            } => {
                let t = self.format_expr(target);
                let v = self.format_expr(value);
                if let Some(op) = op {
                    format!("{t} {op}= {v}")
                } else {
                    format!("{t} = {v}")
                }
            }
            Node::ListLiteral(elems) => {
                let items = self
                    .format_comma_sequence(elems.iter().map(|e| self.format_expr(e)).collect(), 1);
                format!("[{items}]")
            }
            Node::DictLiteral(entries) => self.format_dict_entries(entries),
            Node::RangeExpr {
                start,
                end,
                inclusive,
            } => {
                let s = self.format_expr(start);
                let e = self.format_expr(end);
                if *inclusive {
                    format!("{s} to {e}")
                } else {
                    format!("{s} to {e} exclusive")
                }
            }
            Node::Closure {
                params,
                body,
                fn_syntax,
            } => {
                if *fn_syntax {
                    self.format_fn_closure(params, body)
                } else {
                    self.format_arrow_closure(params, body)
                }
            }
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                if args.is_empty() {
                    format!("{enum_name}.{variant}")
                } else {
                    let args_str = self.format_call_args(args, enum_name.len() + variant.len() + 2);
                    format!("{enum_name}.{variant}({args_str})")
                }
            }
            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                let items = self.format_dict_entry_list(fields, struct_name.len() + 2);
                format!("{struct_name} {{{items}}}")
            }
            Node::DeferStmt { body } => self.format_block_expr("defer {", body),
            Node::SpawnExpr { body } => self.format_block_expr("spawn {", body),
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    format!("yield {}", self.format_expr(val))
                } else {
                    "yield".to_string()
                }
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    format!("return {}", self.format_expr(val))
                } else {
                    "return".to_string()
                }
            }
            Node::ThrowStmt { value } => format!("throw {}", self.format_expr(value)),
            Node::BreakStmt => "break".to_string(),
            Node::ContinueStmt => "continue".to_string(),
            Node::Block(stmts) => self.format_block_expr("{", stmts),
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value);
                let mut result = format!("match {val} {{\n");
                let arm_indent = self.indent + 1;
                for arm in arms {
                    let indent_str = "  ".repeat(arm_indent);
                    let pattern = self.format_expr(&arm.pattern);
                    let guard_str = if let Some(ref guard) = arm.guard {
                        format!(" if {}", self.format_expr(guard))
                    } else {
                        String::new()
                    };
                    if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
                        let expr = self.format_expr(&arm.body[0]);
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern}{guard_str} -> {{ {expr} }}\n"));
                    } else {
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern}{guard_str} -> {{\n"));
                        result.push_str(&self.format_body_string(&arm.body, arm_indent + 1));
                        result.push_str(&indent_str);
                        result.push_str("}\n");
                    }
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                self.format_block_expr(&format!("guard {cond} else {{"), else_body)
            }
            Node::RequireStmt { condition, message } => {
                let cond = self.format_expr(condition);
                if let Some(message) = message {
                    format!("require {cond}, {}", self.format_expr(message))
                } else {
                    format!("require {cond}")
                }
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration);
                self.format_block_expr(&format!("deadline {dur} {{"), body)
            }
            Node::MutexBlock { body } => self.format_block_expr("mutex {", body),
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                let indent = self.indent + 1;
                let mut result = format!("if {cond} {{\n");
                result.push_str(&self.format_body_string(then_body, indent));
                let close = "  ".repeat(self.indent);
                if let Some(eb) = else_body {
                    result.push_str(&close);
                    result.push_str("} else {\n");
                    result.push_str(&self.format_body_string(eb, indent));
                    result.push_str(&close);
                    result.push('}');
                } else {
                    result.push_str(&close);
                    result.push('}');
                }
                result
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let pat = format_pattern(pattern);
                let iter_str = self.format_expr(iterable);
                self.format_block_expr(&format!("for {pat} in {iter_str} {{"), body)
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                self.format_block_expr(&format!("while {cond} {{"), body)
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count);
                self.format_block_expr(&format!("retry {cnt} {{"), body)
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                let indent = self.indent + 1;
                let close = "  ".repeat(self.indent);
                let mut result = String::from("try {\n");
                result.push_str(&self.format_body_string(body, indent));
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = format_catch_param(error_var, error_type);
                    result.push_str(&close);
                    result.push_str(&format!("}} catch{catch_param} {{\n"));
                    result.push_str(&self.format_body_string(catch_body, indent));
                }
                if let Some(fb) = finally_body {
                    result.push_str(&close);
                    result.push_str("} finally {\n");
                    result.push_str(&self.format_body_string(fb, indent));
                }
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::TryExpr { body } => self.format_block_expr("try {", body),
            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                let e = self.format_expr(expr);
                let keyword = match mode {
                    ParallelMode::Count => "parallel",
                    ParallelMode::Each => "parallel each",
                    ParallelMode::Settle => "parallel settle",
                };
                let options_clause = if options.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> = options
                        .iter()
                        .map(|(key, value)| format!("{key}: {}", self.format_expr(value)))
                        .collect();
                    format!(" with {{ {} }}", formatted.join(", "))
                };
                let opening = if let Some(var) = variable {
                    format!("{keyword} {e}{options_clause} {{ {var} ->")
                } else {
                    format!("{keyword} {e}{options_clause} {{")
                };
                self.format_block_expr(&opening, body)
            }
            // Declaration nodes rendered as placeholders when used in expr position.
            Node::Pipeline { name, .. } => format!("/* pipeline {name} */"),
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let sig = self.format_fn_signature(
                    pub_prefix,
                    name,
                    type_params,
                    params,
                    return_type,
                    where_clauses,
                    self.indent,
                );
                self.format_block_expr(&format!("{sig} {{"), body)
            }
            Node::ToolDecl {
                name,
                description,
                params,
                return_type,
                body,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let ret = if let Some(rt) = return_type {
                    format!(" -> {}", format_type_expr(rt))
                } else {
                    String::new()
                };
                let prefix_len = self.indent * 2 + pub_prefix.len() + 5 + name.len() + 1;
                let params_str = self.format_typed_params_wrapped(params, prefix_len);
                let mut effective_body = Vec::new();
                if let Some(desc) = description {
                    let escaped = escape_string(desc);
                    effective_body.push(harn_parser::Spanned::dummy(Node::FunctionCall {
                        name: "description".to_string(),
                        args: vec![harn_parser::Spanned::dummy(Node::StringLiteral(escaped))],
                    }));
                }
                effective_body.extend(body.iter().cloned());
                self.format_block_expr(
                    &format!("{pub_prefix}tool {name}({params_str}){ret} {{"),
                    &effective_body,
                )
            }
            Node::SkillDecl {
                name,
                fields,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let item_indent = "  ".repeat(self.indent + 1);
                let close_indent = "  ".repeat(self.indent);
                let mut inner = String::new();
                for (field_name, field_expr) in fields {
                    let expr_str = self.format_expr(field_expr);
                    inner.push_str(&item_indent);
                    inner.push_str(field_name);
                    inner.push(' ');
                    inner.push_str(&expr_str);
                    inner.push('\n');
                }
                format!("{pub_prefix}skill {name} {{\n{inner}{close_indent}}}")
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("let {pat}{type_str} = {val}")
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                format!("var {pat}{type_str} = {val}")
            }
            Node::ImportDecl { path } => format!("import \"{path}\""),
            Node::SelectiveImport { names, path } => {
                self.format_selective_import_names(names, path)
            }
            Node::EnumDecl { name, .. } => format!("/* enum {name} */"),
            Node::StructDecl { name, .. } => format!("/* struct {name} */"),
            Node::InterfaceDecl { name, .. } => format!("/* interface {name} */"),
            Node::ImplBlock { type_name, .. } => format!("/* impl {type_name} */"),
            Node::OverrideDecl { name, .. } => format!("/* override {name} */"),
            Node::TypeDecl {
                name,
                type_params,
                type_expr,
            } => {
                let params = format_type_params(type_params);
                let te = format_type_expr(type_expr);
                format!("type {name}{params} = {te}")
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let mut result = String::from("select {\n");
                let case_indent = self.indent + 1;
                let body_indent = case_indent + 1;
                let case_pad = "  ".repeat(case_indent);
                for case in cases {
                    let ch = self.format_expr(&case.channel);
                    result.push_str(&format!("{case_pad}{} from {ch} {{\n", case.variable));
                    result.push_str(&self.format_body_string(&case.body, body_indent));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                if let Some((dur, body)) = timeout {
                    let d = self.format_expr(dur);
                    result.push_str(&format!("{case_pad}timeout {d} {{\n"));
                    result.push_str(&self.format_body_string(body, body_indent));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                if let Some(body) = default_body {
                    result.push_str(&format!("{case_pad}default {{\n"));
                    result.push_str(&self.format_body_string(body, body_indent));
                    result.push_str(&case_pad);
                    result.push_str("}\n");
                }
                let close = "  ".repeat(self.indent);
                result.push_str(&close);
                result.push('}');
                result
            }
            Node::Spread(inner) => format!("...{}", self.format_expr(inner)),
            Node::AttributedDecl { attributes, inner } => {
                let attrs = format_attributes(attributes);
                format!("{}{}", attrs, self.format_expr(inner))
            }
            Node::OrPattern(alternatives) => alternatives
                .iter()
                .map(|p| self.format_expr(p))
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }

    fn format_arrow_closure(&self, params: &[TypedParam], body: &[SNode]) -> String {
        let params_str = format_typed_params(params);
        if body.len() == 1 && is_simple_expr(&body[0]) {
            let expr = self.format_expr(&body[0]);
            if params.is_empty() {
                format!("{{ {expr} }}")
            } else {
                format!("{{ {params_str} -> {expr} }}")
            }
        } else {
            let opening = if params.is_empty() {
                String::from("{")
            } else {
                format!("{{ {params_str} ->")
            };
            self.format_block_expr(&opening, body)
        }
    }

    fn format_fn_closure(&self, params: &[TypedParam], body: &[SNode]) -> String {
        let params_str = format_typed_params(params);
        if body.len() == 1 && is_simple_expr(&body[0]) {
            let expr = self.format_expr(&body[0]);
            format!("fn({params_str}) {{ {expr} }}")
        } else {
            self.format_block_expr(&format!("fn({params_str}) {{"), body)
        }
    }

    fn format_dict_key(&self, node: &SNode) -> String {
        match &node.node {
            Node::StringLiteral(s) if is_identifier(s) => s.clone(),
            _ => format!("[{}]", self.format_expr(node)),
        }
    }

    fn format_dict_entries(&self, entries: &[harn_parser::DictEntry]) -> String {
        let items = self.format_dict_entry_list(entries, 1);
        format!("{{{items}}}")
    }

    fn format_dict_entry_list(
        &self,
        entries: &[harn_parser::DictEntry],
        prefix_len: usize,
    ) -> String {
        let rendered = entries
            .iter()
            .map(|e| {
                if let Node::Spread(inner) = &e.value.node {
                    return format!("...{}", self.format_expr(inner));
                }
                let k = self.format_dict_key(&e.key);
                let v = self.format_expr(&e.value);
                format!("{k}: {v}")
            })
            .collect::<Vec<_>>();
        self.format_comma_sequence(rendered, prefix_len)
    }
}
