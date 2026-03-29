use harn_lexer::{LexerError, Span};
use harn_parser::{format_type, ParserError};
use tower_lsp::lsp_types::*;

use crate::symbols::SymbolInfo;

// ---------------------------------------------------------------------------
// Position / span utilities
// ---------------------------------------------------------------------------

/// Convert a 1-based Span to a 0-based LSP Range.
pub(crate) fn span_to_range(span: &Span) -> Range {
    Range {
        start: Position::new(
            span.line.saturating_sub(1) as u32,
            span.column.saturating_sub(1) as u32,
        ),
        end: Position::new(span.line.saturating_sub(1) as u32, span.column as u32),
    }
}

/// Convert a Span to an LSP Range using byte offsets for accurate end position.
pub(crate) fn span_to_full_range(span: &Span, source: &str) -> Range {
    let start_line = span.line.saturating_sub(1) as u32;
    let start_col = span.column.saturating_sub(1) as u32;

    // Calculate end position from byte offset
    let mut end_line = start_line;
    let mut end_col = start_col;
    if span.end > span.start && span.end <= source.len() {
        let segment = &source[span.start..span.end];
        for ch in segment.chars() {
            if ch == '\n' {
                end_line += 1;
                end_col = 0;
            } else {
                end_col += 1;
            }
        }
        // If we only advanced columns (single line), set end_col relative to start
        if end_line == start_line {
            end_col = start_col + segment.chars().count() as u32;
        }
    } else {
        end_col = start_col + 1;
    }

    Range {
        start: Position::new(start_line, start_col),
        end: Position::new(end_line, end_col),
    }
}

/// Check whether a 0-based LSP Position falls within a 1-based Span.
pub(crate) fn position_in_span(pos: &Position, span: &Span, source: &str) -> bool {
    let r = span_to_full_range(span, source);
    if pos.line < r.start.line || pos.line > r.end.line {
        return false;
    }
    if pos.line == r.start.line && pos.character < r.start.character {
        return false;
    }
    if pos.line == r.end.line && pos.character > r.end.character {
        return false;
    }
    true
}

/// Convert a 0-based LSP Position to a byte offset in the source string.
pub(crate) fn lsp_position_to_offset(source: &str, pos: Position) -> usize {
    let mut offset = 0;
    for (i, line) in source.split('\n').enumerate() {
        if i == pos.line as usize {
            return offset + (pos.character as usize).min(line.len());
        }
        offset += line.len() + 1; // +1 for the newline
    }
    source.len()
}

/// Convert a byte offset in `source` to a 0-based LSP Position.
pub(crate) fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in source.char_indices() {
        if i == offset {
            return Position::new(line, col);
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    // offset == source.len() (end of file)
    Position::new(line, col)
}

/// Get the word at a given position.
pub(crate) fn word_at_position(source: &str, position: Position) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let line = lines.get(position.line as usize)?;
    let col = position.character as usize;
    if col > line.len() {
        return None;
    }

    let chars: Vec<char> = line.chars().collect();
    let mut start = col;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
        end += 1;
    }

    if start == end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// Check if cursor is right after a `.` (for method completion).
pub(crate) fn char_before_position(source: &str, position: Position) -> Option<char> {
    let lines: Vec<&str> = source.lines().collect();
    let line = lines.get(position.line as usize)?;
    let col = position.character as usize;
    if col == 0 {
        return None;
    }
    line.chars().nth(col - 1)
}

/// Try to figure out what type the expression before `.` is.
pub(crate) fn infer_dot_receiver_type(
    source: &str,
    position: Position,
    symbols: &[SymbolInfo],
) -> Option<String> {
    // Walk backwards from the dot to find the identifier
    let lines: Vec<&str> = source.lines().collect();
    let line = lines.get(position.line as usize)?;
    let col = position.character as usize;
    if col < 2 {
        return None;
    }

    let chars: Vec<char> = line.chars().collect();
    // Position is after the `.`, so chars[col-1] is `.`. Walk back from col-2.
    let mut end = col - 1; // the dot
    if end == 0 {
        return None;
    }
    end -= 1; // char before dot

    // Skip trailing whitespace (unusual but handle it)
    while end > 0 && chars[end] == ' ' {
        end -= 1;
    }

    // Check for string literal ending in "
    if chars[end] == '"' {
        return Some("string".to_string());
    }
    // Check for ] (list subscript or literal)
    if chars[end] == ']' {
        return Some("list".to_string());
    }
    // Check for } (dict literal)
    if chars[end] == '}' {
        return Some("dict".to_string());
    }

    // Otherwise try to extract an identifier
    if !chars[end].is_alphanumeric() && chars[end] != '_' {
        return None;
    }
    let id_end = end + 1;
    let mut id_start = end;
    while id_start > 0 && (chars[id_start - 1].is_alphanumeric() || chars[id_start - 1] == '_') {
        id_start -= 1;
    }
    let name: String = chars[id_start..id_end].iter().collect();

    // Look up the variable's type in the symbol table
    for sym in symbols.iter().rev() {
        if sym.name == name {
            if let Some(ref ty) = sym.type_info {
                return Some(format_type(ty));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Error conversion helpers
// ---------------------------------------------------------------------------

pub(crate) fn lexer_error_to_diagnostic(err: &LexerError) -> Diagnostic {
    let (message, line, col) = match err {
        LexerError::UnexpectedCharacter(ch, span) => (
            format!("Unexpected character '{ch}'"),
            span.line,
            span.column,
        ),
        LexerError::UnterminatedString(span) => {
            ("Unterminated string".to_string(), span.line, span.column)
        }
        LexerError::UnterminatedBlockComment(span) => (
            "Unterminated block comment".to_string(),
            span.line,
            span.column,
        ),
    };

    Diagnostic {
        range: Range {
            start: Position::new((line - 1) as u32, (col - 1) as u32),
            end: Position::new((line - 1) as u32, col as u32),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("harn".to_string()),
        message,
        ..Default::default()
    }
}

pub(crate) fn parser_error_to_diagnostic(err: &ParserError) -> Diagnostic {
    match err {
        ParserError::Unexpected {
            got,
            expected,
            span,
        } => Diagnostic {
            range: Range {
                start: Position::new((span.line - 1) as u32, (span.column - 1) as u32),
                end: Position::new((span.line - 1) as u32, span.column as u32),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Expected {expected}, got {got}"),
            ..Default::default()
        },
        ParserError::UnexpectedEof { expected } => Diagnostic {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(0, 1),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Unexpected end of file, expected {expected}"),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Code action helpers
// ---------------------------------------------------------------------------

/// Extract the first backtick-quoted name from a diagnostic message.
/// E.g., "variable `foo` is declared but never used" -> Some("foo")
pub(crate) fn extract_backtick_name(msg: &str) -> Option<String> {
    let start = msg.find('`')? + 1;
    let rest = &msg[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Find the byte offset of a whole-word occurrence of `word` within `region`.
pub(crate) fn find_word_in_region(region: &str, word: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(pos) = region[search_from..].find(word) {
        let abs = search_from + pos;
        let before_ok = abs == 0
            || !region.as_bytes()[abs - 1].is_ascii_alphanumeric()
                && region.as_bytes()[abs - 1] != b'_';
        let after_pos = abs + word.len();
        let after_ok = after_pos >= region.len()
            || !region.as_bytes()[after_pos].is_ascii_alphanumeric()
                && region.as_bytes()[after_pos] != b'_';
        if before_ok && after_ok {
            return Some(abs);
        }
        search_from = abs + 1;
    }
    None
}

/// Simplify a boolean comparison expression.
/// Handles patterns like `x == true`, `x == false`, `true == x`, `false == x`.
pub(crate) fn simplify_bool_comparison(expr: &str) -> Option<String> {
    let trimmed = expr.trim();

    // Try to split on `==` or `!=`
    for op in &["==", "!="] {
        if let Some(idx) = trimmed.find(op) {
            let lhs = trimmed[..idx].trim();
            let rhs = trimmed[idx + op.len()..].trim();

            let (bool_val, other) = if rhs == "true" || rhs == "false" {
                (rhs, lhs)
            } else if lhs == "true" || lhs == "false" {
                (lhs, rhs)
            } else {
                continue;
            };

            let is_eq = *op == "==";
            let is_true = bool_val == "true";

            // `x == true` -> `x`, `x == false` -> `!x`
            // `x != true` -> `!x`, `x != false` -> `x`
            return if is_eq == is_true {
                Some(other.to_string())
            } else {
                Some(format!("!{other}"))
            };
        }
    }
    None
}
