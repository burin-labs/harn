//! Minimal recursive-descent TypeScript value-expression parser used to
//! decode the argument payload of each `name(...)` tool call. Handles
//! object and array literals, string / template literals, numbers, and
//! the limited set of keywords (`true` / `false` / `null` / `undefined`)
//! that models emit when transcribing tool calls.

use super::parse::{ident_length, scan_heredoc, unescape_heredoc_body, HeredocError};

/// Minimal recursive-descent parser for a TypeScript value expression. Handles
/// object and array literals, string literals (double-quoted and single-quoted),
/// template literals (backticks) including escape sequences, numbers (int and
/// float, negative), booleans, null, undefined, and identifier keys inside
/// object literals.
pub(super) struct TsValueParser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl<'a> TsValueParser<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        TsValueParser {
            bytes: text.as_bytes(),
            text,
            pos: 0,
        }
    }

    pub(super) fn position(&self) -> usize {
        self.pos
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    /// Append a character to `out`, decoding a full multi-byte UTF-8 scalar from
    /// the source when `b` is a non-ASCII lead byte. `advance()` yields one byte
    /// at a time, so pushing `b as char` for a lead byte emits one Latin-1 char
    /// per byte and mojibakes any accented / emoji / CJK value. Heredoc bodies
    /// and `\u{...}` escapes already decode correctly; this keeps quoted and
    /// template string values consistent with them.
    fn push_scalar(&mut self, out: &mut String, b: u8) {
        if b < 0x80 {
            out.push(b as char);
            return;
        }
        // `advance()` already consumed the lead byte (pos is past it). Decode the
        // whole scalar from the lead byte and resync pos to the scalar's end.
        let start = self.pos - 1;
        let ch = self.text[start..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        self.pos = start + ch.len_utf8();
    }

    pub(super) fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(b) = self.peek() {
                if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Line comments
            if self.peek() == Some(b'/') && self.bytes.get(self.pos + 1) == Some(&b'/') {
                while let Some(b) = self.peek() {
                    if b == b'\n' {
                        self.pos += 1;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // Block comments
            if self.peek() == Some(b'/') && self.bytes.get(self.pos + 1) == Some(&b'*') {
                self.pos += 2;
                while self.pos + 1 < self.bytes.len() {
                    if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    pub(super) fn parse_value(&mut self) -> Result<serde_json::Value, String> {
        self.skip_ws_and_comments();
        let c = self.peek().ok_or("unexpected end of input")?;
        match c {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' | b'\'' => self.parse_string_literal(c),
            b'`' => self.parse_template_literal(),
            b'<' if self.bytes.get(self.pos + 1) == Some(&b'<') => self.parse_heredoc(),
            b't' | b'f' => self.parse_boolean(),
            b'n' => self.parse_null(),
            b'u' => self.parse_undefined(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!(
                "unexpected character `{}` starting a value",
                other as char
            )),
        }
    }

    fn parse_object(&mut self) -> Result<serde_json::Value, String> {
        // consume '{'
        self.advance();
        let mut map = serde_json::Map::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b'}') {
                self.advance();
                return Ok(serde_json::Value::Object(map));
            }
            // Key: bare identifier OR string literal.
            let key = if let Some(b) = self.peek() {
                if b == b'"' || b == b'\'' {
                    match self.parse_string_literal(b)? {
                        serde_json::Value::String(s) => s,
                        _ => unreachable!(),
                    }
                } else {
                    let len = ident_length(&self.bytes[self.pos..])
                        .ok_or("expected an object key (identifier or string) inside `{ ... }`")?;
                    let k = self.text[self.pos..self.pos + len].to_string();
                    self.pos += len;
                    k
                }
            } else {
                return Err("unexpected end of input inside object literal".to_string());
            };
            self.skip_ws_and_comments();
            // TS shorthand `{ foo }` is legal but rare for our tool calls; we
            // disallow it to keep the contract explicit.
            if self.peek() != Some(b':') {
                return Err(format!(
                    "expected `:` after key `{key}` inside object literal"
                ));
            }
            self.advance();
            self.skip_ws_and_comments();
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws_and_comments();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                    continue;
                }
                Some(b'}') => {
                    self.advance();
                    return Ok(serde_json::Value::Object(map));
                }
                Some(other) => {
                    return Err(format!(
                        "expected `,` or `}}` after value inside object literal, got `{}`",
                        other as char
                    ));
                }
                None => {
                    return Err("unexpected end of input inside object literal".to_string());
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<serde_json::Value, String> {
        self.advance(); // '['
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b']') {
                self.advance();
                return Ok(serde_json::Value::Array(items));
            }
            items.push(self.parse_value()?);
            self.skip_ws_and_comments();
            match self.peek() {
                Some(b',') => {
                    self.advance();
                    continue;
                }
                Some(b']') => {
                    self.advance();
                    return Ok(serde_json::Value::Array(items));
                }
                Some(other) => {
                    return Err(format!(
                        "expected `,` or `]` inside array literal, got `{}`",
                        other as char
                    ));
                }
                None => {
                    return Err("unexpected end of input inside array literal".to_string());
                }
            }
        }
    }

    fn parse_string_literal(&mut self, quote: u8) -> Result<serde_json::Value, String> {
        self.advance(); // opening quote
        if self.peek() == Some(b'<') && self.bytes.get(self.pos + 1) == Some(&b'<') {
            return self.parse_quoted_heredoc_literal(quote);
        }
        let mut out = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated string literal".to_string()),
                Some(b) if b == quote => return Ok(serde_json::Value::String(out)),
                Some(b'\\') => {
                    let esc = self
                        .advance()
                        .ok_or("unterminated escape sequence in string literal")?;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'0' => out.push('\0'),
                        b'\\' => out.push('\\'),
                        b'\'' => out.push('\''),
                        b'"' => out.push('"'),
                        b'`' => out.push('`'),
                        b'\n' => { /* line continuation — drop */ }
                        b'u' => {
                            // \uXXXX or \u{XXXXX}
                            let (ch, consumed) = parse_unicode_escape(&self.bytes[self.pos..])
                                .ok_or("invalid \\u escape in string literal")?;
                            out.push(ch);
                            self.pos += consumed;
                        }
                        b'x' => {
                            if self.pos + 2 > self.bytes.len() {
                                return Err("invalid \\x escape in string literal".to_string());
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 2])
                                .map_err(|_| "invalid \\x escape".to_string())?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| "invalid \\x escape".to_string())?;
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                self.pos += 2;
                            } else {
                                return Err("invalid \\x code point".to_string());
                            }
                        }
                        other => out.push(other as char),
                    }
                }
                Some(b) => {
                    // A literal newline inside a double/single quote is a TS
                    // syntax error. We accept it anyway so weaker models that
                    // forget the heredoc/template-literal rule still get their
                    // content through rather than silently dropping the call.
                    self.push_scalar(&mut out, b);
                }
            }
        }
    }

    /// Recover malformed `"content": "<<EOF ... EOF` values by treating the
    /// quoted heredoc opener as intent to write a heredoc string rather than a
    /// normal string literal. Models commonly forget to drop the opening quote
    /// before `<<EOF`, and often omit the closing quote entirely.
    fn parse_quoted_heredoc_literal(&mut self, quote: u8) -> Result<serde_json::Value, String> {
        let value = self.parse_heredoc()?;
        if self.peek() == Some(quote) {
            self.advance();
        }
        Ok(value)
    }

    fn parse_template_literal(&mut self) -> Result<serde_json::Value, String> {
        self.advance(); // opening backtick
        let mut out = String::new();
        loop {
            match self.advance() {
                None => return Err("unterminated template literal".to_string()),
                Some(b'`') => return Ok(serde_json::Value::String(out)),
                Some(b'\\') => {
                    let esc = self
                        .advance()
                        .ok_or("unterminated escape in template literal")?;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'\\' => out.push('\\'),
                        b'`' => out.push('`'),
                        b'$' => out.push('$'),
                        b'\n' => { /* line continuation — drop */ }
                        other => {
                            out.push('\\');
                            out.push(other as char);
                        }
                    }
                }
                Some(b'$') if self.peek() == Some(b'{') => {
                    // Template literal interpolation. Tool arguments never
                    // evaluate expressions; pass through the literal text.
                    out.push('$');
                    out.push('{');
                    self.advance();
                    let mut depth = 1usize;
                    while depth > 0 {
                        match self.advance() {
                            None => {
                                return Err(
                                    "unterminated ${{...}} interpolation in template literal"
                                        .to_string(),
                                );
                            }
                            Some(b'{') => {
                                depth += 1;
                                out.push('{');
                            }
                            Some(b'}') => {
                                depth -= 1;
                                out.push('}');
                            }
                            Some(b) => self.push_scalar(&mut out, b),
                        }
                    }
                }
                Some(b) => {
                    self.push_scalar(&mut out, b);
                }
            }
        }
    }

    /// Parse a heredoc string: `<<TAG\n...\nTAG`
    ///
    /// The tag is any sequence of uppercase letters/digits/underscore (e.g. EOF,
    /// END, CONTENT). Content between the opening tag line and a closing line
    /// that starts with the tag is returned raw — no escaping of any kind is
    /// needed inside. Closing punctuation may follow the tag on that same line,
    /// so tightly-collapsed tails like `EOF },` still parse correctly. This
    /// makes heredocs ideal for multiline code that contains backticks, quotes,
    /// or backslashes (Go raw strings, shell scripts, YAML, etc.).
    fn parse_heredoc(&mut self) -> Result<serde_json::Value, String> {
        // Both `<<'EOF'`/`<<"EOF"` quoting and the close-line word-boundary
        // rule live in the shared `scan_heredoc` authority; anything after the
        // tag on the closing line is left for the outer parser by rewinding
        // `self.pos` to right after the tag.
        match scan_heredoc(self.text, self.pos) {
            Ok(span) => {
                let raw = &self.text[span.content];
                let content = if span.escaped {
                    // Degraded literal-`\n` form: the body is JSON/string-escaped
                    // on one physical line. Dispatch the call with a real body
                    // (non-fatal — strong models emit clean heredocs and never
                    // reach this branch). Surfaced to telemetry via tracing.
                    tracing::debug!(
                        target: "harn::tool_parse",
                        "recovered JSON-escaped heredoc body (literal \\n line breaks)"
                    );
                    unescape_heredoc_body(raw)
                } else {
                    raw.to_string()
                };
                self.pos = span.end;
                Ok(serde_json::Value::String(content))
            }
            Err(HeredocError::MissingTag) => {
                Err("heredoc requires a tag after << (e.g. <<EOF)".to_string())
            }
            Err(HeredocError::MissingNewline { tag }) => {
                Err(format!("expected newline after heredoc tag <<{tag}"))
            }
            Err(HeredocError::Unterminated { tag }) => Err(format!(
                "unterminated heredoc: expected closing {tag} at the start of a line"
            )),
        }
    }

    fn parse_boolean(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("true") {
            self.pos += 4;
            Ok(serde_json::Value::Bool(true))
        } else if self.text[self.pos..].starts_with("false") {
            self.pos += 5;
            Ok(serde_json::Value::Bool(false))
        } else {
            Err("expected `true` or `false`".to_string())
        }
    }

    fn parse_null(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("null") {
            self.pos += 4;
            Ok(serde_json::Value::Null)
        } else {
            Err("expected `null`".to_string())
        }
    }

    fn parse_undefined(&mut self) -> Result<serde_json::Value, String> {
        if self.text[self.pos..].starts_with("undefined") {
            self.pos += 9;
            Ok(serde_json::Value::Null)
        } else {
            Err("expected `undefined`".to_string())
        }
    }

    fn parse_number(&mut self) -> Result<serde_json::Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' || b == b'+' || b == b'-' {
                self.advance();
            } else {
                break;
            }
        }
        let slice = &self.text[start..self.pos];
        if let Ok(n) = slice.parse::<i64>() {
            return Ok(serde_json::json!(n));
        }
        if let Ok(n) = slice.parse::<f64>() {
            return serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| "non-finite number literal".to_string());
        }
        Err(format!("invalid number literal `{slice}`"))
    }
}

/// Parse a `\uXXXX` or `\u{XXXXXX}` escape starting at bytes[0]. Returns the
/// decoded character AND the number of bytes consumed after the `\u`.
fn parse_unicode_escape(bytes: &[u8]) -> Option<(char, usize)> {
    if bytes.first() == Some(&b'{') {
        // \u{XXXXXX}
        let close = bytes.iter().position(|&b| b == b'}')?;
        let hex = std::str::from_utf8(&bytes[1..close]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        Some((char::from_u32(code)?, close + 1))
    } else if bytes.len() >= 4 {
        let hex = std::str::from_utf8(&bytes[..4]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        Some((char::from_u32(code)?, 4))
    } else {
        None
    }
}
