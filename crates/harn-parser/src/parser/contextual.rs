//! Contextual keywords: words that carry syntax only in one position.
//!
//! `tool`, `select`, `require`, and the other words in
//! [`harn_lexer::CONTEXTUAL_KEYWORDS`] lex as identifiers. The rules here
//! decide, from the surrounding tokens alone, when one introduces its
//! construct. The parser and the language server both read them, so editor
//! highlighting cannot disagree with the parse.

use harn_lexer::{Token, TokenKind, CONTEXTUAL_STATEMENT_KEYWORDS};

/// The contextual statement keyword introduced by the identifier at `pos`,
/// if any.
#[must_use]
pub fn contextual_statement_at(tokens: &[Token], pos: usize) -> Option<&'static str> {
    let Some(TokenKind::Identifier(name)) = tokens.get(pos).map(|token| &token.kind) else {
        return None;
    };
    let word = CONTEXTUAL_STATEMENT_KEYWORDS
        .iter()
        .copied()
        .find(|word| *word == name)?;
    introduces_construct(tokens, pos, word).then_some(word)
}

/// Whether the identifier `name` at `pos` introduces its construct.
///
/// Every rule accepts exactly the token sequences the construct accepted
/// while its word was reserved, so a program that parsed before still parses
/// the same way. Sequences that were syntax errors then, such as
/// `select = 1` or `tool.name`, now read the word as an identifier.
pub(super) fn introduces_construct(tokens: &[Token], pos: usize, name: &str) -> bool {
    if !matches!(tokens.get(pos).map(|t| &t.kind), Some(TokenKind::Identifier(word)) if word == name)
    {
        return false;
    }
    let next = tokens.get(pos + 1).map(|t| &t.kind);
    match name {
        "select" | "scope" | "block" | "cost_route" => matches!(next, Some(TokenKind::LBrace)),
        "tool" | "skill" | "eval_pack" => matches!(next, Some(TokenKind::Identifier(_))),
        "mutex" => match next {
            Some(TokenKind::LBrace) => true,
            Some(TokenKind::LParen) => paren_group_precedes_brace(tokens, pos + 1),
            _ => false,
        },
        "deadline" => match next {
            Some(
                TokenKind::DurationLiteral(_)
                | TokenKind::IntLiteral(_)
                | TokenKind::FloatLiteral(_)
                | TokenKind::Identifier(_),
            ) => true,
            Some(TokenKind::LParen) => paren_group_precedes_brace(tokens, pos + 1),
            _ => false,
        },
        "require" | "emit" => next.is_some_and(|kind| !continues_value_expression(kind)),
        _ => false,
    }
}

/// Tokens that can follow an identifier inside an expression or an
/// assignment target, or that end it.
fn continues_value_expression(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Newline
            | TokenKind::Eof
            | TokenKind::Semicolon
            | TokenKind::Comma
            | TokenKind::Colon
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::Question
            | TokenKind::Assign
            | TokenKind::PlusAssign
            | TokenKind::MinusAssign
            | TokenKind::StarAssign
            | TokenKind::SlashAssign
            | TokenKind::PercentAssign
            | TokenKind::Eq
            | TokenKind::Neq
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Pipe
            | TokenKind::NilCoal
            | TokenKind::Pow
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::Lte
            | TokenKind::Gte
            | TokenKind::Plus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::In
            | TokenKind::DotDot
            | TokenKind::DotDotEq
            | TokenKind::LineComment { .. }
            | TokenKind::BlockComment { .. }
    )
}

/// Whether the `(` at `open` closes before a `{`.
fn paren_group_precedes_brace(tokens: &[Token], open: usize) -> bool {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return matches!(
                        tokens.get(index + 1).map(|t| &t.kind),
                        Some(TokenKind::LBrace)
                    );
                }
            }
            TokenKind::Eof => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::{Lexer, CONTEXTUAL_KEYWORDS};

    #[test]
    fn every_contextual_statement_is_a_contextual_keyword() {
        for word in CONTEXTUAL_STATEMENT_KEYWORDS {
            assert!(
                CONTEXTUAL_KEYWORDS.contains(word),
                "`{word}` must be listed in harn_lexer::CONTEXTUAL_KEYWORDS"
            );
        }
    }

    #[test]
    fn statement_words_follow_their_construct_shape() {
        let at = |source: &str| {
            let tokens = Lexer::new(source).tokenize().unwrap();
            contextual_statement_at(&tokens, 0)
        };
        assert_eq!(at("select { }"), Some("select"));
        assert_eq!(at("select = 1"), None);
        assert_eq!(at("tool lookup(q: string) { }"), Some("tool"));
        assert_eq!(at("tool.name"), None);
        assert_eq!(at("require x > 0"), Some("require"));
        assert_eq!(at("require = 1"), None);
        assert_eq!(at("mutex(key) { }"), Some("mutex"));
        assert_eq!(at("mutex(key)"), None);
        assert_eq!(at("deadline 5s { }"), Some("deadline"));
        assert_eq!(at("deadline + 1"), None);
    }
}
