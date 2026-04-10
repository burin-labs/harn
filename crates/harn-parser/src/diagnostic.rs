use std::io::IsTerminal;

use harn_lexer::Span;
use yansi::{Color, Paint};

use crate::ParserError;

/// Compute the Levenshtein edit distance between two strings.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = b_chars.len();
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for (i, ac) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bc) in b_chars.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Find the closest match to `name` among `candidates`, within `max_dist` edits.
pub fn find_closest_match<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
    max_dist: usize,
) -> Option<&'a str> {
    candidates
        .filter(|c| c.len().abs_diff(name.len()) <= max_dist)
        .min_by_key(|c| edit_distance(name, c))
        .filter(|c| edit_distance(name, c) <= max_dist && *c != name)
}

/// Render a Rust-style diagnostic message.
///
/// Example output:
/// ```text
/// error: undefined variable `x`
///   --> example.harn:5:12
///    |
///  5 |     let y = x + 1
///    |             ^ not found in this scope
/// ```
pub fn render_diagnostic(
    source: &str,
    filename: &str,
    span: &Span,
    severity: &str,
    message: &str,
    label: Option<&str>,
    help: Option<&str>,
) -> String {
    let mut out = String::new();
    let severity_color = severity_color(severity);
    let gutter = style_fragment("|", Color::Blue, false);
    let arrow = style_fragment("-->", Color::Blue, true);
    let help_prefix = style_fragment("help", Color::Cyan, true);
    let note_prefix = style_fragment("note", Color::Magenta, true);

    // Header: severity + message
    out.push_str(&style_fragment(severity, severity_color, true));
    out.push_str(": ");
    out.push_str(message);
    out.push('\n');

    // Location line
    let line_num = span.line;
    let col_num = span.column;

    let gutter_width = line_num.to_string().len();

    out.push_str(&format!(
        "{:>width$}{arrow} {filename}:{line_num}:{col_num}\n",
        " ",
        width = gutter_width + 1,
    ));

    // Blank gutter
    out.push_str(&format!(
        "{:>width$} {gutter}\n",
        " ",
        width = gutter_width + 1,
    ));

    // Source line
    let source_line_opt = source.lines().nth(line_num.wrapping_sub(1));
    if let Some(source_line) = source_line_opt.filter(|_| line_num > 0) {
        out.push_str(&format!(
            "{:>width$} {gutter} {source_line}\n",
            line_num,
            width = gutter_width + 1,
        ));

        // Caret line
        if let Some(label_text) = label {
            // Calculate span display width using character counts, not byte offsets
            let span_len = if span.end > span.start && span.start <= source.len() {
                let span_text = &source[span.start.min(source.len())..span.end.min(source.len())];
                span_text.chars().count().max(1)
            } else {
                1
            };
            let col_num = col_num.max(1); // ensure at least 1
            let padding = " ".repeat(col_num - 1);
            let carets = style_fragment(&"^".repeat(span_len), severity_color, true);
            out.push_str(&format!(
                "{:>width$} {gutter} {padding}{carets} {label_text}\n",
                " ",
                width = gutter_width + 1,
            ));
        }
    }

    // Help line
    if let Some(help_text) = help {
        out.push_str(&format!(
            "{:>width$} = {help_prefix}: {help_text}\n",
            " ",
            width = gutter_width + 1,
        ));
    }

    if let Some(note_text) = fun_note(severity) {
        out.push_str(&format!(
            "{:>width$} = {note_prefix}: {note_text}\n",
            " ",
            width = gutter_width + 1,
        ));
    }

    out
}

fn severity_color(severity: &str) -> Color {
    match severity {
        "error" => Color::Red,
        "warning" => Color::Yellow,
        "note" => Color::Magenta,
        _ => Color::Cyan,
    }
}

fn style_fragment(text: &str, color: Color, bold: bool) -> String {
    if !colors_enabled() {
        return text.to_string();
    }

    let mut paint = Paint::new(text).fg(color);
    if bold {
        paint = paint.bold();
    }
    paint.to_string()
}

fn colors_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

fn fun_note(severity: &str) -> Option<&'static str> {
    if std::env::var("HARN_FUN").ok().as_deref() != Some("1") {
        return None;
    }

    Some(match severity {
        "error" => "the compiler stepped on a rake here.",
        "warning" => "this still runs, but it has strong 'double-check me' energy.",
        _ => "a tiny gremlin has left a note in the margins.",
    })
}

pub fn parser_error_message(err: &ParserError) -> String {
    match err {
        ParserError::Unexpected { got, expected, .. } => {
            format!("expected {expected}, found {got}")
        }
        ParserError::UnexpectedEof { expected, .. } => {
            format!("unexpected end of file, expected {expected}")
        }
    }
}

pub fn parser_error_label(err: &ParserError) -> &'static str {
    match err {
        ParserError::Unexpected { got, .. } if got == "Newline" => "line break not allowed here",
        ParserError::Unexpected { .. } => "unexpected token",
        ParserError::UnexpectedEof { .. } => "file ends here",
    }
}

pub fn parser_error_help(err: &ParserError) -> Option<&'static str> {
    match err {
        ParserError::UnexpectedEof { expected, .. } | ParserError::Unexpected { expected, .. } => {
            match expected.as_str() {
                "}" => Some("add a closing `}` to finish this block"),
                ")" => Some("add a closing `)` to finish this expression or parameter list"),
                "]" => Some("add a closing `]` to finish this list or subscript"),
                "fn, struct, enum, or pipeline after pub" => {
                    Some("use `pub fn`, `pub pipeline`, `pub enum`, or `pub struct`")
                }
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_diagnostic() {
        let source = "pipeline default(task) {\n    let y = x + 1\n}";
        let span = Span {
            start: 28,
            end: 29,
            line: 2,
            column: 13,
            end_line: 2,
        };
        let output = render_diagnostic(
            source,
            "example.harn",
            &span,
            "error",
            "undefined variable `x`",
            Some("not found in this scope"),
            None,
        );
        assert!(output.contains("error: undefined variable `x`"));
        assert!(output.contains("--> example.harn:2:13"));
        assert!(output.contains("let y = x + 1"));
        assert!(output.contains("^ not found in this scope"));
    }

    #[test]
    fn test_diagnostic_with_help() {
        let source = "let y = xx + 1";
        let span = Span {
            start: 8,
            end: 10,
            line: 1,
            column: 9,
            end_line: 1,
        };
        let output = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "undefined variable `xx`",
            Some("not found in this scope"),
            Some("did you mean `x`?"),
        );
        assert!(output.contains("help: did you mean `x`?"));
    }

    #[test]
    fn test_multiline_source() {
        let source = "line1\nline2\nline3";
        let span = Span::with_offsets(6, 11, 2, 1); // "line2"
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "bad line",
            Some("here"),
            None,
        );
        assert!(result.contains("line2"));
        assert!(result.contains("^^^^^"));
    }

    #[test]
    fn test_single_char_span() {
        let source = "let x = 42";
        let span = Span::with_offsets(4, 5, 1, 5); // "x"
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "warning",
            "unused",
            Some("never used"),
            None,
        );
        assert!(result.contains("^"));
        assert!(result.contains("never used"));
    }

    #[test]
    fn test_with_help() {
        let source = "let y = reponse";
        let span = Span::with_offsets(8, 15, 1, 9);
        let result = render_diagnostic(
            source,
            "test.harn",
            &span,
            "error",
            "undefined",
            None,
            Some("did you mean `response`?"),
        );
        assert!(result.contains("help:"));
        assert!(result.contains("response"));
    }

    #[test]
    fn test_parser_error_helpers_for_eof() {
        let err = ParserError::UnexpectedEof {
            expected: "}".into(),
            span: Span::with_offsets(10, 10, 3, 1),
        };
        assert_eq!(
            parser_error_message(&err),
            "unexpected end of file, expected }"
        );
        assert_eq!(parser_error_label(&err), "file ends here");
        assert_eq!(
            parser_error_help(&err),
            Some("add a closing `}` to finish this block")
        );
    }
}
