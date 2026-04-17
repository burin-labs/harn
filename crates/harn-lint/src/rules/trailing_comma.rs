//! `trailing-comma` rule: multiline comma-separated lists (argument
//! lists, list literals, dict/struct literals) must end with a trailing
//! comma on the last item. Autofix inserts a `,` at the byte offset
//! immediately after the last item.

use harn_lexer::{FixEdit, Span};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

/// Emit `trailing-comma` diagnostics by scanning the source's tokens for
/// multiline comma-separated lists that lack a trailing comma. Autofix
/// inserts a `,` at the byte offset immediately after the last item.
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
        saw_comma: bool,
        /// True when `{ ... }` has been identified as a dict/struct literal.
        /// Paren/Bracket are always eligible when they contain commas.
        eligible: bool,
        /// For `{ ... }` we look at the first "meaningful" token to decide
        /// eligibility. This tracks whether that decision has been made.
        decision_made: bool,
        /// First identifier/string token inside `{ ... }`, kept so a
        /// subsequent `:` can confirm the dict/struct decision.
        pending_key_token: bool,
    }
    let mut stack: Vec<Frame> = Vec::new();

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
                    saw_comma: false,
                    eligible: true,
                    decision_made: true,
                    pending_key_token: false,
                });
            }
            harn_lexer::TokenKind::LBracket => {
                stack.push(Frame {
                    opener: Opener::Bracket,
                    open_line: tok.span.line,
                    saw_comma: false,
                    eligible: true,
                    decision_made: true,
                    pending_key_token: false,
                });
            }
            harn_lexer::TokenKind::LBrace => {
                stack.push(Frame {
                    opener: Opener::Brace,
                    open_line: tok.span.line,
                    saw_comma: false,
                    eligible: false,
                    decision_made: false,
                    pending_key_token: false,
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
                if !frame.eligible || !frame.saw_comma {
                    continue;
                }
                if tok.span.line <= frame.open_line {
                    continue;
                }
                let close_pos = tok.span.start;
                let Some(last_byte) = last_meaningful_byte_before(source, close_pos) else {
                    continue;
                };
                if source.as_bytes()[last_byte] == b',' {
                    continue;
                }
                let insert_pos = last_byte + 1;
                // Report on the insert line, not the closer's line — editors
                // highlight by span and the closer may be many lines away.
                let insert_line = source[..insert_pos].bytes().filter(|b| *b == b'\n').count() + 1;
                let span = Span::with_offsets(insert_pos, insert_pos, insert_line, 1);
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
            }
            harn_lexer::TokenKind::Comma => {
                if let Some(top) = stack.last_mut() {
                    top.saw_comma = true;
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
                    if matches!(top.opener, Opener::Brace) && !top.decision_made {
                        top.decision_made = true;
                        top.eligible = false;
                    }
                }
            }
        }
    }
}
