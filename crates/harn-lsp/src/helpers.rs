use harn_lexer::{LexerError, Span};
use harn_parser::{diagnostic, ParserError, TypeExpr};
use tower_lsp::lsp_types::*;

use crate::symbols::SymbolInfo;

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
    Range {
        start: offset_to_position(source, span.start),
        end: offset_to_position(source, span.end.max(span.start + 1).min(source.len())),
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
    let Some((line_start, line_end)) = line_byte_range(source, pos.line as usize) else {
        return source.len();
    };
    line_start + utf16_col_to_byte(&source[line_start..line_end], pos.character)
}

/// Convert a byte offset in `source` to a 0-based LSP Position.
pub(crate) fn offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            let col = utf16_len(&source[line_start..offset]);
            return Position::new(line, col);
        }
        if ch == '\n' {
            line += 1;
            line_start = i + ch.len_utf8();
        }
    }
    Position::new(line, utf16_len(&source[line_start..offset]))
}

pub(crate) fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

fn line_byte_range(source: &str, line: usize) -> Option<(usize, usize)> {
    let mut current_line = 0usize;
    let mut line_start = 0usize;
    for (idx, ch) in source.char_indices() {
        if current_line == line && ch == '\n' {
            return Some((line_start, idx));
        }
        if ch == '\n' {
            if current_line == line {
                return Some((line_start, idx));
            }
            current_line += 1;
            line_start = idx + ch.len_utf8();
        }
    }
    if current_line == line {
        Some((line_start, source.len()))
    } else {
        None
    }
}

fn utf16_col_to_byte(line: &str, character: u32) -> usize {
    let mut units = 0u32;
    for (idx, ch) in line.char_indices() {
        let width = ch.len_utf16() as u32;
        if units + width > character {
            return idx;
        }
        units += width;
        if units == character {
            return idx + ch.len_utf8();
        }
    }
    line.len()
}

/// Get the word at a given position.
pub(crate) fn word_at_position(source: &str, position: Position) -> Option<String> {
    let offset = lsp_position_to_offset(source, position);
    let (line_start, line_end) = line_byte_range(source, position.line as usize)?;
    if offset < line_start || offset > line_end {
        return None;
    }
    let line = &source[line_start..line_end];
    let mut rel = offset - line_start;
    if !line.is_char_boundary(rel) {
        rel = previous_char_boundary(line, rel);
    }

    let mut start = rel;
    while start > 0 {
        let prev = previous_char_boundary(line, start);
        let ch = line[prev..start].chars().next()?;
        if !(ch.is_alphanumeric() || ch == '_') {
            break;
        }
        start = prev;
    }

    let mut end = rel;
    while end < line.len() {
        let ch = line[end..].chars().next()?;
        if !(ch.is_alphanumeric() || ch == '_') {
            break;
        }
        end += ch.len_utf8();
    }

    if start == end {
        return None;
    }
    Some(line[start..end].to_string())
}

/// Check if cursor is right after a `.` (for method completion).
pub(crate) fn char_before_position(source: &str, position: Position) -> Option<char> {
    let offset = lsp_position_to_offset(source, position);
    let (line_start, _) = line_byte_range(source, position.line as usize)?;
    if offset <= line_start {
        return None;
    }
    source[..offset].chars().next_back()
}

fn dot_receiver_identifier(source: &str, position: Position) -> Option<String> {
    let offset = lsp_position_to_offset(source, position);
    let (line_start, line_end) = line_byte_range(source, position.line as usize)?;
    if offset <= line_start {
        return None;
    }
    let line = &source[line_start..line_end];
    let mut rel = offset - line_start;
    if !line.is_char_boundary(rel) {
        rel = previous_char_boundary(line, rel);
    }
    let dot_start = previous_char_boundary(line, rel);
    if &line[dot_start..rel] != "." || dot_start == 0 {
        return None;
    }
    let mut end = previous_char_boundary(line, dot_start);

    while end > 0 {
        let ch = line[end..].chars().next()?;
        if ch != ' ' {
            break;
        }
        end = previous_char_boundary(line, end);
    }

    let ch = line[end..].chars().next()?;
    if !ch.is_alphanumeric() && ch != '_' {
        return None;
    }
    let id_end = end + ch.len_utf8();
    let mut id_start = end;
    while id_start > 0 {
        let prev = previous_char_boundary(line, id_start);
        let ch = line[prev..id_start].chars().next()?;
        if !(ch.is_alphanumeric() || ch == '_') {
            break;
        }
        id_start = prev;
    }
    Some(line[id_start..id_end].to_string())
}

fn previous_char_boundary(text: &str, index: usize) -> usize {
    let mut i = index.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    text[..i]
        .char_indices()
        .next_back()
        .map_or(0, |(idx, _)| idx)
}

pub(crate) fn infer_dot_receiver_name(source: &str, position: Position) -> Option<String> {
    dot_receiver_identifier(source, position)
}

/// Try to figure out what type the expression before `.` is.
pub(crate) fn infer_dot_receiver_type(
    source: &str,
    position: Position,
    symbols: &[SymbolInfo],
) -> Option<TypeExpr> {
    let offset = lsp_position_to_offset(source, position);
    let (line_start, line_end) = line_byte_range(source, position.line as usize)?;
    if offset <= line_start {
        return None;
    }
    let line = &source[line_start..line_end];
    let mut rel = offset - line_start;
    if !line.is_char_boundary(rel) {
        rel = previous_char_boundary(line, rel);
    }
    let dot_start = previous_char_boundary(line, rel);
    if dot_start == 0 {
        return None;
    }
    let mut end = previous_char_boundary(line, dot_start);
    while end > 0 {
        let ch = line[end..].chars().next()?;
        if ch != ' ' {
            break;
        }
        end = previous_char_boundary(line, end);
    }

    let ch = line[end..].chars().next()?;
    if ch == '"' {
        return Some(TypeExpr::Named("string".to_string()));
    }
    if ch == ']' {
        return Some(TypeExpr::Named("list".to_string()));
    }
    if ch == '}' {
        return Some(TypeExpr::Named("dict".to_string()));
    }

    let name = dot_receiver_identifier(source, position)?;
    for sym in symbols.iter().rev() {
        if sym.name == name {
            if let Some(ref ty) = sym.type_info {
                return Some(ty.clone());
            }
            if matches!(
                sym.kind,
                crate::symbols::HarnSymbolKind::Struct | crate::symbols::HarnSymbolKind::Enum
            ) {
                return Some(TypeExpr::Named(sym.name.clone()));
            }
        }
    }
    None
}

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
        ParserError::Unexpected { span, .. } => Diagnostic {
            range: Range {
                start: Position::new((span.line - 1) as u32, (span.column - 1) as u32),
                end: Position::new((span.line - 1) as u32, span.column as u32),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: diagnostic::parser_error_message(err),
            ..Default::default()
        },
        ParserError::UnexpectedEof { span, .. } => Diagnostic {
            range: Range {
                start: Position::new(
                    (span.line.saturating_sub(1)) as u32,
                    (span.column.saturating_sub(1)) as u32,
                ),
                end: Position::new((span.line.saturating_sub(1)) as u32, span.column as u32),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: diagnostic::parser_error_message(err),
            ..Default::default()
        },
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_offsets_use_utf16_columns() {
        let source = "let 😀name = \"é\"\nnext";
        let name_offset = source.find("name").unwrap();
        assert_eq!(
            lsp_position_to_offset(source, Position::new(0, 6)),
            name_offset
        );
        assert_eq!(offset_to_position(source, name_offset), Position::new(0, 6));
    }

    #[test]
    fn word_at_position_handles_non_ascii_prefix() {
        let source = "let café = 1";
        assert_eq!(
            word_at_position(source, Position::new(0, 6)).as_deref(),
            Some("café")
        );
    }

    #[test]
    fn span_range_uses_utf16_length() {
        let source = "let mood = \"😀\"";
        let start = source.find("\"😀\"").unwrap();
        let end = start + "\"😀\"".len();
        let range = span_to_full_range(
            &Span {
                start,
                end,
                line: 1,
                column: 12,
                end_line: 1,
            },
            source,
        );
        assert_eq!(range.start, Position::new(0, 11));
        assert_eq!(range.end, Position::new(0, 15));
    }
}
