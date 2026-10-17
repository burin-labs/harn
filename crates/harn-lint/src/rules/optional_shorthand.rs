//! `prefer-optional-shorthand` rule: type expressions of the form
//! `T | nil` (or `nil | T`) should be written as the postfix sugar
//! `T?`. The formatter rewrites this automatically; the lint rule
//! flags it independently so callers see the diagnostic without
//! running `harn fmt --fix` first.

use std::collections::HashSet;

use harn_lexer::{FixEdit, Span, TokenKind};
use harn_parser::{
    format_type, DiagnosticCode as Code, EnumVariant, InterfaceMethod, Node, SNode, ShapeField,
    StructField, TypeExpr, TypedParam,
};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

pub(crate) fn check_prefer_optional_shorthand(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut state = State {
        source,
        diagnostics,
        emitted: HashSet::new(),
        redacted: collect_redacted_ranges(source),
    };
    for node in program {
        state.visit_node(node);
    }
}

struct State<'a, 'd> {
    source: &'a str,
    diagnostics: &'d mut Vec<LintDiagnostic>,
    emitted: HashSet<usize>,
    /// Half-open `[start, end)` byte ranges covering comments, string
    /// literals, raw strings, and interpolated strings — substrings
    /// inside these are not real `T | nil` annotations and must be
    /// skipped to avoid false-positives like a comment that mentions
    /// the legacy syntax.
    redacted: Vec<(usize, usize)>,
}

impl<'a, 'd> State<'a, 'd> {
    fn visit_node(&mut self, node: &SNode) {
        let span = node.span;
        match &node.node {
            Node::Pipeline {
                params,
                return_type,
                body,
                ..
            }
            | Node::FnDecl {
                params,
                return_type,
                body,
                ..
            }
            | Node::ToolDecl {
                params,
                return_type,
                body,
                ..
            }
            | Node::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                self.visit_typed_params(params, span);
                if let Some(ty) = return_type {
                    self.visit_type(ty, span);
                }
                self.visit_block(body);
                return;
            }
            Node::LetBinding { type_ann, .. } | Node::ConstBinding { type_ann, .. } => {
                if let Some(ty) = type_ann {
                    self.visit_type(ty, span);
                }
            }
            Node::TypeDecl { type_expr, .. } => self.visit_type(type_expr, span),
            Node::StructDecl { fields, .. } => {
                for field in fields {
                    self.visit_struct_field(field, span);
                }
            }
            Node::EnumDecl { variants, .. } => {
                for variant in variants {
                    self.visit_enum_variant(variant, span);
                }
            }
            Node::InterfaceDecl {
                associated_types,
                methods,
                ..
            } => {
                for assoc in associated_types {
                    if let Some(ty) = &assoc.default {
                        self.visit_type(ty, span);
                    }
                }
                for method in methods {
                    self.visit_interface_method(method, span);
                }
                return;
            }
            Node::TryCatch {
                body,
                error_type,
                catch_body,
                finally_body,
                ..
            } => {
                // Preserve annotation order inside the encompassing span.
                self.visit_block(body);
                if let Some(ty) = error_type {
                    self.visit_type(ty, span);
                }
                self.visit_block(catch_body);
                if let Some(body) = finally_body {
                    self.visit_block(body);
                }
                return;
            }
            Node::FunctionCall { type_args, .. } => {
                for ty in type_args {
                    self.visit_type(ty, span);
                }
            }
            _ => {}
        }
        harn_parser::visit::for_each_immediate_child(node, &mut |child| self.visit_node(child));
    }

    fn visit_block(&mut self, body: &[SNode]) {
        for n in body {
            self.visit_node(n);
        }
    }

    fn visit_typed_params(&mut self, params: &[TypedParam], parent_span: Span) {
        for p in params {
            if let Some(ty) = &p.type_expr {
                self.visit_type(ty, parent_span);
            }
            if let Some(default) = &p.default_value {
                self.visit_node(default);
            }
        }
    }

    fn visit_struct_field(&mut self, field: &StructField, parent_span: Span) {
        if let Some(ty) = &field.type_expr {
            self.visit_type(ty, parent_span);
        }
    }

    fn visit_enum_variant(&mut self, variant: &EnumVariant, parent_span: Span) {
        for f in &variant.fields {
            if let Some(ty) = &f.type_expr {
                self.visit_type(ty, parent_span);
            }
        }
    }

    fn visit_interface_method(&mut self, method: &InterfaceMethod, parent_span: Span) {
        self.visit_typed_params(&method.params, parent_span);
        if let Some(ty) = &method.return_type {
            self.visit_type(ty, parent_span);
        }
    }

    /// Walk a TypeExpr tree, emitting a diagnostic for every union arm
    /// that is shorthand-eligible (`T | nil` → `T?`).
    fn visit_type(&mut self, ty: &TypeExpr, parent_span: Span) {
        match ty {
            TypeExpr::Union(members) => {
                if let Some(inner) = optional_inner(members) {
                    self.try_emit(inner, parent_span);
                }
                for m in members {
                    self.visit_type(m, parent_span);
                }
            }
            TypeExpr::Intersection(members) => {
                for m in members {
                    self.visit_type(m, parent_span);
                }
            }
            TypeExpr::Tuple(items) => {
                for item in items {
                    self.visit_type(item, parent_span);
                }
            }
            TypeExpr::List(inner)
            | TypeExpr::Iter(inner)
            | TypeExpr::Generator(inner)
            | TypeExpr::Stream(inner) => self.visit_type(inner, parent_span),
            TypeExpr::DictType(k, v) => {
                self.visit_type(k, parent_span);
                self.visit_type(v, parent_span);
            }
            TypeExpr::Applied { args, .. } => {
                for a in args {
                    self.visit_type(a, parent_span);
                }
            }
            TypeExpr::Shape(fields) => {
                for f in fields {
                    self.visit_shape_field(f, parent_span);
                }
            }
            TypeExpr::OpenShape { fields, rests } => {
                for f in fields {
                    self.visit_shape_field(f, parent_span);
                }
                for r in rests {
                    self.visit_type(r, parent_span);
                }
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                for p in params {
                    self.visit_type(p, parent_span);
                }
                self.visit_type(return_type, parent_span);
            }
            TypeExpr::Named(_) | TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => {
            }
            TypeExpr::Owned(inner) => self.visit_type(inner, parent_span),
        }
    }

    fn visit_shape_field(&mut self, field: &ShapeField, parent_span: Span) {
        self.visit_type(&field.type_expr, parent_span);
    }

    /// Find the earliest match of either needle within `region` whose
    /// absolute start hasn't already been flagged. Returns absolute
    /// (start, end) source offsets.
    fn next_unemitted_match(
        &self,
        region: &str,
        region_offset: usize,
        needle_after: &str,
        needle_before: &str,
    ) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        let mut consider = |rel_start: usize, len: usize| {
            let abs_start = region_offset + rel_start;
            if self.emitted.contains(&abs_start) {
                return;
            }
            if self.in_redacted_range(abs_start) {
                return;
            }
            if best.map(|(s, _)| abs_start < s).unwrap_or(true) {
                best = Some((abs_start, abs_start + len));
            }
        };
        // Advance past the match's first char, not one byte: the needle can
        // start with a multibyte type name, and a byte-sized step would land
        // inside it and panic the next slice.
        #[expect(
            clippy::string_slice,
            reason = "search_at advances by find offsets plus a whole char, a char boundary"
        )]
        let mut scan = |needle: &str| {
            let step = needle.chars().next().map_or(1, char::len_utf8);
            let mut search_at = 0;
            while let Some(pos) = region[search_at..].find(needle) {
                consider(search_at + pos, needle.len());
                search_at += pos + step;
            }
        };
        scan(needle_after);
        scan(needle_before);
        best
    }

    /// Emit a `prefer-optional-shorthand` diagnostic for a single
    /// `T | nil` site if its literal source can be located within
    /// `parent_span`. Render `inner` via the canonical type formatter
    /// and look for `<inner> | nil` or `nil | <inner>` substrings.
    /// Falls back to a fix-less diagnostic when the source can't be
    /// matched (e.g., the source whitespace doesn't normalize).
    fn try_emit(&mut self, inner: &TypeExpr, parent_span: Span) {
        let inner_text = format_type(inner);
        let region = match self.source.get(parent_span.start..parent_span.end) {
            Some(r) => r,
            None => return,
        };
        // Multiple sites in the same parent (e.g. `fn f(x: T | nil) -> T | nil`)
        // share a parent span, so we must walk past already-emitted offsets to
        // find the next un-flagged occurrence rather than re-flagging the first.
        let needle_after = format!("{inner_text} | nil");
        let needle_before = format!("nil | {inner_text}");
        let Some((abs_start, abs_end)) =
            self.next_unemitted_match(region, parent_span.start, &needle_after, &needle_before)
        else {
            return;
        };
        if !self.emitted.insert(abs_start) {
            return;
        }
        let span = Span::with_offsets(
            abs_start,
            abs_end,
            line_for(self.source, abs_start),
            column_for(self.source, abs_start),
        );
        let replacement = format!("{inner_text}?");
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintPreferOptionalShorthand,
            rule: "prefer-optional-shorthand".into(),
            message: format!("prefer `{inner_text}?` over `{inner_text} | nil`"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rewrite the optional type to its postfix-`?` shorthand: `{inner_text}?`"
            )),
            fix: Some(vec![FixEdit { span, replacement }]),
        });
    }
}

impl<'a, 'd> State<'a, 'd> {
    fn in_redacted_range(&self, pos: usize) -> bool {
        self.redacted
            .iter()
            .any(|&(start, end)| pos >= start && pos < end)
    }
}

/// Collect half-open byte ranges covering tokens whose contents must
/// not be searched for `T | nil` substrings — comments and string
/// forms. Returns an empty list when the source fails to lex; the lint
/// downgrades to "no diagnostics" rather than crashing on partial input.
fn collect_redacted_ranges(source: &str) -> Vec<(usize, usize)> {
    let Some(tokens) = crate::source_tokens::with_comments(source) else {
        return Vec::new();
    };
    tokens
        .iter()
        .filter_map(|t| match t.kind {
            TokenKind::LineComment { .. }
            | TokenKind::BlockComment { .. }
            | TokenKind::StringLiteral(_)
            | TokenKind::InterpolatedString(_)
            | TokenKind::RawStringLiteral(_) => Some((t.span.start, t.span.end)),
            _ => None,
        })
        .collect()
}

fn optional_inner(types: &[TypeExpr]) -> Option<&TypeExpr> {
    if types.len() != 2 {
        return None;
    }
    let nil_idx = types
        .iter()
        .position(|t| matches!(t, TypeExpr::Named(n) if n == "nil"))?;
    let inner = &types[1 - nil_idx];
    if matches!(
        inner,
        TypeExpr::Union(_) | TypeExpr::Intersection(_) | TypeExpr::FnType { .. }
    ) {
        return None;
    }
    if matches!(inner, TypeExpr::Named(n) if n == "nil") {
        return None;
    }
    Some(inner)
}

#[expect(
    clippy::string_slice,
    reason = "offset is a find-derived match offset anchored at a token span, a char boundary"
)]
fn line_for(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[expect(
    clippy::string_slice,
    reason = "offset is a find-derived match offset anchored at a token span, and line_start \
              is rfind-derived, so both are char boundaries"
)]
fn column_for(source: &str, offset: usize) -> usize {
    // 1-based *character* column: byte arithmetic would overshoot on any
    // line containing multibyte characters before the offset.
    let upto = &source[..offset.min(source.len())];
    let line_start = upto.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    upto[line_start..].chars().count() + 1
}

#[cfg(test)]
mod position_tests {
    use super::column_for;

    #[test]
    fn column_counts_chars_not_bytes() {
        // 'é' is 2 bytes / 1 char: 'b' sits at byte 4 but char column 4.
        let first = "aé b";
        let off = first.find('b').unwrap();
        assert_eq!(column_for(first, off), 4);

        // Same shape on a later line: line-relative, still char-based.
        let multi = "x\naé b";
        let off = multi.rfind('b').unwrap();
        assert_eq!(column_for(multi, off), 4);
    }

    #[test]
    fn column_is_one_based_at_line_start() {
        assert_eq!(column_for("abc", 0), 1);
        assert_eq!(column_for("a\nbc", 2), 1);
    }
}
