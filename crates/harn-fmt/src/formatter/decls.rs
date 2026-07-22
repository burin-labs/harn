use harn_parser::{Node, ParallelMode, SNode, ShapeField, TypeExpr};

use crate::helpers::{
    column_after, escape_string, format_attribute, format_catch_param, format_pattern,
    format_throws_clause, format_type_ann_wrapped, format_type_expr, format_type_expr_wrapped,
    format_type_params, text_width,
};

use super::Formatter;

impl Formatter<'_> {
    pub(super) fn format_node(&mut self, node: &SNode) {
        let node_line = node.span.line;
        let node_end_line = node.span.end_line;
        match &node.node {
            Node::Pipeline {
                name,
                params,
                return_type,
                throws,
                body,
                extends,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let ret_inline = return_type
                    .as_ref()
                    .map(|rt| format!(" -> {}", format_type_expr(rt)))
                    .unwrap_or_default();
                let throws_str = format_throws_clause(throws);
                let ext = if let Some(base) = extends {
                    format!(" extends {base}")
                } else {
                    String::new()
                };
                let prefix_col =
                    self.indent * 2 + text_width(pub_prefix) + 9 + text_width(name) + 1;
                let suffix_len =
                    text_width(&ret_inline) + text_width(&throws_str) + text_width(&ext) + 2;
                let params_str =
                    self.format_typed_params_wrapped(params, prefix_col + suffix_len, self.indent);
                let ret = self.format_return_type(
                    return_type,
                    prefix_col,
                    &params_str,
                    self.indent,
                    text_width(&throws_str) + text_width(&ext) + 2,
                );
                self.writeln(&format!(
                    "{pub_prefix}pipeline {name}({params_str}){ret}{throws_str}{ext} {{"
                ));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::LetBinding {
                pattern,
                type_ann,
                value,
                is_pub,
            } => {
                let vis = if *is_pub { "pub " } else { "" };
                let pat = format_pattern(pattern);
                let type_col =
                    self.indent * 2 + text_width(vis) + text_width("let ") + text_width(&pat) + 2;
                let type_str =
                    format_type_ann_wrapped(type_ann, self.indent, type_col, self.line_width);
                let prefix = format!("{vis}let {pat}{type_str} = ");
                let val =
                    self.format_expr(value, self.indent, column_after(self.indent * 2, &prefix));
                let formatted =
                    self.format_prefixed_value(&prefix, &val, self.indent, self.indent * 2);
                self.writeln(&formatted);
            }
            Node::ConstBinding {
                pattern,
                type_ann,
                value,
                is_pub,
            } => {
                let vis = if *is_pub { "pub " } else { "" };
                let pat = format_pattern(pattern);
                let type_col =
                    self.indent * 2 + text_width(vis) + text_width("const ") + text_width(&pat) + 2;
                let type_str =
                    format_type_ann_wrapped(type_ann, self.indent, type_col, self.line_width);
                let prefix = format!("{vis}const {pat}{type_str} = ");
                let val =
                    self.format_expr(value, self.indent, column_after(self.indent * 2, &prefix));
                let formatted =
                    self.format_prefixed_value(&prefix, &val, self.indent, self.indent * 2);
                self.writeln(&formatted);
            }
            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                throws,
                where_clauses,
                body,
                is_pub,
                is_stream,
            } => {
                let pub_prefix = match (*is_pub, *is_stream) {
                    (true, true) => "pub gen ",
                    (true, false) => "pub ",
                    (false, true) => "gen ",
                    (false, false) => "",
                };
                let sig = self.format_fn_signature(
                    pub_prefix,
                    name,
                    type_params,
                    params,
                    return_type,
                    throws,
                    where_clauses,
                    self.indent * 2,
                    self.indent,
                );
                self.writeln(&format!("{sig} {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::ToolDecl {
                name,
                description,
                params,
                return_type,
                throws,
                body,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let ret_inline = return_type
                    .as_ref()
                    .map(|rt| format!(" -> {}", format_type_expr(rt)))
                    .unwrap_or_default();
                let throws_str = format_throws_clause(throws);
                let prefix_col =
                    self.indent * 2 + text_width(pub_prefix) + 5 + text_width(name) + 1;
                let suffix_len = text_width(&ret_inline) + text_width(&throws_str) + 2;
                let params_str =
                    self.format_typed_params_wrapped(params, prefix_col + suffix_len, self.indent);
                let ret = self.format_return_type(
                    return_type,
                    prefix_col,
                    &params_str,
                    self.indent,
                    text_width(&throws_str) + 2,
                );
                self.writeln(&format!(
                    "{pub_prefix}tool {name}({params_str}){ret}{throws_str} {{"
                ));
                self.indent();
                if let Some(desc) = description {
                    let escaped = escape_string(desc);
                    self.writeln(&format!("description \"{escaped}\""));
                }
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::SkillDecl {
                name,
                fields,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                self.writeln(&format!("{pub_prefix}skill {name} {{"));
                self.indent();
                for (field_name, field_expr) in fields {
                    let prefix_col = self.indent * 2 + text_width(field_name) + 1;
                    let expr_str = self.format_expr(field_expr, self.indent, prefix_col);
                    self.writeln(&format!("{field_name} {expr_str}"));
                }
                self.dedent();
                self.writeln("}");
            }
            Node::EvalPackDecl {
                binding_name,
                pack_id,
                fields,
                body,
                summarize,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                if binding_name == pack_id {
                    self.writeln(&format!("{pub_prefix}eval_pack {binding_name} {{"));
                } else {
                    let escaped = escape_string(pack_id);
                    self.writeln(&format!(
                        "{pub_prefix}eval_pack {binding_name} \"{escaped}\" {{"
                    ));
                }
                self.indent();
                for (field_name, field_expr) in fields {
                    let prefix_col = self.indent * 2 + text_width(field_name) + 2;
                    let expr_str = self.format_expr(field_expr, self.indent, prefix_col);
                    self.writeln(&format!("{field_name}: {expr_str}"));
                }
                self.format_body(
                    body,
                    node_line,
                    if summarize.is_some() {
                        None
                    } else {
                        Some(node_end_line)
                    },
                );
                if let Some(summary_body) = summarize {
                    self.writeln("summarize {");
                    self.indent();
                    self.format_body(summary_body, node_line, None);
                    self.dedent();
                    self.writeln("}");
                }
                self.dedent();
                self.writeln("}");
            }
            Node::IfElse {
                condition,
                then_body,
                then_span,
                else_body,
                else_span,
            } => {
                let cond = self.format_expr(condition, self.indent, self.indent * 2 + 5);
                self.writeln(&format!("if {cond} {{"));
                self.indent();
                self.format_body(then_body, then_span.line, Some(then_span.end_line));
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
                    let else_span = else_span.expect("braced else has a block span");
                    self.format_body(eb, else_span.line, Some(else_span.end_line));
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
                let iter_str = self.format_expr(
                    iterable,
                    self.indent,
                    self.indent * 2 + 4 + text_width(&pat) + 4,
                );
                self.writeln(&format!("for {pat} in {iter_str} {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition, self.indent, self.indent * 2 + 6);
                self.writeln(&format!("while {cond} {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count, self.indent, self.indent * 2 + 6);
                self.writeln(&format!("retry {cnt} {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::TryCatch {
                body,
                try_span,
                has_catch,
                error_var,
                error_type,
                catch_body,
                catch_span,
                finally_body,
                finally_span,
            } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, try_span.line, Some(try_span.end_line));
                self.dedent();
                if *has_catch {
                    let catch_param = format_catch_param(error_var, error_type);
                    self.writeln(&format!("}} catch{catch_param} {{"));
                    self.indent();
                    let catch_span = catch_span.expect("catch has a block span");
                    self.format_body(catch_body, catch_span.line, Some(catch_span.end_line));
                    self.dedent();
                }
                if let Some(fb) = finally_body {
                    self.writeln("} finally {");
                    self.indent();
                    let finally_span = finally_span.expect("finally has a block span");
                    self.format_body(fb, finally_span.line, Some(finally_span.end_line));
                    self.dedent();
                }
                self.writeln("}");
            }
            Node::TryExpr { body } => {
                self.writeln("try {");
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::ReturnStmt { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val, self.indent, self.indent * 2 + 7);
                    self.writeln(&format!("return {v}"));
                } else {
                    self.writeln("return");
                }
            }
            Node::ThrowStmt { value } => {
                let v = self.format_expr(value, self.indent, self.indent * 2 + 6);
                self.writeln(&format!("throw {v}"));
            }
            Node::BreakStmt => self.writeln("break"),
            Node::ContinueStmt => self.writeln("continue"),
            Node::ImportDecl { path, is_pub } => {
                let prefix = if *is_pub { "pub " } else { "" };
                self.writeln(&format!("{prefix}import \"{path}\""));
            }
            Node::SelectiveImport {
                names,
                path,
                is_pub,
            } => {
                let prefix = if *is_pub { "pub " } else { "" };
                let line = self.format_selective_import_names(
                    names,
                    path,
                    self.indent * 2 + text_width(prefix),
                    self.indent,
                );
                self.writeln(&format!("{prefix}{line}"));
            }
            Node::NamespaceImport {
                alias,
                path,
                is_pub,
            } => {
                let prefix = if *is_pub { "pub " } else { "" };
                self.writeln(&format!("{prefix}import * as {alias} from \"{path}\""));
            }
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value, self.indent, self.indent * 2 + 7);
                self.writeln(&format!("match {val} {{"));
                self.indent();
                self.format_members(
                    arms,
                    |a| a.span,
                    node_line,
                    Some(node_end_line),
                    |f, a| f.format_match_arm(a),
                );
                self.dedent();
                self.writeln("}");
            }
            Node::EnumDecl {
                name,
                type_params,
                variants,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let generics = format_type_params(type_params);
                self.writeln(&format!("{pub_prefix}enum {name}{generics} {{"));
                self.indent();
                self.format_members(
                    variants,
                    |v| v.span,
                    node_line,
                    Some(node_end_line),
                    |f, v| f.format_enum_variant(v),
                );
                self.dedent();
                self.writeln("}");
            }
            Node::StructDecl {
                name,
                type_params,
                fields,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let generics = format_type_params(type_params);
                self.writeln(&format!("{pub_prefix}struct {name}{generics} {{"));
                self.indent();
                self.format_members(
                    fields,
                    |f| f.span,
                    node_line,
                    Some(node_end_line),
                    |fmt, field| fmt.format_struct_field(field),
                );
                self.dedent();
                self.writeln("}");
            }
            Node::InterfaceDecl {
                name,
                type_params,
                associated_types,
                methods,
            } => {
                let generics = format_type_params(type_params);
                self.writeln(&format!("interface {name}{generics} {{"));
                self.indent();
                // The parser sorts an interface body into two lists, losing the
                // order the author wrote. Comment anchoring is bounded by the
                // PREVIOUS member, so members must be rendered in source order
                // or a comment binds to the wrong side; interleaving by span
                // both fixes that and preserves the author's layout.
                let mut members: Vec<InterfaceMember<'_>> = associated_types
                    .iter()
                    .map(InterfaceMember::Assoc)
                    .chain(methods.iter().map(InterfaceMember::Method))
                    .collect();
                members.sort_by_key(|m| m.span().start);
                self.format_members(
                    &members,
                    |m| m.span(),
                    node_line,
                    Some(node_end_line),
                    |f, m| match m {
                        InterfaceMember::Assoc(a) => f.format_associated_type(a),
                        InterfaceMember::Method(m) => f.format_interface_method(m),
                    },
                );
                self.dedent();
                self.writeln("}");
            }
            Node::ImplBlock { type_name, methods } => {
                self.writeln(&format!("impl {type_name} {{"));
                self.indent();
                self.format_body(methods, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::Parallel {
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                let mode_word = match mode {
                    ParallelMode::Count => "",
                    ParallelMode::Each | ParallelMode::EachStream => "each ",
                    ParallelMode::Settle => "settle ",
                };
                let stream_suffix = if matches!(mode, ParallelMode::EachStream) {
                    " as stream"
                } else {
                    ""
                };
                let keyword_len = 9 + text_width(mode_word);
                let e = self.format_expr(expr, self.indent, self.indent * 2 + keyword_len + 1);
                let options_clause = if options.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> = options
                        .iter()
                        .map(|(key, value)| {
                            format!(
                                "{key}: {}",
                                self.format_expr(
                                    value,
                                    self.indent,
                                    self.indent * 2 + keyword_len + 1
                                )
                            )
                        })
                        .collect();
                    format!(" with {{ {} }}", formatted.join(", "))
                };
                let header = if let Some(var) = variable {
                    format!("parallel {mode_word}{e}{options_clause} {{ {var} ->")
                } else {
                    format!("parallel {mode_word}{e}{options_clause} {{")
                };
                self.writeln(&header);
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln(&format!("}}{stream_suffix}"));
            }
            Node::SpawnExpr { body } => {
                self.writeln("spawn {");
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::ScopeBlock { body } => {
                self.writeln("scope {");
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::CostRoute { options, body } => {
                self.writeln("cost_route {");
                self.indent();
                for (key, value) in options {
                    let prefix_col = self.indent * 2 + text_width(key) + 2;
                    let value = self.format_expr(value, self.indent, prefix_col);
                    self.writeln(&format!("{key}: {value}"));
                }
                if !options.is_empty() && !body.is_empty() {
                    self.writeln("");
                }
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond = self.format_expr(condition, self.indent, self.indent * 2 + 6);
                self.writeln(&format!("guard {cond} else {{"));
                self.indent();
                self.format_body(else_body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::RequireStmt { condition, message } => {
                let formatted = self.format_require_stmt(
                    condition,
                    message.as_deref(),
                    self.indent,
                    self.indent * 2,
                );
                self.writeln(&formatted);
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration, self.indent, self.indent * 2 + 9);
                self.writeln(&format!("deadline {dur} {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::MutexBlock { key, body } => {
                match key {
                    Some(key_expr) => {
                        let k = self.format_expr(key_expr, self.indent, self.indent * 2 + 6);
                        self.writeln(&format!("mutex({k}) {{"));
                    }
                    None => self.writeln("mutex {"),
                }
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::YieldExpr { value } => {
                if let Some(val) = value {
                    let v = self.format_expr(val, self.indent, self.indent * 2 + 6);
                    self.writeln(&format!("yield {v}"));
                } else {
                    self.writeln("yield");
                }
            }
            Node::EmitExpr { value } => {
                let v = self.format_expr(value, self.indent, self.indent * 2 + 5);
                self.writeln(&format!("emit {v}"));
            }
            Node::OverrideDecl { name, params, body } => {
                let prefix_col = self.indent * 2 + 9 + text_width(name) + 1;
                let params_str = self.format_string_list_wrapped(params, prefix_col, self.indent);
                self.writeln(&format!("override {name}({params_str}) {{"));
                self.indent();
                self.format_body(body, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::TypeDecl {
                name,
                type_params,
                type_expr,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let params = format_type_params(type_params);
                let prefix = self.indent * 2
                    + text_width(pub_prefix)
                    + text_width("type ")
                    + text_width(name)
                    + text_width(&params)
                    + text_width(" = ");
                let has_field_comments = matches!(type_expr, TypeExpr::Shape(_))
                    && self.has_unclaimed_comments_in_range(node_line + 1, node_end_line);
                if has_field_comments {
                    let TypeExpr::Shape(fields) = type_expr else {
                        unreachable!("only shape aliases can carry shape-field comments")
                    };
                    self.writeln(&format!("{pub_prefix}type {name}{params} = {{"));
                    self.indent();
                    self.format_members(
                        fields,
                        |field| field.span,
                        node_line,
                        Some(node_end_line),
                        |formatter, field| formatter.format_shape_field(field),
                    );
                    self.dedent();
                    self.writeln("}");
                } else {
                    let te =
                        format_type_expr_wrapped(type_expr, self.indent, prefix, self.line_width);
                    self.writeln(&format!("{pub_prefix}type {name}{params} = {te}"));
                }
            }
            Node::Block(stmts) => {
                self.writeln("block {");
                self.indent();
                self.format_body(stmts, node_line, Some(node_end_line));
                self.dedent();
                self.writeln("}");
            }
            Node::AttributedDecl { attributes, inner } => {
                for attr in attributes {
                    self.writeln(&format_attribute(attr));
                }
                // A doc comment may sit between the last attribute and the
                // inner declaration (`@attr \n /** doc */ \n pub fn …`) — the
                // lint rule `missing-harndoc` requires the doc block to sit
                // directly above the fn, so we must preserve that position.
                if let Some(last_attr) = attributes.last() {
                    let from = last_attr.span.line + 1;
                    let to = inner.span.line;
                    if from < to {
                        self.emit_comments_in_range(from, to);
                    }
                }
                self.format_node(inner);
            }
            _ => {
                let expr = self.format_expr(node, self.indent, self.indent * 2);
                self.writeln(&expr);
            }
        }
    }

    /// Like format_node but without writing the leading indent (for else-if chains).
    fn format_node_no_indent(&mut self, node: &SNode) {
        if let Node::IfElse {
            condition,
            then_body,
            then_span,
            else_body,
            else_span,
        } = &node.node
        {
            let cond = self.format_expr(condition, self.indent, self.indent * 2 + 3);
            self.output.push_str(&format!("if {cond} {{\n"));
            self.indent();
            self.format_body(then_body, then_span.line, Some(then_span.end_line));
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
                let else_span = else_span.expect("braced else has a block span");
                self.format_body(eb, else_span.line, Some(else_span.end_line));
                self.dedent();
                self.writeln("}");
            } else {
                self.writeln("}");
            }
        }
    }

    fn format_match_arm(&mut self, arm: &harn_parser::MatchArm) {
        let pattern = self.format_expr(&arm.pattern, self.indent, self.indent * 2);
        let guard = if let Some(ref guard) = arm.guard {
            format!(
                " if {}",
                self.format_expr(
                    guard,
                    self.indent,
                    self.indent * 2 + text_width(&pattern) + 4
                )
            )
        } else {
            String::new()
        };
        // The compact form (`1 -> { x }`) has nowhere to put a comment written
        // on its own line inside the arm, so an arm carrying one must fall back
        // to the block form. Choosing the layout first and discovering the
        // comment afterwards is how it ends up with no home and escapes the
        // file. A comment trailing on the arm's OWN line is not interior — the
        // compact form keeps that one, so the range starts below it.
        let has_interior_comment =
            self.has_unclaimed_comments_in_range(arm.span.line + 1, arm.span.end_line + 1);
        if arm.body.len() == 1
            && crate::helpers::is_simple_expr(&arm.body[0])
            && !has_interior_comment
        {
            let expr = self.format_expr(
                &arm.body[0],
                self.indent,
                self.indent * 2 + text_width(&pattern) + text_width(&guard) + 4,
            );
            self.writeln(&format!("{pattern}{guard} -> {{ {expr} }}"));
        } else {
            self.writeln(&format!("{pattern}{guard} -> {{"));
            self.indent();
            // Bounded by the arm's own closing brace: without the arm's span
            // there was no end to flush a trailing interior comment against.
            self.format_body(&arm.body, arm.span.line, Some(arm.span.end_line));
            self.dedent();
            self.writeln("}");
        }
    }

    fn format_enum_variant(&mut self, v: &harn_parser::EnumVariant) {
        if v.fields.is_empty() {
            self.writeln(&v.name);
        } else {
            let prefix_col = self.indent * 2 + text_width(&v.name) + 1;
            let fields = self.format_typed_params_wrapped(&v.fields, prefix_col, self.indent);
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

    fn format_shape_field(&mut self, field: &ShapeField) {
        let optional = if field.optional { "?" } else { "" };
        let prefix_len = self.indent * 2 + field.name.len() + optional.len() + 2;
        let type_expr =
            format_type_expr_wrapped(&field.type_expr, self.indent, prefix_len, self.line_width);
        self.writeln(&format!("{}{optional}: {type_expr},", field.name));
    }

    fn format_associated_type(&mut self, a: &harn_parser::AssociatedType) {
        let rendered = match &a.default {
            Some(default) => format!("type {} = {}", a.name, format_type_expr(default)),
            None => format!("type {}", a.name),
        };
        self.writeln(&rendered);
    }

    fn format_interface_method(&mut self, m: &harn_parser::InterfaceMethod) {
        let method_generics = format_type_params(&m.type_params);
        let prefix_col =
            self.indent * 2 + 3 + text_width(&m.name) + text_width(&method_generics) + 1;
        let params = self.format_typed_params_wrapped(&m.params, prefix_col, self.indent);
        match &m.return_type {
            Some(ret) => self.writeln(&format!(
                "fn {}{}({}) -> {}",
                m.name,
                method_generics,
                params,
                format_type_expr(ret)
            )),
            None => self.writeln(&format!("fn {}{}({})", m.name, method_generics, params)),
        }
    }
}

/// A borrowed view over one entry of an interface body, so the two lists the
/// parser splits it into can be walked back in the order they were written.
enum InterfaceMember<'a> {
    Assoc(&'a harn_parser::AssociatedType),
    Method(&'a harn_parser::InterfaceMethod),
}

impl InterfaceMember<'_> {
    fn span(&self) -> harn_lexer::Span {
        match self {
            InterfaceMember::Assoc(a) => a.span,
            InterfaceMember::Method(m) => m.span,
        }
    }
}
