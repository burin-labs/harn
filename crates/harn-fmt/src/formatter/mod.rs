mod comments;
mod decls;
mod expressions;
mod statements;

use std::collections::{BTreeMap, HashSet};

use harn_parser::{Node, SNode, TypedParam};

use crate::helpers::*;

/// A captured comment with metadata.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    pub(crate) text: String,
    pub(crate) is_block: bool,
    pub(crate) is_doc: bool,
}

pub(crate) struct Formatter {
    pub(crate) output: String,
    pub(crate) indent: usize,
    pub(crate) line_width: usize,
    pub(crate) separator_width: usize,
    /// Line → comments on that line.
    pub(crate) comments: BTreeMap<usize, Vec<Comment>>,
    /// Track which comment lines have been emitted.
    pub(crate) emitted_lines: HashSet<usize>,
}

impl Formatter {
    pub(crate) fn new(
        comments: BTreeMap<usize, Vec<Comment>>,
        line_width: usize,
        separator_width: usize,
    ) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            line_width,
            separator_width,
            comments,
            emitted_lines: HashSet::new(),
        }
    }

    pub(crate) fn finish(mut self) -> String {
        let trimmed: Vec<&str> = self.output.lines().map(|l| l.trim_end()).collect();
        self.output = trimmed.join("\n");
        if !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
    }

    pub(crate) fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }

    pub(crate) fn indent(&mut self) {
        self.indent += 1;
    }

    pub(crate) fn dedent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    pub(crate) fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.output.push_str(s);
        self.output.push('\n');
    }

    /// Inner lines of a block — does NOT include opening/closing braces.
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

    /// Format selective import names, wrapping when they exceed `line_width`.
    fn format_selective_import_names(&self, names: &[String], path: &str) -> String {
        let mut sorted_names = names.to_vec();
        sorted_names.sort();
        let inline = sorted_names.join(", ");
        let prefix_len = self.indent * 2 + 9; // "import { "
        let total = prefix_len + inline.len() + " } ".len() + 6 + path.len() + 1;
        if total > self.line_width {
            let item_indent = "  ".repeat(self.indent + 1);
            let close_indent = "  ".repeat(self.indent);
            let mut inner = String::from("\n");
            for name in &sorted_names {
                inner.push_str(&item_indent);
                inner.push_str(name);
                inner.push_str(",\n");
            }
            inner.push_str(&close_indent);
            format!("import {{{inner}}} from \"{path}\"")
        } else {
            format!("import {{ {inline} }} from \"{path}\"")
        }
    }

    fn is_import_node(node: &SNode) -> bool {
        matches!(
            node.node,
            Node::ImportDecl { .. } | Node::SelectiveImport { .. }
        )
    }

    fn import_sort_key(node: &SNode) -> (u8, String, u8, String) {
        match &node.node {
            Node::ImportDecl { path } => (
                u8::from(!path.starts_with("std/")),
                path.clone(),
                0,
                String::new(),
            ),
            Node::SelectiveImport { names, path } => {
                let mut sorted_names = names.clone();
                sorted_names.sort();
                (
                    u8::from(!path.starts_with("std/")),
                    path.clone(),
                    1,
                    sorted_names.join(","),
                )
            }
            _ => (2, String::new(), 2, String::new()),
        }
    }

    fn format_sorted_import_block(&mut self, nodes: &[SNode]) {
        let mut imports: Vec<(usize, &SNode)> = nodes
            .iter()
            .enumerate()
            .take_while(|(_, node)| Self::is_import_node(node))
            .collect();
        imports.sort_by(|(_, left), (_, right)| {
            Self::import_sort_key(left).cmp(&Self::import_sort_key(right))
        });

        for (original_index, node) in imports {
            let comment_from = if original_index == 0 {
                1
            } else {
                nodes[original_index - 1].span.line + 1
            };
            // Imports inside a sorted block stay tight — no blank line between them.
            self.emit_comments_in_range(comment_from, node.span.line);
            self.format_node(node);
        }
    }

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

    pub(crate) fn format_program(&mut self, nodes: &[SNode]) {
        let import_count = nodes
            .iter()
            .take_while(|node| Self::is_import_node(node))
            .count();
        if import_count > 0 {
            self.format_sorted_import_block(nodes);
        } else if let Some(first) = nodes.first() {
            self.emit_top_level_comments_in_range(1, first.span.line);
        }
        for (i, node) in nodes.iter().enumerate().skip(import_count) {
            if i > 0 {
                // Exactly one blank line between top-level items. Leading comments
                // (doc blocks, section headers) are emitted AFTER the blank line
                // so a doc comment stays glued to the item it documents.
                self.output.push('\n');
                let prev_end = if i == import_count && import_count > 0 {
                    nodes[import_count - 1].span.line + 1
                } else {
                    nodes[i - 1].span.line + 1
                };
                self.emit_top_level_comments_in_range(prev_end, node.span.line);
            }
            self.format_node(node);
        }
        if !self.comments.is_empty() {
            let max_line = *self.comments.keys().max().unwrap_or(&0);
            let last_line = nodes.last().map(|n| n.span.line + 1).unwrap_or(1);
            self.emit_top_level_comments_in_range(last_line, max_line + 1);
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
}
