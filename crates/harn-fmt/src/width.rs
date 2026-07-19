use std::collections::BTreeMap;

use harn_lexer::{Lexer, Span, Token, TokenKind};

/// A physical output line that exceeds the configured formatter width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineWidthViolation {
    /// One-based physical line number.
    pub line: usize,
    /// Number of Unicode scalar values on the line.
    pub width: usize,
    /// The complete physical line, without its line ending.
    pub text: String,
}

/// Find output lines that exceed `line_width` after allowing only content that
/// cannot be split without changing its meaning or the formatter's comment
/// preservation policy.
pub fn line_width_violations(source: &str, line_width: usize) -> Vec<LineWidthViolation> {
    let tokens = match Lexer::new(source).tokenize_with_comments() {
        Ok(tokens) => tokens,
        // A caller can use this guard independently of `format_source`, so do
        // not silently accept an overlong line just because its input is not
        // lexically valid. Without tokens there is no safe way to classify an
        // exception; report every physical overflow instead.
        Err(_) => return physical_line_violations(source, line_width),
    };
    let mut comments = Vec::new();
    let mut unbreakable_tokens = Vec::new();
    let mut tokens_by_line: BTreeMap<usize, Vec<&Token>> = BTreeMap::new();

    for token in &tokens {
        match token.kind {
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => {
                comments.push(token.span);
            }
            TokenKind::Newline | TokenKind::Eof => {}
            TokenKind::StringLiteral(_)
            | TokenKind::RawStringLiteral(_)
            | TokenKind::InterpolatedString(_) => {
                unbreakable_tokens.push(token.span);
                if token.span.line == token.span.end_line {
                    tokens_by_line
                        .entry(token.span.line)
                        .or_default()
                        .push(token);
                }
            }
            _ if token.span.line == token.span.end_line => {
                tokens_by_line
                    .entry(token.span.line)
                    .or_default()
                    .push(token);
            }
            _ => {}
        }
    }

    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line_number = index + 1;
            let width = line.chars().count();
            (width > line_width
                && !line_is_comment_only_overflow(source, line, line_number, line_width, &comments)
                && !line_is_inside_unbreakable_token(line_number, &unbreakable_tokens)
                && !line_is_single_unbreakable_token(source, line, line_number, &tokens_by_line)
                && !line.trim_start().starts_with("#!"))
            .then(|| LineWidthViolation {
                line: line_number,
                width,
                text: line.to_string(),
            })
        })
        .collect()
}

fn physical_line_violations(source: &str, line_width: usize) -> Vec<LineWidthViolation> {
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let width = line.chars().count();
            (width > line_width).then(|| LineWidthViolation {
                line: index + 1,
                width,
                text: line.to_string(),
            })
        })
        .collect()
}

fn line_is_inside_unbreakable_token(line_number: usize, tokens: &[Span]) -> bool {
    tokens
        .iter()
        .any(|span| line_number >= span.line && line_number <= span.end_line)
}

fn line_is_comment_only_overflow(
    source: &str,
    line: &str,
    line_number: usize,
    line_width: usize,
    comments: &[Span],
) -> bool {
    comments.iter().any(|span| {
        if line_number < span.line || line_number > span.end_line {
            return false;
        }

        if line_number > span.line {
            return true;
        }

        let start_column = span.column.saturating_sub(1);
        let code_prefix = line.chars().take(start_column).collect::<String>();
        if code_prefix.chars().count() > line_width {
            return false;
        }

        if span.end_line == span.line {
            source
                .get(span.end..)
                .and_then(|rest| rest.split('\n').next())
                .is_some_and(|rest| rest.trim().is_empty())
        } else {
            true
        }
    })
}

fn line_is_single_unbreakable_token(
    source: &str,
    line: &str,
    line_number: usize,
    tokens_by_line: &BTreeMap<usize, Vec<&Token>>,
) -> bool {
    let Some(tokens) = tokens_by_line.get(&line_number) else {
        return false;
    };
    if tokens.len() > 2 {
        return false;
    }
    let token = tokens[0];
    let Some(raw) = source.get(token.span.start..token.span.end) else {
        return false;
    };
    if tokens.len() == 1 {
        return raw == line.trim();
    }

    let suffix = tokens[1];
    let Some(suffix_raw) = source.get(suffix.span.start..suffix.span.end) else {
        return false;
    };
    if matches!(
        suffix.kind,
        TokenKind::Comma | TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
    ) && format!("{raw}{suffix_raw}") == line.trim()
    {
        return true;
    }

    matches!(
        token.kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Pipe
            | TokenKind::Eq
            | TokenKind::Neq
            | TokenKind::NilCoal
    ) && matches!(
        suffix.kind,
        TokenKind::Identifier(_)
            | TokenKind::StringLiteral(_)
            | TokenKind::InterpolatedString(_)
            | TokenKind::RawStringLiteral(_)
    ) && format!("{raw} {suffix_raw}") == line.trim()
}

#[cfg(test)]
mod tests {
    use super::line_width_violations;

    #[test]
    fn allows_a_single_unbreakable_token() {
        assert!(line_width_violations("  an_identifier_that_cannot_be_split\n", 10).is_empty());
        assert!(line_width_violations("  \"a string with spaces\"\n", 10).is_empty());
    }

    #[test]
    fn allows_overflow_that_belongs_only_to_a_trailing_comment() {
        assert!(
            line_width_violations("let x = 1 // this comment is intentionally long\n", 10)
                .is_empty()
        );
        assert_eq!(
            line_width_violations("let value = 1 // the code prefix is too long\n", 10).len(),
            1
        );
    }

    #[test]
    fn reports_breakable_output_overflow() {
        let violations = line_width_violations("let value = one + two + three\n", 10);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].line, 1);
    }

    #[test]
    fn reports_overflow_when_input_cannot_be_lexed() {
        let violations = line_width_violations("let value = \"unterminated\n", 10);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].line, 1);
    }
}
