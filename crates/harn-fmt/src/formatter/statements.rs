use harn_parser::{Node, SNode};

use crate::helpers::{
    escape_string, format_catch_param, format_pattern, format_type_ann, format_type_expr,
};

use super::Formatter;

impl Formatter<'_> {
    /// Hybrid context (closure / block bodies): only handles node types that
    /// need multi-line treatment different from `format_expr`.
    pub(super) fn format_expr_or_stmt(&self, node: &SNode, indent_level: usize) -> String {
        match &node.node {
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond = self.format_expr(condition, indent_level);
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
                let iter_str = self.format_expr(iterable, indent_level);
                self.format_block_expr(&format!("for {pat} in {iter_str} {{"), body, indent_level)
            }
            Node::WhileLoop { condition, body } => {
                let cond = self.format_expr(condition, indent_level);
                self.format_block_expr(&format!("while {cond} {{"), body, indent_level)
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
                self.format_block_expr(&format!("{sig} {{"), body, indent_level)
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
                let prefix_len = indent_level * 2 + pub_prefix.len() + 5 + name.len() + 1;
                let params_str = self.format_typed_params_wrapped(params, prefix_len, indent_level);
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
                    indent_level,
                )
            }
            Node::SkillDecl {
                name,
                fields,
                is_pub,
            } => {
                let pub_prefix = if *is_pub { "pub " } else { "" };
                let item_indent = "  ".repeat(indent_level + 1);
                let close_indent = "  ".repeat(indent_level);
                let mut inner = String::new();
                for (field_name, field_expr) in fields {
                    let expr_str = self.format_expr(field_expr, indent_level + 1);
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
                let val = self.format_expr(value, indent_level);
                format!("let {pat}{type_str} = {val}")
            }
            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                let pat = format_pattern(pattern);
                let type_str = format_type_ann(type_ann);
                let val = self.format_expr(value, indent_level);
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
            _ => self.format_expr(node, indent_level),
        }
    }
}
