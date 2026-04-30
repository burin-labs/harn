//! `trailing-comma` rule: multiline comma-separated lists (argument
//! lists, list literals, dict/struct literals) must end with a trailing
//! comma on the last item, and single-line lists must not. Autofix inserts
//! or removes the comma at the canonical boundary.

use harn_lexer::{FixEdit, Span, TokenKind};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

/// Emit `trailing-comma` diagnostics by scanning the source's tokens for
/// comma-separated lists whose trailing comma does not match layout.
pub(crate) fn check_trailing_comma(source: &str, diagnostics: &mut Vec<LintDiagnostic>) {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return;
    };

    #[derive(Clone, Copy)]
    enum Opener {
        Paren,
        Bracket,
        Brace,
    }
    struct Frame {
        opener: Opener,
        open_line: usize,
        saw_item: bool,
        /// True when `{ ... }` has been identified as a dict/struct literal.
        /// Paren/Bracket are eligible when they are list-like delimiters.
        eligible: bool,
        /// For `{ ... }` we look at the first "meaningful" token to decide
        /// eligibility. This tracks whether that decision has been made.
        decision_made: bool,
        /// First identifier/string token inside `{ ... }`, kept so a
        /// subsequent `:` can confirm the dict/struct decision.
        pending_key_token: bool,
        trailing_comma: Option<Span>,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut previous_meaningful_kind: Option<TokenKind> = None;

    fn last_meaningful_byte_before(source: &str, pos: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        if pos == 0 {
            return None;
        }
        let mut i = pos;
        while i > 0 {
            i -= 1;
            let b = bytes[i];
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                continue;
            }
            // Comments are intentionally not skipped — the FixEdit lands
            // after a trailing comment sitting above the close.
            return Some(i);
        }
        None
    }

    fn span_at_offset(source: &str, start: usize, end: usize) -> Span {
        let line = source[..start].bytes().filter(|b| *b == b'\n').count() + 1;
        let line_start = source[..start].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        Span::with_offsets(start, end, line, start - line_start + 1)
    }

    fn paren_can_be_comma_list(prev: Option<&TokenKind>) -> bool {
        matches!(
            prev,
            Some(
                TokenKind::Identifier(_)
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Fn
                    | TokenKind::At
            )
        )
    }

    fn token_can_start_brace_key(kind: &TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Identifier(_)
                | TokenKind::StringLiteral(_)
                | TokenKind::RawStringLiteral(_)
                | TokenKind::IntLiteral(_)
                | TokenKind::FloatLiteral(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Nil
                | TokenKind::LBracket
        )
    }

    for tok in &tokens {
        match &tok.kind {
            harn_lexer::TokenKind::LineComment { .. }
            | harn_lexer::TokenKind::BlockComment { .. }
            | harn_lexer::TokenKind::Newline => continue,
            _ => {}
        }

        match &tok.kind {
            harn_lexer::TokenKind::LParen => {
                stack.push(Frame {
                    opener: Opener::Paren,
                    open_line: tok.span.line,
                    saw_item: false,
                    eligible: paren_can_be_comma_list(previous_meaningful_kind.as_ref()),
                    decision_made: true,
                    pending_key_token: false,
                    trailing_comma: None,
                });
            }
            harn_lexer::TokenKind::LBracket => {
                stack.push(Frame {
                    opener: Opener::Bracket,
                    open_line: tok.span.line,
                    saw_item: false,
                    eligible: true,
                    decision_made: true,
                    pending_key_token: false,
                    trailing_comma: None,
                });
            }
            harn_lexer::TokenKind::LBrace => {
                stack.push(Frame {
                    opener: Opener::Brace,
                    open_line: tok.span.line,
                    saw_item: false,
                    eligible: false,
                    decision_made: false,
                    pending_key_token: false,
                    trailing_comma: None,
                });
            }
            harn_lexer::TokenKind::RParen
            | harn_lexer::TokenKind::RBracket
            | harn_lexer::TokenKind::RBrace => {
                let Some(frame) = stack.pop() else { continue };
                let matching = matches!(
                    (&frame.opener, &tok.kind),
                    (Opener::Paren, harn_lexer::TokenKind::RParen)
                        | (Opener::Bracket, harn_lexer::TokenKind::RBracket)
                        | (Opener::Brace, harn_lexer::TokenKind::RBrace)
                );
                if !matching {
                    continue;
                }
                if !frame.eligible || !frame.saw_item {
                    continue;
                }
                let close_pos = tok.span.start;
                let Some(last_byte) = last_meaningful_byte_before(source, close_pos) else {
                    continue;
                };
                let has_trailing_comma = source.as_bytes()[last_byte] == b',';
                if tok.span.line > frame.open_line {
                    if has_trailing_comma {
                        continue;
                    }
                    let insert_pos = last_byte + 1;
                    let span = span_at_offset(source, insert_pos, insert_pos);
                    diagnostics.push(LintDiagnostic {
                        rule: "trailing-comma",
                        message: "multiline comma-separated list is missing a trailing comma"
                            .to_string(),
                        span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("add a trailing comma after the last item".to_string()),
                        fix: Some(vec![FixEdit {
                            span,
                            replacement: ",".to_string(),
                        }]),
                    });
                } else if has_trailing_comma {
                    let Some(comma_span) = frame.trailing_comma else {
                        continue;
                    };
                    let mut delete_end = comma_span.end;
                    while delete_end < close_pos {
                        match source.as_bytes()[delete_end] {
                            b' ' | b'\t' => delete_end += 1,
                            _ => break,
                        }
                    }
                    let span = Span::with_offsets(
                        comma_span.start,
                        delete_end,
                        comma_span.line,
                        comma_span.column,
                    );
                    diagnostics.push(LintDiagnostic {
                        rule: "trailing-comma",
                        message: "single-line comma-separated list has a trailing comma"
                            .to_string(),
                        span,
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the trailing comma".to_string()),
                        fix: Some(vec![FixEdit {
                            span,
                            replacement: String::new(),
                        }]),
                    });
                }
            }
            harn_lexer::TokenKind::Comma => {
                if let Some(top) = stack.last_mut() {
                    top.trailing_comma = Some(tok.span);
                }
            }
            harn_lexer::TokenKind::Colon => {
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace)
                        && !top.decision_made
                        && top.pending_key_token
                    {
                        top.eligible = true;
                        top.decision_made = true;
                    }
                }
            }
            harn_lexer::TokenKind::Identifier(_) | harn_lexer::TokenKind::StringLiteral(_) => {
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace) && !top.decision_made {
                        top.pending_key_token = true;
                    }
                }
            }
            _ => {
                // Any other token inside `{ ... }` before a decision means
                // this is a block, not a dict/struct literal.
                if let Some(top) = stack.last_mut() {
                    if matches!(top.opener, Opener::Brace)
                        && !top.decision_made
                        && token_can_start_brace_key(&tok.kind)
                    {
                        top.pending_key_token = true;
                    } else if matches!(top.opener, Opener::Brace) && !top.decision_made {
                        top.decision_made = true;
                        top.eligible = false;
                    }
                }
            }
        }
        if let Some(top) = stack.last_mut() {
            if !matches!(
                tok.kind,
                TokenKind::Comma
                    | TokenKind::Colon
                    | TokenKind::LParen
                    | TokenKind::LBracket
                    | TokenKind::LBrace
            ) {
                top.saw_item = true;
                top.trailing_comma = None;
                if matches!(top.opener, Opener::Brace)
                    && !top.decision_made
                    && matches!(tok.kind, TokenKind::RBracket)
                {
                    top.pending_key_token = true;
                }
            }
        }
        previous_meaningful_kind = Some(tok.kind.clone());
    }
}
