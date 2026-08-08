use harn_lexer::{Span, Token, TokenKind};
use harn_parser::{DiagnosticCode as Code, TypeExpr, TypedParam};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};

impl Linter<'_> {
    /// Narrow a declaration span to its keyword and name for focused lint
    /// rendering. Synthetic nodes fall back to their original span.
    pub(super) fn name_anchored_span(&self, name: &str, span: Span) -> Span {
        let Some(source) = self.source else {
            return span;
        };
        if name.is_empty() || span.start >= span.end {
            return span;
        }
        let scan_end = source[span.start..span.end]
            .find('\n')
            .map_or(span.end, |newline| span.start + newline);
        let Some(haystack) = source.get(span.start..scan_end) else {
            return span;
        };
        let mut search_from = 0;
        while let Some(relative) = haystack[search_from..].find(name) {
            let match_start = search_from + relative;
            let match_end = match_start + name.len();
            let prev_ok = match_start == 0
                || !haystack[..match_start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_alphanumeric() || character == '_');
            let next_ok = !haystack[match_end..]
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
            if prev_ok && next_ok {
                return Span::with_offsets(
                    span.start,
                    span.start + match_end,
                    span.line,
                    span.column,
                );
            }
            search_from = match_start + 1;
        }
        span
    }

    fn callable_name_token_span(&mut self, name: &str, span: Span) -> Span {
        self.callable_signature_tokens(span)
            .into_iter()
            .find_map(|token| match token.kind {
                TokenKind::Identifier(identifier) if identifier == name => Some(token.span),
                _ => None,
            })
            .unwrap_or(span)
    }

    fn callable_parameter_token_spans(&mut self, params: &[TypedParam], span: Span) -> Vec<Span> {
        let mut spans = Vec::with_capacity(params.len());
        let mut in_parameters = false;
        let mut expect_parameter = false;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0usize;
        let mut in_default = false;

        for token in self.callable_signature_tokens(span) {
            match token.kind {
                TokenKind::LParen if !in_parameters => {
                    in_parameters = true;
                    expect_parameter = true;
                }
                TokenKind::LParen if in_parameters => paren_depth += 1,
                TokenKind::RParen if in_parameters && paren_depth > 0 => paren_depth -= 1,
                TokenKind::RParen if in_parameters => break,
                TokenKind::LBracket if in_parameters => bracket_depth += 1,
                TokenKind::RBracket if in_parameters => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
                TokenKind::LBrace if in_parameters => brace_depth += 1,
                TokenKind::RBrace if in_parameters => brace_depth = brace_depth.saturating_sub(1),
                TokenKind::Lt if in_parameters && !in_default => angle_depth += 1,
                TokenKind::Gt if in_parameters && !in_default => {
                    angle_depth = angle_depth.saturating_sub(1);
                }
                TokenKind::Assign
                    if in_parameters
                        && paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0 =>
                {
                    angle_depth = 0;
                    in_default = true;
                }
                TokenKind::Comma
                    if in_parameters
                        && paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0
                        && angle_depth == 0 =>
                {
                    expect_parameter = true;
                    in_default = false;
                }
                TokenKind::Identifier(_) if in_parameters && expect_parameter => {
                    spans.push(token.span);
                    expect_parameter = false;
                    if spans.len() == params.len() {
                        break;
                    }
                }
                _ => {}
            }
        }
        spans.resize(params.len(), span);
        spans
    }

    fn callable_signature_tokens(&mut self, span: Span) -> Vec<Token> {
        let tokens = self.cached_source_tokens.get_or_insert_with(|| {
            let Some(source) = self.source else {
                return Vec::new();
            };
            harn_lexer::Lexer::new(source)
                .tokenize()
                .unwrap_or_default()
        });
        tokens
            .iter()
            .filter(|token| token.span.start >= span.start && token.span.start < span.end)
            .cloned()
            .collect()
    }

    pub(super) fn check_public_api_types(
        &mut self,
        kind: &str,
        name: &str,
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        span: Span,
        existing_return_owner: bool,
    ) {
        if !self.require_public_api_types {
            return;
        }
        let parameter_spans = self.callable_parameter_token_spans(params, span);
        for (param, param_span) in params.iter().zip(parameter_spans) {
            if param.type_expr.is_some() {
                continue;
            }
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintMissingPublicApiType,
                rule: "missing-public-api-type".into(),
                message: format!(
                    "public {kind} `{name}` parameter `{}` is missing an explicit type",
                    param.name
                ),
                span: param_span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!(
                    "annotate the parameter explicitly: `{}: Type`",
                    param.name
                )),
                fix: None,
            });
        }
        if return_type.is_none() && !existing_return_owner {
            let name_span = self.callable_name_token_span(name, span);
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintMissingPublicApiType,
                rule: "missing-public-api-type".into(),
                message: format!("public {kind} `{name}` is missing an explicit return type"),
                span: name_span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!(
                    "declare a return type: `pub {kind} {name}(...) -> Type {{ ... }}`"
                )),
                fix: None,
            });
        }
    }
}
