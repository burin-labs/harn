use harn_lexer::StringSegment;
use harn_parser::{Node, SNode, TypedParam};

use crate::helpers::*;
use crate::Formatter;

impl Formatter {
    // ------------------------------------------------------------------
    // Shared helpers
    // ------------------------------------------------------------------

    /// Format a list of body statements as a string at the given indent level.
    /// Each statement is formatted and indented; the result does NOT include
    /// opening/closing braces — only the inner lines (each ending with `\n`).
    fn format_body_string(&self, body: &[SNode], indent_level: usize) -> String {
        let mut out = String::new();
        let indent_str = "  ".repeat(indent_level);
        for n in body {
            let expr = self.format_expr_or_stmt(n, indent_level);
            out.push_str(&indent_str);
            out.push_str(&expr);
            out.push('\n');
        }
        out
    }

    /// Build `opening\n  body\nclose}` string for a simple block expression.
    fn format_block_expr(&self, opening: &str, body: &[SNode]) -> String {
        let inner = self.format_body_string(body, self.indent + 1);
        let close = "  ".repeat(self.indent);
        format!("{opening}\n{inner}{close}}}")
    }

    fn format_comma_sequence(&self, rendered: Vec<String>, prefix_len: usize) -> String {
        let inline = rendered.join(", ");
        let should_wrap = !rendered.is_empty()
            && (inline.contains('\n') || prefix_len + inline.len() + 1 > self.line_width);
        if !should_wrap {
            return inline;
        }
        let item_indent = "  ".repeat(self.indent + 1);
        let close_indent = "  ".repeat(self.indent);
        let mut out = String::new();
        out.push('\n');
        for arg in rendered {
            out.push_str(&item_indent);
            out.push_str(&arg);
            out.push_str(",\n");
        }
        out.push_str(&close_indent);
        out
    }

    fn format_typed_params_wrapped(&self, params: &[TypedParam], prefix_len: usize) -> String {
        self.format_comma_sequence(render_typed_params(params), prefix_len)
    }

    fn format_string_list_wrapped(&self, items: &[String], prefix_len: usize) -> String {
        self.format_comma_sequence(items.to_vec(), prefix_len)
    }

    fn format_call_args(&self, args: &[SNode], prefix_len: usize) -> String {
        let rendered = args
            .iter()
            .map(|arg| self.format_expr(arg))
            .collect::<Vec<_>>();
        self.format_comma_sequence(rendered, prefix_len)
    }

    /// Format selective import names, wrapping across lines when necessary.
    /// Unlike `format_comma_sequence`, this does NOT emit a trailing comma
    /// on the last item (the Harn parser rejects trailing commas in imports).
    fn format_selective_import_names(&self, names: &[String], path: &str) -> String {
        let inline = names.join(", ");
        let prefix_len = self.indent * 2 + 9; // "import { "
        let total = prefix_len + inline.len() + " } ".len() + 6 + path.len() + 1;
        if total > self.line_width {
            let item_indent = "  ".repeat(self.indent + 1);
            let close_indent = "  ".repeat(self.indent);
            let mut inner = String::from("\n");
            for (i, name) in names.iter().enumerate() {
                inner.push_str(&item_indent);
                inner.push_str(name);
                if i + 1 < names.len() {
                    inner.push(',');
                }
                inner.push('\n');
            }
            inner.push_str(&close_indent);
            format!("import {{{inner}}} from \"{path}\"")
        } else {
            format!("import {{ {inline} }} from \"{path}\"")
        }
    }

    /// Format a function signature's parameter list with wrapping awareness.
    fn format_fn_signature(
        &self,
        pub_prefix: &str,
        name: &str,
        type_params: &[harn_parser::TypeParam],
        params: &[TypedParam],
        return_type: &Option<harn_parser::TypeExpr>,
        where_clauses: &[harn_parser::WhereClause],
        indent_level: usize,
    ) -> String {
        let generics = format_type_params(type_params);
        let ret = if let Some(rt) = return_type {
            format!(" -> {}", format_type_expr(rt))
        } else {
            String::new()
        };
        let where_str = format_where_clauses(where_clauses);
        let prefix_len = indent_level * 2 + pub_prefix.len() + 3 + name.len() + generics.len() + 1;
        let params_str = self.format_typed_params_wrapped(params, prefix_len);
        format!("{pub_prefix}fn {name}{generics}({params_str}){ret}{where_str}")
    }

    // ------------------------------------------------------------------
    // Statement-level formatting (writes directly to self.output)
    // ------------------------------------------------------------------

    pub(crate) fn format_program(&mut self, nodes: &[SNode]) {
        if let Some(first) = nodes.first() {
            self.emit_comments_in_range(1, first.span.line);
        }
        for (i, node) in nodes.iter().enumerate() {
            if i > 0 {
                self.output.push('\n');
                let prev_end = nodes[i - 1].span.line + 1;
                self.emit_comments_in_range(prev_end, node.span.line);
            }
            self.format_node(node);
        }
        if !self.comments.is_empty() {
            let max_line = *self.comments.keys().max().unwrap_or(&0);
            let last_line = nodes.last().map(|n| n.span.line + 1).unwrap_or(1);
            self.emit_comments_in_range(last_line, max_line + 1);
        }
    }

    pub(crate) fn format_body(&mut self, nodes: &[SNode], block_start_line: usize) {
        for (i, node) in nodes.iter().enumerate() {
            let range_start = if i > 0 {
                nodes[i - 1].span.line + 1
            } else {
                block_start_line + 1
            };
            self.emit_comments_in_range(range_start, node.span.line);
            self.format_node(node);
        }
    }

    fn format_node(&mut self, node: &SNode) {
        let node_line = node.span.line;
        match &node.node {
            Node::Pipeline {
                name,
                params,
                body,
                extends,
            } => {
                let ext = if let Some(base) = extends {
                    format!(" extends {base}")
                } else {
                    String::new()
                };
                let prefix_len = self.indent * 2 + 9 + name.len() + 1;
                let params_str = self.format_string_list_wrapped(params, prefix_len);
                self.writeln(&format!("pipeline {name}({params_str}){ext} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                self.writeln(&format!("let {pat}{type_str} = {val}"));
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value);
                self.writeln(&format!("var {pat}{type_str} = {val}"));
            }
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
                self.writeln(&format!("{sig} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("if {cond} {{"));
                self.indent();
                self.format_body(then_body, node_line);
                self.dedent();
                if let Some(eb) = else_body {
                    if eb.len() == 1 {
                        if let Node::IfElse { .. } = &eb[0].node {
                            self.write_indent();
                            self.output.push_str("} else ");
                            self.format_node_no_indent(&eb[0]);
                            return;
                        }
                    }
                    self.writeln("} else {");
                    self.indent();
                    self.format_body(eb, node_line);
                    self.dedent();
                    self.writeln("}");
                } else {
                    self.writeln("}");
                }
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                let pat = format_pattern(pattern);
                let iter_str = self.format_expr(iterable);
                self.writeln(&format!("for {pat} in {iter_str} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("while {cond} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count);
                self.writeln(&format!("retry {cnt} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = format_catch_param(error_var, error_type);
                    self.writeln(&format!("}} catch{catch_param} {{"));
                    self.indent();
                    self.format_body(catch_body, node_line);
                    self.dedent();
                }
                if let Some(fb) = finally_body {
                    self.writeln("} finally {");
                    self.indent();
                    self.format_body(fb, node_line);
                    self.dedent();
                }
                self.writeln("}");
            }
            Node::TryExpr { body } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    self.writeln(&format!("return {v}"));
                } else {
                    self.writeln("return");
                }
            }
            Node::ThrowStmt { value } => {
                let v = self.format_expr(value);
                self.writeln(&format!("throw {v}"));
            }
            Node::BreakStmt => self.writeln("break"),
            Node::ContinueStmt => self.writeln("continue"),
            Node::ImportDecl { path } => {
                self.writeln(&format!("import \"{path}\""));
            }
            Node::SelectiveImport { names, path } => {
                self.writeln(&self.format_selective_import_names(names, path));
            }
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value);
                self.writeln(&format!("match {val} {{"));
                self.indent();
                for arm in arms {
                    self.format_match_arm(arm);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::EnumDecl { name, variants } => {
                self.writeln(&format!("enum {name} {{"));
                self.indent();
                for v in variants {
                    self.format_enum_variant(v);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::StructDecl { name, fields } => {
                self.writeln(&format!("struct {name} {{"));
                self.indent();
                for f in fields {
                    self.format_struct_field(f);
                }
                self.dedent();
                self.writeln("}");
            }
            Node::InterfaceDecl { name, methods } => {
                self.writeln(&format!("interface {name} {{"));
                self.indent();
                for m in methods {
                    let prefix_len = self.indent * 2 + 3 + m.name.len() + 1;
                    let params = self.format_typed_params_wrapped(&m.params, prefix_len);
                    if let Some(ret) = &m.return_type {
                        self.writeln(&format!(
                            "fn {}({}) -> {}",
                            m.name,
                            params,
                            format_type_expr(ret)
                        ));
                    } else {
                        self.writeln(&format!("fn {}({})", m.name, params));
                    }
                }
                self.dedent();
                self.writeln("}");
            }
            Node::ImplBlock { type_name, methods } => {
                self.writeln(&format!("impl {type_name} {{"));
                self.indent();
                self.format_body(methods, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::Parallel {
                count,
                variable,
                body,
            } => {
                let cnt = self.format_expr(count);
                if let Some(var) = variable {
                    self.writeln(&format!("parallel({cnt}) {{ {var} ->"));
                } else {
                    self.writeln(&format!("parallel({cnt}) {{"));
                }
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.writeln(&format!("parallel_map({lst}) {{ {variable} ->"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.writeln(&format!("parallel_settle({lst}) {{ {variable} ->"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::SpawnExpr { body } => {
                self.writeln("spawn {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                self.writeln(&format!("guard {cond} else {{"));
                self.indent();
                self.format_body(else_body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::RequireStmt { condition, message } => {
                let cond = self.format_expr(condition);
                if let Some(message) = message {
                    self.writeln(&format!("require {cond}, {}", self.format_expr(message)));
                } else {
                    self.writeln(&format!("require {cond}"));
                }
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration);
                self.writeln(&format!("deadline {dur} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::MutexBlock { body } => {
                self.writeln("mutex {");
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val);
                    self.writeln(&format!("yield {v}"));
                } else {
                    self.writeln("yield");
                }
            }
            Node::OverrideDecl { name, params, body } => {
                let prefix_len = self.indent * 2 + 9 + name.len() + 1;
                let params_str = self.format_string_list_wrapped(params, prefix_len);
                self.writeln(&format!("override {name}({params_str}) {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::TypeDecl { name, type_expr } => {
                let te = format_type_expr(type_expr);
                self.writeln(&format!("type {name} = {te}"));
            }
            Node::Block(stmts) => {
                self.writeln("{");
                self.indent();
                self.format_body(stmts, node_line);
                self.dedent();
                self.writeln("}");
            }
            // Everything else is an expression used as a statement
            _ => {
                let expr = self.format_expr(node);
                self.writeln(&expr);
            }
        }
    }

    /// Like format_node but without writing the leading indent (for else-if chains).
    fn format_node_no_indent(&mut self, node: &SNode) {
        let line = node.span.line;
        if let Node::IfElse {
            condition,
            then_body,
            else_body,
        } = &node.node
        {
            let cond = self.format_expr(condition);
            self.output.push_str(&format!("if {cond} {{\n"));
            self.indent();
            self.format_body(then_body, line);
            self.dedent();
            if let Some(eb) = else_body {
                if eb.len() == 1 {
                    if let Node::IfElse { .. } = &eb[0].node {
                        self.write_indent();
                        self.output.push_str("} else ");
                        self.format_node_no_indent(&eb[0]);
                        return;
                    }
                }
                self.writeln("} else {");
                self.indent();
                self.format_body(eb, line);
                self.dedent();
                self.writeln("}");
            } else {
                self.writeln("}");
            }
        }
    }

    // ------------------------------------------------------------------
    // Expression-level formatting (returns String)
    // ------------------------------------------------------------------

    pub(crate) fn format_expr(&self, node: &SNode) -> String {
        match &node.node {
            Node::StringLiteral(s) => {
                let escaped = escape_string(s);
                format!("\"{escaped}\"")
            }
            Node::InterpolatedString(segments) => {
                let mut result = String::from("\"");
                for seg in segments {
                    match seg {
                        StringSegment::Literal(s) => result.push_str(&escape_string(s)),
                        StringSegment::Expression(e) => {
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
                format!("{op}{expr}")
            }
            Node::TryOperator { operand } => {
                let expr = self.format_expr(operand);
                format!("{expr}?")
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
                let obj = self.format_expr(object);
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
                let obj = self.format_expr(object);
                let args_str = self.format_call_args(args, obj.len() + method.len() + 3);
                if object.span.end_line > 0 && node.span.end_line > object.span.end_line {
                    let pad = "  ".repeat(self.indent + 1);
                    format!("{obj}\n{pad}?.{method}({args_str})")
                } else {
                    format!("{obj}?.{method}({args_str})")
                }
            }
            Node::PropertyAccess { object, property } => {
                let obj = self.format_expr(object);
                format!("{obj}.{property}")
            }
            Node::OptionalPropertyAccess { object, property } => {
                let obj = self.format_expr(object);
                format!("{obj}?.{property}")
            }
            Node::SubscriptAccess { object, index } => {
                let obj = self.format_expr(object);
                let idx = self.format_expr(index);
                format!("{obj}[{idx}]")
            }
            Node::SliceAccess { object, start, end } => {
                let obj = self.format_expr(object);
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
                let kw = if *inclusive { "thru" } else { "upto" };
                format!("{s} {kw} {e}")
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
            Node::AskExpr { fields } => {
                let items = self.format_dict_entry_list(fields, 5);
                format!("ask {{{items}}}")
            }
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
                    if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
                        let expr = self.format_expr(&arm.body[0]);
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern} -> {{ {expr} }}\n"));
                    } else {
                        result.push_str(&indent_str);
                        result.push_str(&format!("{pattern} -> {{\n"));
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
                count,
                variable,
                body,
            } => {
                let cnt = self.format_expr(count);
                let opening = if let Some(var) = variable {
                    format!("parallel({cnt}) {{ {var} ->")
                } else {
                    format!("parallel({cnt}) {{")
                };
                self.format_block_expr(&opening, body)
            }
            Node::ParallelMap {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.format_block_expr(&format!("parallel_map({lst}) {{ {variable} ->"), body)
            }
            Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                let lst = self.format_expr(list);
                self.format_block_expr(&format!("parallel_settle({lst}) {{ {variable} ->"), body)
            }
            // Declaration nodes rendered as placeholders in expr context
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
            Node::TypeDecl { name, type_expr } => {
                let te = format_type_expr(type_expr);
                format!("type {name} = {te}")
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
        }
    }

    // ------------------------------------------------------------------
    // Closure formatting
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // Dict / struct helpers
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // Expression-or-statement (hybrid context for closure / block bodies)
    // ------------------------------------------------------------------

    /// Format a node as either an expression or statement string.
    /// Only handles node types that need special multi-line treatment
    /// different from `format_expr`; everything else delegates there.
    fn format_expr_or_stmt(&self, node: &SNode, indent_level: usize) -> String {
        match &node.node {
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition);
                let inner = indent_level + 1;
                let close = "  ".repeat(indent_level);
                let mut result = format!("if {cond} {{\n");
                result.push_str(&self.format_body_string(then_body, inner));
                if let Some(eb) = else_body {
                    result.push_str(&close);
                    result.push_str("} else {\n");
                    result.push_str(&self.format_body_string(eb, inner));
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
                self.format_block_at(&format!("for {pat} in {iter_str} {{"), body, indent_level)
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition);
                self.format_block_at(&format!("while {cond} {{"), body, indent_level)
            }
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
                    indent_level,
                );
                self.format_block_at(&format!("{sig} {{"), body, indent_level)
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
            Node::TryCatch {
                body,
                error_var,
                error_type,
                catch_body,
                finally_body,
            } => {
                let inner = indent_level + 1;
                let close = "  ".repeat(indent_level);
                let mut result = String::from("try {\n");
                result.push_str(&self.format_body_string(body, inner));
                if !catch_body.is_empty() || error_var.is_some() {
                    let catch_param = format_catch_param(error_var, error_type);
                    result.push_str(&close);
                    result.push_str(&format!("}} catch{catch_param} {{\n"));
                    result.push_str(&self.format_body_string(catch_body, inner));
                }
                if let Some(fb) = finally_body {
                    result.push_str(&close);
                    result.push_str("} finally {\n");
                    result.push_str(&self.format_body_string(fb, inner));
                }
                result.push_str(&close);
                result.push('}');
                result
            }
            _ => self.format_expr(node),
        }
    }

    /// Helper: `opening\n  body\nclose}` at an explicit indent level.
    fn format_block_at(&self, opening: &str, body: &[SNode], indent_level: usize) -> String {
        let inner = self.format_body_string(body, indent_level + 1);
        let close = "  ".repeat(indent_level);
        format!("{opening}\n{inner}{close}}}")
    }

    // ------------------------------------------------------------------
    // Match / enum / struct field helpers
    // ------------------------------------------------------------------

    fn format_match_arm(&mut self, arm: &harn_parser::MatchArm) {
        let pattern = self.format_expr(&arm.pattern);
        if arm.body.len() == 1 && is_simple_expr(&arm.body[0]) {
            let expr = self.format_expr(&arm.body[0]);
            self.writeln(&format!("{pattern} -> {{ {expr} }}"));
        } else {
            self.writeln(&format!("{pattern} -> {{"));
            self.indent();
            self.format_body(&arm.body, arm.pattern.span.line);
            self.dedent();
            self.writeln("}");
        }
    }

    fn format_enum_variant(&mut self, v: &harn_parser::EnumVariant) {
        if v.fields.is_empty() {
            self.writeln(&v.name);
        } else {
            let prefix_len = self.indent * 2 + v.name.len() + 1;
            let fields = self.format_typed_params_wrapped(&v.fields, prefix_len);
            self.writeln(&format!("{}({fields})", v.name));
        }
    }

    fn format_struct_field(&mut self, f: &harn_parser::StructField) {
        let opt = if f.optional { "?" } else { "" };
        if let Some(te) = &f.type_expr {
            let type_str = format_type_expr(te);
            self.writeln(&format!("{}{opt}: {type_str}", f.name));
        } else {
            self.writeln(&format!("{}{opt}", f.name));
        }
    }
}
