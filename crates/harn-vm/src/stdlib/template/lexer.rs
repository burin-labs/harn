use super::error::TemplateError;

#[derive(Debug, Clone)]
pub(super) enum Token {
    /// Literal text between directives.
    Text {
        content: String,
        /// `{{-` on the following directive — trim trailing whitespace of this text.
        trim_right: bool,
        /// `-}}` on the preceding directive — trim leading whitespace of this text.
        trim_left: bool,
    },
    /// Directive body (content between `{{` / `}}`, with `-` markers stripped).
    Directive {
        body: String,
        line: usize,
        col: usize,
    },
    /// Verbatim content of a `{{ raw }}..{{ endraw }}` block.
    Raw(String),
}

pub(super) fn tokenize(src: &str) -> Result<Vec<Token>, TemplateError> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut cursor = 0;
    let mut pending_trim_left = false;
    let len = bytes.len();

    while cursor < len {
        // Look for the next `{{`.
        let open = find_from(src, cursor, "{{");
        let text_end = open.unwrap_or(len);
        let raw_text = &src[cursor..text_end];

        let this_trim_left = pending_trim_left;
        pending_trim_left = false;

        let mut this_trim_right = false;
        if let Some(o) = open {
            // Inspect the directive start for a `-` trim marker.
            if o + 2 < len && bytes[o + 2] == b'-' {
                this_trim_right = true;
            }
        }

        if !raw_text.is_empty() || this_trim_left || this_trim_right {
            tokens.push(Token::Text {
                content: raw_text.to_string(),
                trim_right: this_trim_right,
                trim_left: this_trim_left,
            });
        }

        let Some(open) = open else {
            break;
        };

        // Position after `{{` (and optional `-`).
        let body_start = open + 2 + if this_trim_right { 1 } else { 0 };

        // Handle `{{# comment #}}`: comments are stripped outright.
        if body_start < len && bytes[body_start] == b'#' {
            let after_hash = body_start + 1;
            let Some(close_hash) = find_from(src, after_hash, "#}}") else {
                let (line, col) = line_col(src, open);
                return Err(TemplateError::new(line, col, "unterminated comment"));
            };
            cursor = close_hash + 3;
            continue;
        }

        // Handle `{{ raw }}` specially: capture until `{{ endraw }}` verbatim.
        let body_trim_start = skip_ws(src, body_start);
        let raw_kw_end = body_trim_start + 3;
        if raw_kw_end <= len && &src[body_trim_start..raw_kw_end.min(len)] == "raw" && {
            let after = raw_kw_end;
            after >= len
                || bytes[after] == b' '
                || bytes[after] == b'\t'
                || bytes[after] == b'\n'
                || bytes[after] == b'\r'
                || (after + 1 < len && &src[after..after + 2] == "}}")
                || (after + 2 < len && &src[after..after + 3] == "-}}")
        } {
            let Some(dir_close) = find_from(src, raw_kw_end, "}}") else {
                let (line, col) = line_col(src, open);
                return Err(TemplateError::new(line, col, "unterminated directive"));
            };
            let raw_body_start = dir_close + 2;

            let (raw_end_open, raw_end_close) =
                find_endraw(src, raw_body_start).ok_or_else(|| {
                    let (line, col) = line_col(src, open);
                    TemplateError::new(line, col, "unterminated `{{ raw }}` block")
                })?;
            let raw_content = src[raw_body_start..raw_end_open].to_string();
            tokens.push(Token::Raw(raw_content));
            cursor = raw_end_close;
            continue;
        }

        // Standard directive: scan for `}}`, respecting quoted strings so a
        // `}}` inside `"..."` doesn't prematurely terminate.
        let (close_pos, trim_after) = find_directive_close(src, body_start).ok_or_else(|| {
            let (line, col) = line_col(src, open);
            TemplateError::new(line, col, "unterminated directive")
        })?;
        let body_end = if trim_after { close_pos - 1 } else { close_pos };
        let body = src[body_start..body_end].trim().to_string();
        let (line, col) = line_col(src, open);
        tokens.push(Token::Directive { body, line, col });
        cursor = close_pos + 2;
        pending_trim_left = trim_after;
    }

    Ok(tokens)
}

fn find_from(s: &str, from: usize, pat: &str) -> Option<usize> {
    s[from..].find(pat).map(|i| i + from)
}

fn skip_ws(s: &str, from: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = from;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

fn line_col(s: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in s.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Scan forward from `start` looking for an unquoted `}}`. Returns
/// `(offset_of_closing_braces, trim_marker_present)` where the trim marker
/// is the `-` immediately before the `}}`.
fn find_directive_close(s: &str, start: usize) -> Option<(usize, bool)> {
    let bytes = s.as_bytes();
    let mut i = start;
    let mut in_str = false;
    let mut str_quote = b'"';
    while i + 1 < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == str_quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_str = true;
            str_quote = b;
            i += 1;
            continue;
        }
        if b == b'}' && bytes[i + 1] == b'}' {
            let trim = i > start && bytes[i - 1] == b'-';
            return Some((i, trim));
        }
        i += 1;
    }
    None
}

/// Find the matching `{{ endraw }}` (whitespace- and trim-marker-tolerant),
/// returning `(directive_open_offset, directive_close_offset_exclusive)`.
fn find_endraw(s: &str, from: usize) -> Option<(usize, usize)> {
    let mut cursor = from;
    while let Some(open) = find_from(s, cursor, "{{") {
        let after = open + 2;
        let body_start = if s.as_bytes().get(after) == Some(&b'-') {
            after + 1
        } else {
            after
        };
        let body_trim_start = skip_ws(s, body_start);
        let close = find_directive_close(s, body_start)?;
        let body_end = if close.1 { close.0 - 1 } else { close.0 };
        let body = s[body_trim_start..body_end].trim();
        if body == "endraw" {
            return Some((open, close.0 + 2));
        }
        cursor = close.0 + 2;
    }
    None
}
