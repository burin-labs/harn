use std::str::FromStr;

use harn_lexer::{LexerError, Span};
use harn_parser::{diagnostic, ParserError, Repair, RepairSafety, TypeExpr};
use serde_json::{Map, Value};
use tower_lsp::lsp_types::*;

use crate::source_text::SourceText;
use crate::symbols::SymbolInfo;

/// Serialize a [`Repair`] into the JSON envelope that rides on
/// `Diagnostic.data`. Code-action providers and IDE clients read this
/// without re-deriving from the code registry, so the field names here
/// are the contract surface:
///
/// ```json
/// { "repair": { "id": "...", "summary": "...", "safety": "..." } }
/// ```
pub(crate) fn repair_data_value(repair: Option<&Repair>) -> Option<serde_json::Value> {
    let repair = repair?;
    Some(serde_json::json!({
        "repair": {
            "id": repair.id.as_str(),
            "summary": repair.summary,
            "safety": repair.safety.as_str(),
        }
    }))
}

/// Serialize the stable diagnostic metadata that rides on
/// `Diagnostic.data`. The nested `repair` envelope is retained for
/// existing clients; the flat fields are the newer code-action contract
/// consumed by IDEs that dispatch directly on repair safety.
pub(crate) fn diagnostic_data_value(
    code: impl Into<String>,
    repair: Option<&Repair>,
) -> serde_json::Value {
    let code = code.into();
    let mut data = Map::new();
    data.insert("code".to_string(), Value::String(code));
    if let Some(repair) = repair {
        data.insert(
            "repair_id".to_string(),
            Value::String(repair.id.as_str().to_string()),
        );
        data.insert(
            "safety".to_string(),
            Value::String(repair.safety.as_str().to_string()),
        );
        if let Some(mut repair_data) = repair_data_value(Some(repair)) {
            if let Some(repair_payload) = repair_data.get_mut("repair") {
                data.insert("repair".to_string(), std::mem::take(repair_payload));
            }
        }
    }
    Value::Object(data)
}

pub(crate) fn repair_code_action_kind(repair: Option<&Repair>) -> CodeActionKind {
    repair
        .map(|repair| code_action_kind_for_repair_safety(repair.safety))
        .unwrap_or(CodeActionKind::QUICKFIX)
}

pub(crate) fn repair_code_action_data(
    diagnostic: &Diagnostic,
    repair: Option<&Repair>,
) -> Option<serde_json::Value> {
    let repair = repair?;
    Some(serde_json::json!({
        "repair_id": repair.id.as_str(),
        "safety": repair.safety.as_str(),
        "diagnostic_code": diagnostic_code_string(diagnostic.code.as_ref()),
    }))
}

pub(crate) fn diagnostic_repair_code_action_kind(diagnostic: &Diagnostic) -> CodeActionKind {
    diagnostic
        .data
        .as_ref()
        .and_then(|data| data.get("safety"))
        .and_then(|value| value.as_str())
        .and_then(|safety| RepairSafety::from_str(safety).ok())
        .map(code_action_kind_for_repair_safety)
        .unwrap_or(CodeActionKind::QUICKFIX)
}

pub(crate) fn diagnostic_repair_code_action_data(
    diagnostic: &Diagnostic,
) -> Option<serde_json::Value> {
    let data = diagnostic.data.as_ref()?;
    let repair_id = data.get("repair_id")?.as_str()?;
    let safety = data.get("safety")?.as_str()?;
    Some(serde_json::json!({
        "repair_id": repair_id,
        "safety": safety,
        "diagnostic_code": diagnostic_code_string(diagnostic.code.as_ref()),
    }))
}

pub(crate) fn diagnostic_code_string(code: Option<&NumberOrString>) -> Option<String> {
    match code {
        Some(NumberOrString::String(code)) => Some(code.clone()),
        Some(NumberOrString::Number(code)) => Some(code.to_string()),
        None => None,
    }
}

/// Map a [`RepairSafety`] to its `quickfix.harn.<class>` code-action kind.
///
/// The string suffix matches `RepairSafety::as_str()` so IDE clients that
/// register handlers by safety class observe the same wire-format value
/// they consume from `Diagnostic.data.safety`. `RepairSafety::as_str()`
/// returns a stable `&'static str` so the kind constants below avoid an
/// allocation on the diagnostic hot path.
fn code_action_kind_for_repair_safety(safety: RepairSafety) -> CodeActionKind {
    code_action_kind_for_safety_name(safety.as_str())
}

pub(crate) fn code_action_kind_for_safety_name(safety: &str) -> CodeActionKind {
    match safety {
        "format-only" => CodeActionKind::new("quickfix.harn.format-only"),
        "behavior-preserving" => CodeActionKind::new("quickfix.harn.behavior-preserving"),
        "scope-local" => CodeActionKind::new("quickfix.harn.scope-local"),
        "surface-changing" => CodeActionKind::new("quickfix.harn.surface-changing"),
        "capability-changing" => CodeActionKind::new("quickfix.harn.capability-changing"),
        "needs-human" => CodeActionKind::new("quickfix.harn.needs-human"),
        _ => CodeActionKind::QUICKFIX,
    }
}

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
pub(crate) fn span_to_full_range(span: &Span, source: &SourceText) -> Range {
    Range {
        start: source.position(span.start),
        end: source.position(span.end.max(span.start + 1).min(source.len())),
    }
}

/// Check whether a 0-based LSP Position falls within a 1-based Span.
pub(crate) fn position_in_span(pos: &Position, span: &Span, source: &SourceText) -> bool {
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

/// Get the word at a given position.
pub(crate) fn word_at_position(source: &SourceText, position: Position) -> Option<String> {
    word_span_at_position(source, position).map(|(word, _)| word)
}

/// The word under the cursor together with its absolute byte start offset.
///
/// The start offset is what makes it possible to ask what precedes the word
/// rather than what precedes the cursor. `char_before_position` answers the
/// latter, which is the wrong question for hover: a hover cursor lands *inside*
/// a word (`ex|it`), so the character before it is just the previous letter.
pub(crate) fn word_span_at_position(
    source: &SourceText,
    position: Position,
) -> Option<(String, usize)> {
    let offset = source.offset(position);
    let (line_start, line_end) = source.line_range(position.line)?;
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
    Some((line[start..end].to_string(), line_start + start))
}

/// Whether the word starting at `word_start` is reached through a receiver
/// (`recv.name`) rather than named in its own right.
///
/// A member access and a bare identifier can spell the same word while meaning
/// unrelated things: `exit` is a global builtin, but `harness.exit` is not a
/// method at all. Resolving the former for the latter tells a reader a method
/// exists when the runtime will reject it.
///
/// Leading whitespace is skipped so that a wrapped method chain (`value\n
/// .map(...)`) still reads as a member access. `..` is excluded because a range
/// bound (`arr[0..len]`) is not a receiver, and treating it as one would hide
/// the genuine builtin behind it.
pub(crate) fn is_member_access(source: &str, word_start: usize) -> bool {
    let before = source[..word_start].trim_end_matches([' ', '\t', '\r', '\n']);
    before.ends_with('.') && !before.ends_with("..")
}

/// Check if cursor is right after a `.` (for method completion).
pub(crate) fn char_before_position(source: &SourceText, position: Position) -> Option<char> {
    let offset = source.offset(position);
    let (line_start, _) = source.line_range(position.line)?;
    if offset <= line_start {
        return None;
    }
    source[..offset].chars().next_back()
}

fn dot_receiver_identifier(source: &SourceText, position: Position) -> Option<String> {
    let offset = source.offset(position);
    let (line_start, line_end) = source.line_range(position.line)?;
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

pub(crate) fn infer_dot_receiver_name(source: &SourceText, position: Position) -> Option<String> {
    dot_receiver_identifier(source, position)
}

/// Try to figure out what type the expression before `.` is.
pub(crate) fn infer_dot_receiver_type(
    source: &SourceText,
    position: Position,
    symbols: &[SymbolInfo],
) -> Option<TypeExpr> {
    let offset = source.offset(position);
    let (line_start, line_end) = source.line_range(position.line)?;
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
        LexerError::IntegerLiteralOutOfRange(lit, span) => (
            format!("Integer literal `{lit}` is out of range for int (i64)"),
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
        code: Some(NumberOrString::String(
            diagnostic::lexer_error_code(err).to_string(),
        )),
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
            code: Some(NumberOrString::String(
                diagnostic::parser_error_code(err).to_string(),
            )),
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
            code: Some(NumberOrString::String(
                diagnostic::parser_error_code(err).to_string(),
            )),
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
    fn word_at_position_handles_non_ascii_prefix() {
        let source = SourceText::new("let café = 1");
        assert_eq!(
            word_at_position(&source, Position::new(0, 6)).as_deref(),
            Some("café")
        );
    }

    #[test]
    fn span_range_uses_utf16_length() {
        let source = SourceText::new("let mood = \"😀\"");
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
            &source,
        );
        assert_eq!(range.start, Position::new(0, 11));
        assert_eq!(range.end, Position::new(0, 15));
    }
}
