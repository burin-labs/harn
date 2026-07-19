mod comments;
mod decls;
mod expressions;
mod statements;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};

use harn_lexer::Span;
use harn_parser::{Node, SNode, TypedParam};

use crate::helpers::*;

/// Width of the ` {` that every `fn` signature call site appends before the
/// body. The signature's own wrap decision has to budget for it.
const SIGNATURE_BODY_BRACE_WIDTH: usize = 2;

/// A captured comment with metadata.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    pub(crate) text: String,
    pub(crate) is_block: bool,
    pub(crate) is_doc: bool,
}

/// One item of a comma sequence (list element, dict/struct entry) together
/// with the comments anchored to it in the source.
pub(super) struct CommentedItem {
    pub(super) leading: Vec<String>,
    pub(super) body: String,
    pub(super) trailing: Option<String>,
}

pub(crate) struct Formatter<'a> {
    pub(crate) source: &'a str,
    pub(crate) output: String,
    pub(crate) indent: usize,
    pub(crate) line_width: usize,
    pub(crate) separator_width: usize,
    /// Line → comments on that line.
    pub(crate) comments: BTreeMap<usize, Vec<Comment>>,
    /// Track which comment lines have been emitted. Wrapped in `RefCell` so
    /// inline (trailing-comment) emission can happen from `&self` string-
    /// building paths (`format_body_string`, `format_expr`) without forcing
    /// the entire expression-formatter chain to be `&mut self`.
    pub(crate) emitted_lines: RefCell<HashSet<usize>>,
}

impl<'a> Formatter<'a> {
    pub(crate) fn new(
        source: &'a str,
        comments: BTreeMap<usize, Vec<Comment>>,
        line_width: usize,
        separator_width: usize,
    ) -> Self {
        Self {
            source,
            output: String::new(),
            indent: 0,
            line_width,
            separator_width,
            comments,
            emitted_lines: RefCell::new(HashSet::new()),
        }
    }

    pub(crate) fn finish(mut self) -> String {
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

    pub(crate) fn source_slice(&self, node: &SNode) -> &str {
        &self.source[node.span.start..node.span.end]
    }

    /// Inner lines of a block — does NOT include opening/closing braces.
    /// Trailing same-line comments on each statement are preserved inline, and
    /// standalone comments between statements are claimed here.
    ///
    /// `block_from_line` is the line the block opens on, so comments leading
    /// the first statement can be distinguished from comments belonging to
    /// whatever precedes the block. `block_end_line`, when available, flushes
    /// comments after the last statement before the block's closing brace.
    pub(super) fn format_body_string(
        &self,
        body: &[SNode],
        indent_level: usize,
        block_from_line: usize,
    ) -> String {
        self.format_body_string_bounded(body, indent_level, block_from_line, None)
    }

    pub(super) fn format_body_string_bounded(
        &self,
        body: &[SNode],
        indent_level: usize,
        block_from_line: usize,
        block_end_line: Option<usize>,
    ) -> String {
        let mut out = String::new();
        let indent_str = "  ".repeat(indent_level);
        for (i, n) in body.iter().enumerate() {
            // A synthesized node (`Spanned::dummy`, e.g. the `description(...)`
            // a tool decl splices in) has a zero span: it sits at no source
            // line, so it anchors no comments and cannot bound a range. Walk
            // back to the nearest node that really came from the source, or
            // fall back to the block itself — never to line 0, which would
            // claim every unclaimed comment in the file above this point.
            if n.span.line != 0 {
                let range_start = body[..i]
                    .iter()
                    .rev()
                    .find(|p| p.span.end_line != 0)
                    .map(|p| p.span.end_line + 1)
                    .unwrap_or(block_from_line + 1);
                out.push_str(&self.render_comments_in_range(
                    range_start,
                    n.span.line,
                    indent_level,
                ));
            }
            let expr = self.format_expr_or_stmt(n, indent_level);
            out.push_str(&indent_str);
            out.push_str(&expr);
            if let Some(trail) = self.take_trailing_comment_for_line(n.span.end_line) {
                out.push_str("  ");
                out.push_str(&trail);
            }
            out.push('\n');
        }
        if let Some(end_line) = block_end_line {
            let range_start = body
                .iter()
                .rev()
                .find(|n| n.span.end_line != 0)
                .map(|n| n.span.end_line + 1)
                .unwrap_or(block_from_line + 1);
            out.push_str(&self.render_comments_in_range(range_start, end_line, indent_level));
        }
        out
    }

    pub(super) fn format_block_expr(
        &self,
        opening: &str,
        body: &[SNode],
        indent: usize,
        block_from_line: usize,
    ) -> String {
        let inner = self.format_body_string(body, indent + 1, block_from_line);
        let close = "  ".repeat(indent);
        format!("{opening}\n{inner}{close}}}")
    }

    /// Whether `body` has a standalone comment above its first statement that
    /// nothing has claimed. A construct with a compact single-line form
    /// (`{ x -> x + 1 }`) has nowhere to put one, so it must fall back to the
    /// block form rather than render a layout the comment cannot live in.
    pub(super) fn body_carries_comment(&self, body: &[SNode], block_from_line: usize) -> bool {
        body.first().is_some_and(|first| {
            first.span.line != 0
                && self.has_unclaimed_comments_in_range(block_from_line + 1, first.span.line)
        })
    }

    /// The line a SECOND-or-later block of the same construct (`else`, `catch`,
    /// `finally`) can start claiming comments from: everything up to the end of
    /// the preceding block already belongs to it. Falls back to `default_line`
    /// when the preceding block is empty or ends in a synthesized (zero-span)
    /// node that names no source line.
    pub(super) fn block_from_line_after(preceding: &[SNode], default_line: usize) -> usize {
        preceding
            .iter()
            .rev()
            .find(|n| n.span.end_line != 0)
            .map(|n| n.span.end_line)
            .unwrap_or(default_line)
    }

    /// Render a comma-separated sequence at logical depth `indent`. When the
    /// sequence wraps, items are placed at `indent + 1` and the closing
    /// delimiter aligns with `indent`. Pre-rendered items are expected to have
    /// been formatted at depth `indent + 1` so that any of their internal
    /// wraps land at the correct deeper indent.
    pub(super) fn format_comma_sequence(
        &self,
        rendered: Vec<String>,
        prefix_len: usize,
        indent: usize,
    ) -> String {
        let inline = rendered.join(", ");
        let should_wrap = !rendered.is_empty()
            && (inline.contains('\n') || prefix_len + inline.len() + 1 > self.line_width);
        if !should_wrap {
            return inline;
        }
        let item_indent = "  ".repeat(indent + 1);
        let close_indent = "  ".repeat(indent);
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

    /// Render a comma sequence whose items may carry source comments:
    /// full-line comments that sat above the item inside the literal and a
    /// trailing same-line comment. Any present comment forces the sequence
    /// multiline so the comments are emitted in place — previously they
    /// were left unclaimed and flushed to the end of the file.
    pub(super) fn format_comma_sequence_commented(
        &self,
        items: Vec<CommentedItem>,
        prefix_len: usize,
        indent: usize,
    ) -> String {
        let has_comments = items
            .iter()
            .any(|item| !item.leading.is_empty() || item.trailing.is_some());
        if !has_comments {
            let rendered = items.into_iter().map(|item| item.body).collect();
            return self.format_comma_sequence(rendered, prefix_len, indent);
        }
        let item_indent = "  ".repeat(indent + 1);
        let close_indent = "  ".repeat(indent);
        let mut out = String::new();
        out.push('\n');
        for item in items {
            for comment in &item.leading {
                out.push_str(&item_indent);
                out.push_str(comment);
                out.push('\n');
            }
            out.push_str(&item_indent);
            out.push_str(&item.body);
            out.push(',');
            if let Some(trail) = item.trailing {
                out.push_str("  ");
                out.push_str(&trail);
            }
            out.push('\n');
        }
        out.push_str(&close_indent);
        out
    }

    /// Claim the comments that belong to one sequence item: full-line
    /// comments on the lines `[from_line, item_line)` (inside the literal,
    /// above the item) and the trailing comment on `item_end_line`. The
    /// trailing comment is left alone when the item ends on the literal's
    /// closing line — that comment belongs to the enclosing statement and
    /// is attached by `attach_trailing_comment`/`format_body_string`.
    pub(super) fn commented_item(
        &self,
        body: String,
        from_line: usize,
        item_line: usize,
        item_end_line: usize,
        literal_end_line: usize,
    ) -> CommentedItem {
        let mut leading = Vec::new();
        for line in from_line..item_line {
            if let Some(comment) = self.take_trailing_comment_for_line(line) {
                leading.push(comment);
            }
        }
        let trailing = (item_end_line < literal_end_line)
            .then(|| self.take_trailing_comment_for_line(item_end_line))
            .flatten();
        CommentedItem {
            leading,
            body,
            trailing,
        }
    }

    pub(super) fn format_typed_params_wrapped(
        &self,
        params: &[TypedParam],
        prefix_len: usize,
        indent: usize,
    ) -> String {
        self.format_comma_sequence(render_typed_params(params), prefix_len, indent)
    }

    pub(super) fn format_string_list_wrapped(
        &self,
        items: &[String],
        prefix_len: usize,
        indent: usize,
    ) -> String {
        self.format_comma_sequence(items.to_vec(), prefix_len, indent)
    }

    /// Whether a chained call moves onto its own line.
    ///
    /// Three independent reasons, and no others. In particular NOT "the author
    /// happened to break this call across source lines" — layout is decided by
    /// the formatted result, not by incidental newlines in the input.
    ///
    /// - the formatted receiver is itself multi-line, so there is no single
    ///   physical line left to tail;
    /// - a comment sits between the segments, and only a wrapped chain has a
    ///   line to host it;
    /// - the call head (`obj` + `.method(`) already overflows `line_width`, so
    ///   tailing it cannot fit whatever the arguments do.
    ///
    /// `head_len` is the width of `.method(` (or `?.method(`).
    ///
    /// The overflow test is a lower bound, not the true column: `last_line_width`
    /// measures the expression fragment, and the formatter does not track what
    /// precedes it on the line (`let x = `, `return `, …). So a head can still
    /// overflow undetected. Widening that is a formatter-wide change — it needs
    /// the current column threaded through `format_expr` — and is tracked
    /// separately; counting the indent is strictly closer than counting nothing.
    pub(super) fn chain_wraps(
        &self,
        obj: &str,
        lead: &str,
        head_len: usize,
        indent: usize,
    ) -> bool {
        obj.contains('\n')
            || !lead.is_empty()
            || indent * 2 + last_line_width(obj) + head_len > self.line_width
    }

    /// Column the arguments of a chained call start at.
    ///
    /// A wrapped chain renders `.method(` at `indent + 1`; an unwrapped one
    /// tails the receiver's last physical line.
    pub(super) fn chain_prefix_len(
        &self,
        obj: &str,
        head_len: usize,
        indent: usize,
        wraps: bool,
    ) -> usize {
        if wraps {
            (indent + 1) * 2 + head_len
        } else {
            indent * 2 + last_line_width(obj) + head_len
        }
    }

    /// Depth at which a method call's arguments wrap.
    ///
    /// A chain that wrapped renders `.method(` at `indent + 1`, so its arguments
    /// belong at that depth: an argument must never sit level with — or outdent
    /// from — the call that owns it.
    pub(super) fn chain_args_indent(&self, indent: usize, wraps: bool) -> usize {
        if wraps {
            indent + 1
        } else {
            indent
        }
    }

    pub(super) fn format_call_args(
        &self,
        args: &[SNode],
        prefix_len: usize,
        indent: usize,
    ) -> String {
        // Each arg may itself wrap; if it does, it will land at `indent + 1`
        // so we render children at that depth so their internal wraps are
        // aligned correctly.
        let rendered = args
            .iter()
            .map(|arg| self.format_expr(arg, indent + 1))
            .collect::<Vec<_>>();
        self.format_comma_sequence(rendered, prefix_len, indent)
    }

    /// Format selective import names, wrapping when they exceed `line_width`.
    pub(super) fn format_selective_import_names(
        &self,
        names: &[String],
        path: &str,
        indent: usize,
    ) -> String {
        let mut sorted_names = names.to_vec();
        sorted_names.sort();
        let inline = sorted_names.join(", ");
        let prefix_len = indent * 2 + 9; // "import { "
        let total = prefix_len + inline.len() + " } ".len() + 6 + path.len() + 1;
        if total > self.line_width {
            let item_indent = "  ".repeat(indent + 1);
            let close_indent = "  ".repeat(indent);
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
            Node::ImportDecl { path, .. } => (
                u8::from(!path.starts_with("std/")),
                path.clone(),
                0,
                String::new(),
            ),
            Node::SelectiveImport { names, path, .. } => {
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
            self.attach_trailing_comment(node.span.end_line);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn format_fn_signature(
        &self,
        pub_prefix: &str,
        name: &str,
        type_params: &[harn_parser::TypeParam],
        params: &[TypedParam],
        return_type: &Option<harn_parser::TypeExpr>,
        throws: &Option<harn_parser::TypeExpr>,
        where_clauses: &[harn_parser::WhereClause],
        indent_level: usize,
    ) -> String {
        let generics = format_type_params(type_params);
        let ret = if let Some(rt) = return_type {
            format!(" -> {}", format_type_expr(rt))
        } else {
            String::new()
        };
        let throws_str = format_throws_clause(throws);
        let where_str = format_where_clauses(where_clauses);
        // The wrap decision must budget for the WHOLE line, not just what
        // precedes the params: the return type, throws/where clauses and the
        // ` {` every caller appends all consume width. Counting only the prefix
        // is what let signatures land past `line_width`.
        //
        // The closing `)` is NOT counted here — `format_comma_sequence` already
        // adds it. Counting it twice wraps a signature that fits exactly.
        let suffix_len =
            ret.len() + throws_str.len() + where_str.len() + SIGNATURE_BODY_BRACE_WIDTH;
        let prefix_len =
            indent_level * 2 + pub_prefix.len() + 3 + name.len() + generics.len() + 1 + suffix_len;
        let params_str = self.format_typed_params_wrapped(params, prefix_len, indent_level);
        format!("{pub_prefix}fn {name}{generics}({params_str}){ret}{throws_str}{where_str}")
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
            // Preserve a trailing same-line comment on this top-level item.
            // Without this, `let x = 1 // note` at the top level silently
            // drops the comment — block bodies handle it via `format_body`,
            // but `format_program` previously did not.
            self.attach_trailing_comment(node.span.end_line);
        }
        if !self.comments.is_empty() {
            let max_line = *self.comments.keys().max().unwrap_or(&0);
            let last_line = nodes.last().map(|n| n.span.line + 1).unwrap_or(1);
            self.emit_top_level_comments_in_range(last_line, max_line + 1);
        }
    }

    /// `block_end_line` is the line of the block's closing brace, when the AST
    /// can identify it. A body with a sibling after it must use its own block
    /// extent rather than the enclosing node's end, or the sibling's comments
    /// can be claimed by the preceding block.
    pub(crate) fn format_body(
        &mut self,
        nodes: &[SNode],
        block_start_line: usize,
        block_end_line: Option<usize>,
    ) {
        self.format_members(
            nodes,
            |n| n.span,
            block_start_line,
            block_end_line,
            |f, n| f.format_node(n),
        );
    }

    /// Render the members of a block, keeping every comment in the block it was
    /// written in.
    ///
    /// The one mechanism behind every `{ ... }` the formatter emits —
    /// statements, struct fields, enum variants, interface methods, match arms.
    /// A comment can only be placed next to something that knows its own source
    /// lines, so this asks each member for nothing but its `Span`, and the rule
    /// is uniform: a member claims the comments written above it, bounded below
    /// by the previous member so claiming stays ordered and idempotent, then
    /// takes any comment trailing on its own last line.
    ///
    /// Members that carry no span cannot participate — which is exactly how a
    /// struct field's doc comment used to end up describing the next
    /// declaration.
    pub(crate) fn format_members<T>(
        &mut self,
        members: &[T],
        span_of: impl Fn(&T) -> Span,
        block_start_line: usize,
        block_end_line: Option<usize>,
        mut render: impl FnMut(&mut Self, &T),
    ) {
        for (i, member) in members.iter().enumerate() {
            let span = span_of(member);
            let range_start = if i > 0 {
                span_of(&members[i - 1]).end_line + 1
            } else {
                block_start_line + 1
            };
            self.emit_comments_in_range(range_start, span.line);
            render(self, member);
            self.attach_trailing_comment(span.end_line);
        }
        // Nothing claims a comment written after the last member, so flush the
        // tail before the caller writes `}`. Without this the comment stays
        // unclaimed and `format_program`'s leading-comment sweep adopts it onto
        // the NEXT top-level declaration, where it silently describes code it
        // was never written about.
        if let Some(end_line) = block_end_line {
            // A zero span names no source line, so it can neither anchor a
            // comment nor bound a range; skip synthesized members when looking
            // for where the tail begins.
            let range_start = members
                .iter()
                .rev()
                .map(span_of)
                .find(|s| s.end_line != 0)
                .map(|s| s.end_line + 1)
                .unwrap_or(block_start_line + 1);
            self.emit_comments_in_range(range_start, end_line);
        }
    }

    /// Splice a trailing comment (if any) on `line` into the most recent line
    /// already written to `self.output`. Caller is responsible for having just
    /// written that line via `format_node`/`writeln`. If no trailing comment
    /// exists, this is a no-op.
    pub(crate) fn attach_trailing_comment(&mut self, line: usize) {
        let Some(comment) = self.take_trailing_comment_for_line(line) else {
            return;
        };
        // We expect `self.output` to currently end in "...content\n". Splice
        // the comment in before that final newline so it lands on the same line
        // as the rendered statement.
        if self.output.ends_with('\n') {
            self.output.pop();
            self.output.push_str("  ");
            self.output.push_str(&comment);
            self.output.push('\n');
        } else {
            self.output.push_str("  ");
            self.output.push_str(&comment);
        }
    }
}
