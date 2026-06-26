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
        let value = self.parse_primary_value()?;
        // String concatenation (`"a" + "b"` / `` `a` + "b" ``). Value models —
        // notably Go, where backtick struct tags force JS-style concat like
        // `` `...` + "`json:\"x\"`" + `...` `` — split a single code body into
        // `+`-joined string/template fragments. TS would evaluate that to one
        // string; tool arguments never evaluate expressions, so collapse the
        // fragments into one string value here. Engages ONLY when the first
        // operand already parsed as a string AND a `+` follows, so non-string
        // values and well-formed single strings are untouched. Scoped to
        // tool-call argument parsing — `TsValueParser` is private to the tools
        // module and never parses general documents.
        if matches!(value, serde_json::Value::String(_)) {
            return self.fold_string_concatenation(value);
        }
        Ok(value)
    }

    /// Parse one primary value (object, array, string/template, heredoc,
    /// keyword, or number) without considering a trailing `+` concatenation.
    fn parse_primary_value(&mut self) -> Result<serde_json::Value, String> {
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
            // Leading-dot decimals (`.100`) are not valid JSON/TS, but weak value
            // models (notably OpenRouter DeepSeek) emit them inside tool-call
            // argument objects. Recover `.NNN` as `0.NNN`. Scoped to tool-call
            // argument parsing only — `TsValueParser` is private to the tools
            // module and is never used for general document parsing.
            b'.' if self.bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit) => {
                self.parse_number()
            }
            other => Err(format!(
                "unexpected character `{}` starting a value",
                other as char
            )),
        }
    }

    /// Given an already-parsed leading string `head`, fold any `+`-joined
    /// trailing string/template fragments into it. Each `+` must be followed
    /// (after whitespace/comments) by another string-producing primary; if the
    /// right operand is NOT a string the input is malformed concatenation
    /// (e.g. `"a" + 1` or `"a" +` at EOF) and we error rather than guess. A
    /// recovered concat collapses to one `String` value — canonicalized back to
    /// a single quoted string on replay by `render_canonical_call`.
    fn fold_string_concatenation(
        &mut self,
        head: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let serde_json::Value::String(mut acc) = head else {
            return Ok(head);
        };
        loop {
            // Peek past whitespace/comments without committing: only a `+`
            // continues the concatenation. Anything else is the next structural
            // token (`,`, `}`, `]`, `)`), which the caller handles.
            let save = self.pos;
            self.skip_ws_and_comments();
            if self.peek() != Some(b'+') {
                self.pos = save;
                return Ok(serde_json::Value::String(acc));
            }
            self.advance(); // consume '+'
            self.skip_ws_and_comments();
            let rhs = self.parse_primary_value()?;
            match rhs {
                serde_json::Value::String(s) => acc.push_str(&s),
                other => {
                    return Err(format!(
                        "expected a string fragment after `+` in string concatenation, got `{other}`"
                    ));
                }
            }
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
            // disallow it to keep the contract explicit. Accept `=` as a synonym
            // for `:`: value models (especially before a heredoc/template body,
            // e.g. `{ new_body= <<EOF ... }`) reach for the assignment glyph they
            // know from kwargs/struct-literal syntax. `=` is not a legal
            // object-literal separator in TS, so there is no ambiguity to lose —
            // canonicalized back to `:` on replay by `render_canonical_call`.
            if !matches!(self.peek(), Some(b':') | Some(b'=')) {
                return Err(format!(
                    "expected `:` after key `{key}` inside object literal"
                ));
            }
            self.advance();
            self.skip_ws_and_comments();
            let value_start = self.pos;
            let value_first = self.peek();
            let value = self.parse_value()?;
            self.skip_ws_and_comments();
            // Recover an object string value that closed early on an embedded
            // unescaped quote — e.g. a Rust raw string `r#"..."#` inside a
            // `content` body the model forgot to escape. The strict scan stops
            // at the first bare quote, leaving the object continuation pointing
            // at content (here `#`) instead of `,`/`}`. Re-scan greedily,
            // absorbing embedded quotes until the continuation validates, so the
            // call stays dispatchable instead of being dropped. Fires ONLY when
            // the strict parse already failed its continuation, so a well-formed
            // value is never reinterpreted (same philosophy as heredoc
            // recovery — keep the model's intent, never silently drop it).
            let value = if matches!(value, serde_json::Value::String(_))
                && matches!(value_first, Some(b'"') | Some(b'\''))
                && !matches!(self.peek(), Some(b',') | Some(b'}'))
            {
                match self.recover_overclosed_object_string(value_start, value_first.unwrap()) {
                    Some(recovered) => {
                        self.skip_ws_and_comments();
                        serde_json::Value::String(recovered)
                    }
                    None => value,
                }
            } else {
                value
            };
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

    /// Re-scan an object string value that the strict pass closed too early
    /// because the model left an embedded quote unescaped (the canonical case:
    /// a Rust raw string `r#"..."#`, or any nested quote, inside a `content`
    /// body). Starting at the opening quote, mirror `parse_string_literal`'s
    /// escape handling, but at each *unescaped* closing quote, peek the
    /// continuation: a `,` or `}` (after whitespace) is the value's true end;
    /// anything else means the quote is content — absorb it and keep scanning.
    /// Returns the recovered string and advances `self.pos` past the true close
    /// only on success; on EOF-without-valid-continuation it leaves `self.pos`
    /// untouched and returns `None` so the caller reports the original error.
    /// Because it engages only after the strict continuation already failed, it
    /// can never change a value that parsed cleanly.
    fn recover_overclosed_object_string(
        &mut self,
        value_start: usize,
        quote: u8,
    ) -> Option<String> {
        let mut pos = value_start + 1; // past the opening quote
        let mut out = String::new();
        while let Some(&b) = self.bytes.get(pos) {
            if b == b'\\' {
                pos += 1;
                let &esc = self.bytes.get(pos)?;
                pos += 1;
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
                    b'u' => match parse_unicode_escape(&self.bytes[pos..]) {
                        UnicodeEscape::Char(ch, consumed) => {
                            out.push(ch);
                            pos += consumed;
                        }
                        // An unpaired surrogate aborts recovery (return None) so
                        // the caller reports the original error rather than
                        // emitting a half-character.
                        UnicodeEscape::InvalidScalar => return None,
                        // Not a complete escape: keep `\u` literal, same as the
                        // strict pass — never drop content.
                        UnicodeEscape::NotEscape => out.push_str("\\u"),
                    },
                    b'x' => match parse_hex_escape(&self.bytes[pos..]) {
                        Some((ch, consumed)) => {
                            out.push(ch);
                            pos += consumed;
                        }
                        None => out.push_str("\\x"),
                    },
                    // Unknown escape: keep the backslash AND the char. See the
                    // `parse_string_literal` arm for the rationale — preserving
                    // `\d`/`\w`/`\b` (regex), `\begin` (LaTeX), `\section`, etc.
                    other => {
                        out.push('\\');
                        out.push(other as char);
                    }
                }
            } else if b == quote {
                // Candidate close: is the continuation a valid object boundary?
                let mut look = pos + 1;
                while matches!(self.bytes.get(look), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                    look += 1;
                }
                if matches!(self.bytes.get(look), Some(b',' | b'}')) {
                    // Narrow safety net: recovers the canonical `r#"..."#`-style
                    // case where the embedded quote is NOT followed by `,`/`}`.
                    // It cannot recover a body whose embedded quote IS followed
                    // by `,`/`}` (e.g. `vec!["a", "b"]`) — that ambiguity is only
                    // resolved by the model using the escape-free heredoc body.
                    // Trace firings so the real-world hit rate is observable.
                    tracing::debug!(
                        target: "harn::tool_parse",
                        "recovered object string value with embedded unescaped quote(s)"
                    );
                    self.pos = pos + 1;
                    return Some(out);
                }
                // Embedded quote — absorb as literal content and keep scanning.
                out.push(quote as char);
                pos += 1;
            } else if b < 0x80 {
                out.push(b as char);
                pos += 1;
            } else {
                let ch = self.text[pos..].chars().next().unwrap_or('\u{FFFD}');
                out.push(ch);
                pos += ch.len_utf8();
            }
        }
        None
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
                        b'u' => match parse_unicode_escape(&self.bytes[self.pos..]) {
                            // \uXXXX or \u{XXXXX}
                            UnicodeEscape::Char(ch, consumed) => {
                                out.push(ch);
                                self.pos += consumed;
                            }
                            UnicodeEscape::InvalidScalar => {
                                return Err("invalid \\u escape in string literal".to_string());
                            }
                            // Not a complete escape (`\users`, `\uAB`, `\uABCG`):
                            // keep `\u` literal like any unknown escape rather than
                            // dropping the whole call. Matches the heredoc and
                            // template-literal channels byte-for-byte.
                            UnicodeEscape::NotEscape => out.push_str("\\u"),
                        },
                        b'x' => match parse_hex_escape(&self.bytes[self.pos..]) {
                            Some((ch, consumed)) => {
                                out.push(ch);
                                self.pos += consumed;
                            }
                            // Not a complete `\xHH` escape (Perl `\x{1F600}`, a
                            // trailing `\x`, `\x` + non-hex): keep `\x` literal so
                            // the call still dispatches with the model's content,
                            // identical to the heredoc/template channels.
                            None => out.push_str("\\x"),
                        },
                        // Unknown escape (`\d`, `\w`, `\b`, `\s` regex; `\begin`
                        // LaTeX; `\section`; a Windows path's `\U`): keep BOTH the
                        // backslash and the char. JS would collapse `\d` to `d`,
                        // but tool arguments are content, not evaluated JS — the
                        // model means the literal `\d`, and the template-literal,
                        // heredoc, and native-JSON channels all preserve it. The
                        // bare double/single-quoted channel silently dropping the
                        // backslash was a cross-channel inconsistency that
                        // corrupted every regex/LaTeX/format-string the model
                        // delivered through quotes (`"\d+"` -> `d+`).
                        other => {
                            out.push('\\');
                            self.push_scalar(&mut out, other);
                        }
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
        // Recover leading-dot decimals (`.100`, `-.5`) into canonical `0.100` /
        // `-0.5` so an otherwise-valid tool call isn't dropped over one char.
        // Scoped to tool-call argument parsing only: `TsValueParser` is private
        // to the tools module and never parses general documents.
        let normalized: std::borrow::Cow<'_, str> = if let Some(rest) = slice.strip_prefix('.') {
            format!("0.{rest}").into()
        } else if let Some(rest) = slice.strip_prefix("-.") {
            format!("-0.{rest}").into()
        } else {
            slice.into()
        };
        if let Ok(n) = normalized.parse::<i64>() {
            return Ok(serde_json::json!(n));
        }
        if let Ok(n) = normalized.parse::<f64>() {
            return serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| "non-finite number literal".to_string());
        }
        Err(format!("invalid number literal `{slice}`"))
    }
}

/// Outcome of reading a `\u` escape, given the bytes that follow the `\u`.
///
/// The three cases drive the caller's never-drop-the-call contract: a complete,
/// valid escape decodes; a complete-but-invalid one (unpaired surrogate, out of
/// range) is rejected so we never emit a half-character; and anything that is
/// not even a complete `\u` escape is kept as the literal text `\u…` rather than
/// dropping the entire tool call.
enum UnicodeEscape {
    /// Decoded scalar plus the number of bytes consumed after `\u`.
    Char(char, usize),
    /// Syntactically a complete `\uHHHH` / `\u{HHHH}` escape, but the code point
    /// is not a valid scalar (an unpaired surrogate, or out of range). The model
    /// clearly intended a unicode escape, so reject rather than silently emit a
    /// half-character — this keeps the lone-surrogate guard.
    InvalidScalar,
    /// Not a syntactically complete `\u` escape (`\u` not followed by four hex
    /// digits or a `{HHHH}` group — e.g. a Windows path `\users`, a short
    /// `\uAB`, or `\uABCG`). Treat `\u` as literal content; never drop the call.
    NotEscape,
}

/// Classify a `\u` escape from the bytes following the `\u` prefix.
///
/// The plain 4-hex `\uXXXX` form decodes a single UTF-16 code unit, so a
/// non-BMP scalar (emoji, CJK extension B+, math alphanumerics) arrives as a
/// **surrogate pair** `😀` — exactly what `JSON.stringify` and most
/// provider APIs emit. Detect a high surrogate and fold the following `\uXXXX`
/// low surrogate into the astral scalar, consuming its `\u` prefix too. The
/// `\u{...}` brace form already carries the full scalar.
fn parse_unicode_escape(bytes: &[u8]) -> UnicodeEscape {
    if bytes.first() == Some(&b'{') {
        // \u{XXXXXX}. Missing close brace or non-hex contents is not a complete
        // escape — keep the `\u` literal rather than dropping the whole call.
        let Some(close) = bytes.iter().position(|&b| b == b'}') else {
            return UnicodeEscape::NotEscape;
        };
        let Some(code) = std::str::from_utf8(&bytes[1..close])
            .ok()
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        else {
            return UnicodeEscape::NotEscape;
        };
        match char::from_u32(code) {
            Some(ch) => UnicodeEscape::Char(ch, close + 1),
            None => UnicodeEscape::InvalidScalar,
        }
    } else if bytes.len() >= 4 && bytes[..4].iter().all(u8::is_ascii_hexdigit) {
        let code = u32::from_str_radix(std::str::from_utf8(&bytes[..4]).expect("ascii"), 16)
            .expect("four hex digits");
        if let Some(ch) = char::from_u32(code) {
            return UnicodeEscape::Char(ch, 4);
        }
        // `code` is a surrogate (0xD800..=0xDFFF), invalid as a lone scalar.
        // If it is a high surrogate followed by `\uXXXX` low surrogate, combine
        // them into the astral code point. The trailing `\u` (2 bytes) plus its
        // 4 hex digits are consumed in addition to the first form's 4.
        if (0xD800..=0xDBFF).contains(&code)
            && bytes.get(4) == Some(&b'\\')
            && bytes.get(5) == Some(&b'u')
            && bytes.len() >= 10
            && bytes[6..10].iter().all(u8::is_ascii_hexdigit)
        {
            let low = u32::from_str_radix(std::str::from_utf8(&bytes[6..10]).expect("ascii"), 16)
                .expect("four hex digits");
            if (0xDC00..=0xDFFF).contains(&low) {
                let scalar = 0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                if let Some(ch) = char::from_u32(scalar) {
                    return UnicodeEscape::Char(ch, 10);
                }
            }
        }
        // Complete `\uHHHH` syntax, but an unpaired / unmatched surrogate.
        UnicodeEscape::InvalidScalar
    } else {
        // `\u` not followed by four hex digits or `{...}`: literal `\u`.
        UnicodeEscape::NotEscape
    }
}

/// Decode a `\xHH` escape (exactly two hex digits) from the bytes following the
/// `\x` prefix. Returns the scalar and bytes consumed (always 2) when both
/// following bytes are hex; otherwise `None` so the caller keeps `\x` literal
/// instead of dropping the whole call. Every `0x00..=0xFF` value is a valid
/// scalar, so there is no invalid-code-point case here. This keeps content like
/// Perl `\x{1F600}`, a trailing `\x`, or `\x` + non-hex byte-identical to the
/// heredoc and template-literal channels.
fn parse_hex_escape(bytes: &[u8]) -> Option<(char, usize)> {
    let pair = bytes.get(..2)?;
    if !pair.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let code = u32::from_str_radix(std::str::from_utf8(pair).expect("ascii"), 16).expect("two hex");
    char::from_u32(code).map(|ch| (ch, 2))
}
