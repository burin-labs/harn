use harn_lexer::{Span, Token, TokenKind};
use tower_lsp::lsp_types::*;

use crate::constants::{BUILTINS, TYPE_NAMES};
use crate::helpers::{offset_to_position, utf16_len};
use crate::symbols::{HarnSymbolKind, SymbolInfo};

/// Indices into the semantic token legend's token types array.
/// These must match the order in `semantic_token_legend()`.
pub(crate) mod sem {
    pub const KEYWORD: u32 = 0;
    pub const FUNCTION: u32 = 1;
    pub const PARAMETER: u32 = 2;
    pub const VARIABLE: u32 = 3;
    pub const STRING: u32 = 4;
    pub const NUMBER: u32 = 5;
    pub const OPERATOR: u32 = 6;
    pub const COMMENT: u32 = 7;
    pub const TYPE: u32 = 8;
    pub const ENUM_MEMBER: u32 = 9;
    pub const NAMESPACE: u32 = 10;
}

pub(crate) fn semantic_token_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,     // 0
            SemanticTokenType::FUNCTION,    // 1
            SemanticTokenType::PARAMETER,   // 2
            SemanticTokenType::VARIABLE,    // 3
            SemanticTokenType::STRING,      // 4
            SemanticTokenType::NUMBER,      // 5
            SemanticTokenType::OPERATOR,    // 6
            SemanticTokenType::COMMENT,     // 7
            SemanticTokenType::TYPE,        // 8
            SemanticTokenType::ENUM_MEMBER, // 9
            SemanticTokenType::NAMESPACE,   // 10
        ],
        token_modifiers: vec![],
    }
}

/// Map a lexer `TokenKind` to a semantic token type index.
/// Returns `None` for tokens that should not be highlighted (delimiters, newlines, EOF).
fn token_kind_to_semantic(kind: &TokenKind) -> Option<u32> {
    match kind {
        // Keywords
        TokenKind::Pipeline
        | TokenKind::Extends
        | TokenKind::Override
        | TokenKind::Let
        | TokenKind::Var
        | TokenKind::If
        | TokenKind::Else
        | TokenKind::For
        | TokenKind::In
        | TokenKind::Match
        | TokenKind::Retry
        | TokenKind::Parallel
        | TokenKind::Return
        | TokenKind::Import
        | TokenKind::True
        | TokenKind::False
        | TokenKind::Nil
        | TokenKind::Try
        | TokenKind::Catch
        | TokenKind::Throw
        | TokenKind::Finally
        | TokenKind::Select
        | TokenKind::Fn
        | TokenKind::Spawn
        | TokenKind::While
        | TokenKind::TypeKw
        | TokenKind::Enum
        | TokenKind::Struct
        | TokenKind::Interface
        | TokenKind::Impl
        | TokenKind::Pub
        | TokenKind::From
        | TokenKind::To
        | TokenKind::Tool
        | TokenKind::Skill
        | TokenKind::Exclusive
        | TokenKind::Guard
        | TokenKind::Require
        | TokenKind::Deadline
        | TokenKind::Yield
        | TokenKind::Mutex
        | TokenKind::Defer
        | TokenKind::Break
        | TokenKind::Continue => Some(sem::KEYWORD),

        // Strings
        TokenKind::StringLiteral(_)
        | TokenKind::RawStringLiteral(_)
        | TokenKind::InterpolatedString(_) => Some(sem::STRING),

        // Numbers
        TokenKind::IntLiteral(_) | TokenKind::FloatLiteral(_) | TokenKind::DurationLiteral(_) => {
            Some(sem::NUMBER)
        }

        // Operators
        TokenKind::Eq
        | TokenKind::Neq
        | TokenKind::And
        | TokenKind::Or
        | TokenKind::Pipe
        | TokenKind::NilCoal
        | TokenKind::Pow
        | TokenKind::QuestionDot
        | TokenKind::Arrow
        | TokenKind::Lte
        | TokenKind::Gte
        | TokenKind::PlusAssign
        | TokenKind::MinusAssign
        | TokenKind::StarAssign
        | TokenKind::SlashAssign
        | TokenKind::PercentAssign
        | TokenKind::Assign
        | TokenKind::Not
        | TokenKind::Dot
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::Question
        | TokenKind::Bar => Some(sem::OPERATOR),

        // Comments
        TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => Some(sem::COMMENT),

        // Identifiers are context-dependent — handled separately
        TokenKind::Identifier(_) => Some(sem::VARIABLE),

        // Delimiters, newlines, EOF — not semantically highlighted
        TokenKind::LBrace
        | TokenKind::RBrace
        | TokenKind::LParen
        | TokenKind::RParen
        | TokenKind::LBracket
        | TokenKind::RBracket
        | TokenKind::Comma
        | TokenKind::Colon
        | TokenKind::Semicolon
        | TokenKind::At
        | TokenKind::Newline
        | TokenKind::Eof => None,
    }
}

/// Build semantic tokens from a token stream, using the symbol table for
/// context-aware classification of identifiers (function, parameter, variable,
/// type, enum, namespace).
pub(crate) fn build_semantic_tokens(
    tokens: &[Token],
    symbols: &[SymbolInfo],
    source: &str,
) -> Vec<SemanticToken> {
    let mut result = Vec::new();
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    for (i, token) in tokens.iter().enumerate() {
        let token_type = match &token.kind {
            TokenKind::Identifier(name) => {
                // Context-aware: check what preceded this identifier
                // 1. After `fn` or `pipeline` keyword => function
                // 2. After `:` (type annotation context) and name is a known type => type
                // 3. After `enum` keyword => namespace
                // 4. Check symbol table for classification
                let prev_kind = if i > 0 {
                    Some(&tokens[i - 1].kind)
                } else {
                    None
                };

                if matches!(prev_kind, Some(TokenKind::Fn) | Some(TokenKind::Pipeline)) {
                    sem::FUNCTION
                } else if matches!(prev_kind, Some(TokenKind::Enum)) {
                    sem::NAMESPACE
                } else if matches!(
                    prev_kind,
                    Some(TokenKind::Struct) | Some(TokenKind::Interface)
                ) {
                    sem::TYPE
                } else if matches!(prev_kind, Some(TokenKind::Dot))
                    && is_enum_variant_access(tokens, i, symbols)
                {
                    sem::ENUM_MEMBER
                } else if is_type_annotation_context(tokens, i)
                    && TYPE_NAMES.contains(&name.as_str())
                {
                    sem::TYPE
                } else {
                    // Look up in symbol table
                    classify_identifier(name, &token.span, symbols, source)
                }
            }
            other => match token_kind_to_semantic(other) {
                Some(t) => t,
                None => continue,
            },
        };

        // LSP semantic tokens use 0-based UTF-16 line/column positions.
        let start_position = offset_to_position(source, token.span.start);
        let line = start_position.line;
        let start_char = start_position.character;

        // Calculate token length from byte offsets
        if token.span.end > token.span.start && token.span.end <= source.len() {
            let segment = &source[token.span.start..token.span.end];
            let lines_in_token: Vec<&str> = segment.split('\n').collect();

            if lines_in_token.len() <= 1 {
                // Single-line token
                let length = utf16_len(segment);
                let delta_line = line - prev_line;
                let delta_start = if delta_line == 0 {
                    start_char - prev_start
                } else {
                    start_char
                };
                result.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length,
                    token_type,
                    token_modifiers_bitset: 0,
                });
                prev_line = line;
                prev_start = start_char;
            } else {
                // Multiline token (block comments): emit one entry per line
                for (line_idx, line_text) in lines_in_token.iter().enumerate() {
                    let cur_line = line + line_idx as u32;
                    let cur_start = if line_idx == 0 { start_char } else { 0 };
                    let length = utf16_len(line_text);
                    if length == 0 && line_idx > 0 {
                        continue; // skip empty intermediate lines
                    }
                    let delta_line = cur_line - prev_line;
                    let delta_start = if delta_line == 0 {
                        cur_start - prev_start
                    } else {
                        cur_start
                    };
                    result.push(SemanticToken {
                        delta_line,
                        delta_start,
                        length,
                        token_type,
                        token_modifiers_bitset: 0,
                    });
                    prev_line = cur_line;
                    prev_start = cur_start;
                }
            }
        } else {
            // Fallback: unknown length
            let delta_line = line - prev_line;
            let delta_start = if delta_line == 0 {
                start_char - prev_start
            } else {
                start_char
            };
            result.push(SemanticToken {
                delta_line,
                delta_start,
                length: 1,
                token_type,
                token_modifiers_bitset: 0,
            });
            prev_line = line;
            prev_start = start_char;
        }
    }

    result
}

/// Check if the identifier at position `idx` is an enum variant access.
/// Pattern: `EnumName.Variant` where EnumName is a known enum in the symbol table.
fn is_enum_variant_access(tokens: &[Token], idx: usize, symbols: &[SymbolInfo]) -> bool {
    // idx is the Variant identifier, idx-1 should be Dot, idx-2 should be the enum name
    if idx < 2 {
        return false;
    }
    if !matches!(tokens[idx - 1].kind, TokenKind::Dot) {
        return false;
    }
    if let TokenKind::Identifier(ref enum_name) = tokens[idx - 2].kind {
        symbols
            .iter()
            .any(|s| s.name == *enum_name && s.kind == HarnSymbolKind::Enum)
    } else {
        false
    }
}

/// Check if the identifier at position `idx` is in a type annotation context.
/// This looks for a preceding `:` (possibly after skipping whitespace/newlines)
/// that suggests a type annotation like `x: int` or `-> int`.
fn is_type_annotation_context(tokens: &[Token], idx: usize) -> bool {
    // Walk backwards skipping newlines to find what precedes
    let mut j = idx;
    while j > 0 {
        j -= 1;
        match &tokens[j].kind {
            TokenKind::Newline => continue,
            TokenKind::Colon
            | TokenKind::Arrow
            | TokenKind::Lt
            | TokenKind::Bar
            | TokenKind::Comma => return true,
            // After `[` in list[T] or dict[K, V] context
            TokenKind::LBracket => {
                // Check if preceded by a type name
                if j > 0 {
                    if let TokenKind::Identifier(name) = &tokens[j - 1].kind {
                        if TYPE_NAMES.contains(&name.as_str()) {
                            return true;
                        }
                    }
                }
                return false;
            }
            _ => return false,
        }
    }
    false
}

/// Classify an identifier using the symbol table.
fn classify_identifier(name: &str, span: &Span, symbols: &[SymbolInfo], source: &str) -> u32 {
    // Find the best matching symbol (innermost scope containing this span)
    let offset = span.start;
    let mut best: Option<&SymbolInfo> = None;

    for sym in symbols {
        if sym.name != name {
            continue;
        }
        let in_scope = match sym.scope_span {
            Some(sp) => offset >= sp.start && offset <= sp.end,
            None => true,
        };
        if !in_scope {
            continue;
        }
        match best {
            None => best = Some(sym),
            Some(prev) => {
                let prev_size = match prev.scope_span {
                    Some(sp) => sp.end.saturating_sub(sp.start),
                    None => usize::MAX,
                };
                let this_size = match sym.scope_span {
                    Some(sp) => sp.end.saturating_sub(sp.start),
                    None => usize::MAX,
                };
                if this_size < prev_size {
                    best = Some(sym);
                }
            }
        }
    }

    match best {
        Some(sym) => match sym.kind {
            HarnSymbolKind::Pipeline | HarnSymbolKind::Function => sem::FUNCTION,
            HarnSymbolKind::Parameter => sem::PARAMETER,
            HarnSymbolKind::Variable => sem::VARIABLE,
            HarnSymbolKind::Enum => sem::NAMESPACE,
            HarnSymbolKind::Struct | HarnSymbolKind::Interface => sem::TYPE,
        },
        None => {
            // Check if it's a builtin function
            if BUILTINS.iter().any(|(n, _)| *n == name) {
                sem::FUNCTION
            } else if TYPE_NAMES.contains(&name) {
                sem::TYPE
            } else {
                // Check if it looks like an enum variant (PascalCase with .)
                // by looking at the surrounding source context
                let _ = source; // used for potential future context checks
                sem::VARIABLE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;

    #[test]
    fn semantic_token_lengths_are_utf16() {
        let source = "let mood = \"😀\"\n";
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize_with_comments().unwrap();
        let semantic = build_semantic_tokens(&tokens, &[], source);
        let string_token = semantic
            .iter()
            .find(|token| token.token_type == sem::STRING)
            .expect("string token");
        assert_eq!(string_token.length, 4);
    }
}
