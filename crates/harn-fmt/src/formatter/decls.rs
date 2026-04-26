use harn_parser::{Node, ParallelMode, SNode};

use crate::helpers::{
    escape_string, format_attribute, format_catch_param, format_pattern, format_type_ann,
    format_type_expr, format_type_params,
};

use super::Formatter;

impl Formatter<'_> {
    pub(super) fn format_node(&mut self, node: &SNode) {
        let node_line = node.span.line;
        match &node.node {
            Node::Pipeline {
                name,
                params,
                return_type,
                body,
                extends,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let ret = if let Some(rt) = return_type {
                    format!(" -> {}", format_type_expr(rt))
                } else {
                    String::new()
                };
                let ext = if let Some(base) = extends {
                    format!(" extends {base}")
                } else {
                    String::new()
                };
                let prefix_len = self.indent * 2 + pub_prefix.len() + 9 + name.len() + 1;
                let params_str = self.format_string_list_wrapped(params, prefix_len, self.indent);
                self.writeln(&format!(
                    "{pub_prefix}pipeline {name}({params_str}){ret}{ext} {{"
                ));
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
                let val = self.format_expr(value, self.indent);
                self.writeln(&format!("let {pat}{type_str} = {val}"));
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value, self.indent);
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
                let params_str = self.format_typed_params_wrapped(params, prefix_len, self.indent);
                self.writeln(&format!("{pub_prefix}tool {name}({params_str}){ret} {{"));
                self.indent();
                if let Some(desc) = description {
                    let escaped = escape_string(desc);
                    self.writeln(&format!("description \"{escaped}\""));
                }
                self.format_body(body, node_line);
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
                    let expr_str = self.format_expr(field_expr, self.indent);
                    self.writeln(&format!("{field_name} {expr_str}"));
                }
                self.dedent();
                self.writeln("}");
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition, self.indent);
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
                let iter_str = self.format_expr(iterable, self.indent);
                self.writeln(&format!("for {pat} in {iter_str} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition, self.indent);
                self.writeln(&format!("while {cond} {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::Retry { count, body } => {
                let cnt = self.format_expr(count, self.indent);
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
                    let v = self.format_expr(val, self.indent);
                    self.writeln(&format!("return {v}"));
                } else {
                    self.writeln("return");
                }
            }
            Node::ThrowStmt { value } => {
                let v = self.format_expr(value, self.indent);
                self.writeln(&format!("throw {v}"));
            }
            Node::BreakStmt => self.writeln("break"),
            Node::ContinueStmt => self.writeln("continue"),
            Node::ImportDecl { path } => {
                self.writeln(&format!("import \"{path}\""));
            }
            Node::SelectiveImport { names, path } => {
                let line = self.format_selective_import_names(names, path, self.indent);
                self.writeln(&line);
            }
            Node::MatchExpr { value, arms } => {
                let val = self.format_expr(value, self.indent);
                self.writeln(&format!("match {val} {{"));
                self.indent();
                for arm in arms {
                    self.format_match_arm(arm);
                }
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
                for v in variants {
                    self.format_enum_variant(v);
                }
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
                for f in fields {
                    self.format_struct_field(f);
                }
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
                for (assoc_name, assoc_type) in associated_types {
                    if let Some(assoc_type) = assoc_type {
                        self.writeln(&format!(
                            "type {assoc_name} = {}",
                            format_type_expr(assoc_type)
                        ));
                    } else {
                        self.writeln(&format!("type {assoc_name}"));
                    }
                }
                for m in methods {
                    let method_generics = format_type_params(&m.type_params);
                    let prefix_len = self.indent * 2 + 3 + m.name.len() + method_generics.len() + 1;
                    let params =
                        self.format_typed_params_wrapped(&m.params, prefix_len, self.indent);
                    if let Some(ret) = &m.return_type {
                        self.writeln(&format!(
                            "fn {}{}({}) -> {}",
                            m.name,
                            method_generics,
                            params,
                            format_type_expr(ret)
                        ));
                    } else {
                        self.writeln(&format!("fn {}{}({})", m.name, method_generics, params));
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
                mode,
                expr,
                variable,
                body,
                options,
            } => {
                let e = self.format_expr(expr, self.indent);
                let mode_word = match mode {
                    ParallelMode::Count => "",
                    ParallelMode::Each => "each ",
                    ParallelMode::Settle => "settle ",
                };
                let options_clause = if options.is_empty() {
                    String::new()
                } else {
                    let formatted: Vec<String> = options
                        .iter()
                        .map(|(key, value)| {
                            format!("{key}: {}", self.format_expr(value, self.indent))
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
                let cond = self.format_expr(condition, self.indent);
                self.writeln(&format!("guard {cond} else {{"));
                self.indent();
                self.format_body(else_body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::RequireStmt { condition, message } => {
                let cond = self.format_expr(condition, self.indent);
                if let Some(message) = message {
                    let msg = self.format_expr(message, self.indent);
                    self.writeln(&format!("require {cond}, {msg}"));
                } else {
                    self.writeln(&format!("require {cond}"));
                }
            }
            Node::DeadlineBlock { duration, body } => {
                let dur = self.format_expr(duration, self.indent);
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
                    let v = self.format_expr(val, self.indent);
                    self.writeln(&format!("yield {v}"));
                } else {
                    self.writeln("yield");
                }
            }
            Node::OverrideDecl { name, params, body } => {
                let prefix_len = self.indent * 2 + 9 + name.len() + 1;
                let params_str = self.format_string_list_wrapped(params, prefix_len, self.indent);
                self.writeln(&format!("override {name}({params_str}) {{"));
                self.indent();
                self.format_body(body, node_line);
                self.dedent();
                self.writeln("}");
            }
            Node::TypeDecl {
                name,
                type_params,
                type_expr,
            } => {
                let params = format_type_params(type_params);
                let te = format_type_expr(type_expr);
                self.writeln(&format!("type {name}{params} = {te}"));
            }
            Node::Block(stmts) => {
                self.writeln("{");
                self.indent();
                self.format_body(stmts, node_line);
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
                let expr = self.format_expr(node, self.indent);
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
            let cond = self.format_expr(condition, self.indent);
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

    fn format_match_arm(&mut self, arm: &harn_parser::MatchArm) {
        let pattern = self.format_expr(&arm.pattern, self.indent);
        if arm.body.len() == 1 && crate::helpers::is_simple_expr(&arm.body[0]) {
            let expr = self.format_expr(&arm.body[0], self.indent);
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
            let fields = self.format_typed_params_wrapped(&v.fields, prefix_len, self.indent);
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
