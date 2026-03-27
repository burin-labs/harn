use harn_lexer::Span;

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

    // Header: severity + message
    out.push_str(severity);
    out.push_str(": ");
    out.push_str(message);
    out.push('\n');

    // Location line
    let line_num = span.line;
    let col_num = span.column;

    let gutter_width = line_num.to_string().len();

    out.push_str(&format!(
        "{:>width$}--> {filename}:{line_num}:{col_num}\n",
        " ",
        width = gutter_width + 1,
    ));

    // Blank gutter
    out.push_str(&format!("{:>width$} |\n", " ", width = gutter_width + 1));

    // Source line
    let source_line_opt = source.lines().nth(line_num.wrapping_sub(1));
    if let Some(source_line) = source_line_opt.filter(|_| line_num > 0) {
        out.push_str(&format!(
            "{:>width$} | {source_line}\n",
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
            let carets = "^".repeat(span_len);
            out.push_str(&format!(
                "{:>width$} | {padding}{carets} {label_text}\n",
                " ",
                width = gutter_width + 1,
            ));
        }
    }

    // Help line
    if let Some(help_text) = help {
        out.push_str(&format!(
            "{:>width$} = help: {help_text}\n",
            " ",
            width = gutter_width + 1,
        ));
    }

    out
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
}
