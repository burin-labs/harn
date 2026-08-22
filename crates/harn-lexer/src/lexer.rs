use crate::token::*;
use std::fmt;

mod positioned;

/// Lexer errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexerError {
    UnexpectedCharacter(char, Span),
    UnterminatedString(Span),
    UnterminatedBlockComment(Span),
    /// An integer literal outside `i64`, reported instead of becoming a lossy float.
    IntegerLiteralOutOfRange(String, Span),
}

impl LexerError {
    /// The source span the error points at.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            LexerError::UnexpectedCharacter(_, span)
            | LexerError::UnterminatedString(span)
            | LexerError::UnterminatedBlockComment(span)
            | LexerError::IntegerLiteralOutOfRange(_, span) => *span,
        }
    }
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexerError::UnexpectedCharacter(ch, span) => {
                write!(f, "Unexpected character '{ch}' at {span}")
            }
            LexerError::UnterminatedString(span) => {
                write!(f, "Unterminated string at {span}")
            }
            LexerError::UnterminatedBlockComment(span) => {
                write!(f, "Unterminated block comment at {span}")
            }
            LexerError::IntegerLiteralOutOfRange(lit, span) => {
                write!(
                    f,
                    "Integer literal `{lit}` is out of range for int (i64) at {span}"
                )
            }
        }
    }
}

impl std::error::Error for LexerError {}

/// Byte-indexed scanner producing tokens.
///
/// The lexer borrows the source text and walks it as UTF-8 bytes: every piece
/// of Harn syntax (operators, quotes, digits, keywords) is ASCII, so the hot
/// dispatch loop matches single bytes, and token text is carved out of the
/// source with one slice per token instead of accumulating `char`s one push at
/// a time. Non-ASCII input only ever appears inside identifiers, strings, and
/// comments; those paths decode a full `char` exactly where Unicode semantics
/// matter (identifier alphabet checks, column counting, error reporting).
/// UTF-8 continuation bytes never collide with ASCII byte comparisons, so byte
/// dispatch is safe without decoding first.
///
/// Columns count Unicode scalar values (not bytes), matching every span
/// consumer (`harn fmt`, the LSP, diagnostics); byte offsets in spans address
/// the owning file directly.
pub struct Lexer<'src> {
    src: &'src str,
    /// Byte index of the scan position within `src`.
    pos: usize,
    /// Byte offset of `src[0]` within the owning file (non-zero when re-lexing
    /// a slice, e.g. an interpolation hole).
    base: usize,
    line: usize,
    column: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(source: &'src str) -> Self {
        Self {
            src: source,
            pos: 0,
            base: 0,
            line: 1,
            column: 1,
        }
    }

    /// Tokenize source code, including comment tokens.
    pub fn tokenize_with_comments(&mut self) -> Result<Vec<Token>, LexerError> {
        self.tokenize_inner(true)
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexerError> {
        self.tokenize_inner(false)
    }

    fn tokenize_inner(&mut self, keep_comments: bool) -> Result<Vec<Token>, LexerError> {
        // Harn source averages roughly six bytes per token; reserving that up
        // front spares the token vector its doubling copies (tokens are large
        // — a `TokenKind` plus a five-word `Span`), at the cost of one bounded
        // over-allocation on comment- or string-heavy files.
        let mut tokens = Vec::with_capacity(self.src.len() / 6 + 8);
        let bytes = self.src.as_bytes();

        // Skip a `#!` shebang line if present at the very start of the file.
        // Only valid at byte offset 0 — anywhere else, `#` is still an error.
        if self.pos == 0 && bytes.starts_with(b"#!") {
            let text = self.rest_of_line();
            self.consume_str(text);
        }

        while let Some(byte) = self.byte_at(self.pos) {
            match byte {
                b' ' | b'\t' | b'\r' => {
                    // Consume the whole run (indentation comes in blocks) in a
                    // tight loop instead of one trip through this match per
                    // byte.
                    let run = bytes[self.pos..]
                        .iter()
                        .position(|&b| !matches!(b, b' ' | b'\t' | b'\r'))
                        .unwrap_or(bytes.len() - self.pos);
                    self.pos += run;
                    self.column += run;
                    continue;
                }
                // Join LF or CRLF continuations; formatter output must parse
                // on Windows checkouts.
                b'\\'
                    if self.byte_at(self.pos + 1) == Some(b'\n')
                        || (self.byte_at(self.pos + 1) == Some(b'\r')
                            && self.byte_at(self.pos + 2) == Some(b'\n')) =>
                {
                    self.pos += if self.byte_at(self.pos + 1) == Some(b'\r') {
                        3
                    } else {
                        2
                    };
                    self.line += 1;
                    self.column = 1;
                    continue;
                }
                b'\n' => {
                    let start = self.byte_pos();
                    tokens.push(Token::with_span(
                        TokenKind::Newline,
                        Span::with_offsets(start, start + 1, self.line, self.column),
                    ));
                    self.pos += 1;
                    self.line += 1;
                    self.column = 1;
                    continue;
                }
                b'/' if self.byte_at(self.pos + 1) == Some(b'/') => {
                    let tok = self.read_line_comment();
                    if keep_comments {
                        tokens.push(tok);
                    }
                    continue;
                }
                b'/' if self.byte_at(self.pos + 1) == Some(b'*') => {
                    let tok = self.read_block_comment()?;
                    if keep_comments {
                        tokens.push(tok);
                    }
                    continue;
                }
                b'r' if self.byte_at(self.pos + 1) == Some(b'"') => {
                    tokens.push(self.read_raw_string()?);
                    continue;
                }
                // Hashed raw string: r#"..."#, r##"..."##, etc. The `#` run
                // lets the body hold literal `"` without escaping — the close
                // is the first `"` followed by the matching number of `#`.
                // Only treated as a raw string when the `#` run is actually
                // followed by `"`; otherwise we fall through (no
                // `r#`-identifier syntax in Harn).
                b'r' if self.byte_at(self.pos + 1) == Some(b'#') => {
                    if let Some(hashes) = self.raw_hash_count() {
                        tokens.push(self.read_raw_string_hashed(hashes)?);
                        continue;
                    }
                    tokens.push(self.read_identifier());
                    continue;
                }
                b'"' => {
                    tokens.push(self.read_string()?);
                    continue;
                }
                b'0'..=b'9' => {
                    tokens.push(self.read_number()?);
                    continue;
                }
                b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                    tokens.push(self.read_identifier());
                    continue;
                }
                _ => {}
            }

            // Non-ASCII bytes reach here: a Unicode-alphabetic char starts an
            // identifier, anything else is the same error the ASCII fallthrough
            // reports below.
            if byte >= 0x80 {
                let ch = self.char_at(self.pos).expect("pos is a char boundary");
                if ch.is_alphabetic() {
                    tokens.push(self.read_identifier());
                    continue;
                }
                return Err(self.unexpected_character(ch));
            }

            if let Some(tok) = self.try_two_char_op() {
                tokens.push(tok);
                continue;
            }

            if let Some(kind) = Self::single_char_token(byte) {
                let start = self.byte_pos();
                let col = self.column;
                self.pos += 1;
                self.column += 1;
                tokens.push(Token::with_span(
                    kind,
                    Span::with_offsets(start, self.byte_pos(), self.line, col),
                ));
                continue;
            }

            return Err(self.unexpected_character(byte as char));
        }

        tokens.push(Token::with_span(
            TokenKind::Eof,
            Span::with_offsets(self.byte_pos(), self.byte_pos(), self.line, self.column),
        ));
        Ok(tokens)
    }

    /// Absolute byte offset of the scan position within the owning file.
    #[inline]
    fn byte_pos(&self) -> usize {
        self.base + self.pos
    }

    #[inline]
    fn byte_at(&self, index: usize) -> Option<u8> {
        self.src.as_bytes().get(index).copied()
    }

    /// Decode the char starting at byte `index` (must be a char boundary).
    #[inline]
    fn char_at(&self, index: usize) -> Option<char> {
        self.src[index..].chars().next()
    }

    /// The source text from the scan position to (excluding) the next `\n`.
    #[inline]
    fn rest_of_line(&self) -> &'src str {
        let rest = &self.src[self.pos..];
        match rest.as_bytes().iter().position(|&b| b == b'\n') {
            Some(len) => &rest[..len],
            None => rest,
        }
    }

    /// Advance past `text` (which must be the source at the scan position),
    /// bumping the column once per char. Must not contain newlines.
    #[inline]
    fn consume_str(&mut self, text: &str) {
        self.pos += text.len();
        self.column += char_count(text);
    }

    /// Advance over one char, bumping the column once. Returns the char.
    #[inline]
    fn bump_char(&mut self) -> Option<char> {
        let ch = self.char_at(self.pos)?;
        self.pos += ch.len_utf8();
        self.column += 1;
        Some(ch)
    }

    fn unexpected_character(&self, ch: char) -> LexerError {
        LexerError::UnexpectedCharacter(
            ch,
            Span::with_offsets(
                self.byte_pos(),
                self.byte_pos() + ch.len_utf8(),
                self.line,
                self.column,
            ),
        )
    }

    fn read_line_comment(&mut self) -> Token {
        let start_byte = self.byte_pos();
        let start_col = self.column;
        let start_line = self.line;
        self.pos += 2; // `//`
        self.column += 2;
        // `///foo` is a doc comment, but `////foo` (a separator bar) is not.
        let is_doc =
            self.byte_at(self.pos) == Some(b'/') && self.byte_at(self.pos + 1) != Some(b'/');
        if is_doc {
            self.pos += 1;
            self.column += 1;
        }
        let text = self.rest_of_line();
        self.consume_str(text);
        Token::with_span(
            TokenKind::LineComment {
                text: text.to_string(),
                is_doc,
            },
            Span::with_offsets(start_byte, self.byte_pos(), start_line, start_col),
        )
    }

    fn read_block_comment(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos();
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);
        self.pos += 2; // `/*`
        self.column += 2;
        // `/** ... */` is a doc comment, but `/*** */` and `/**/` are not.
        let is_doc = self.byte_at(self.pos) == Some(b'*')
            && self.byte_at(self.pos + 1) != Some(b'*')
            && self.byte_at(self.pos + 1) != Some(b'/');
        if is_doc {
            self.pos += 1;
            self.column += 1;
        }
        // The comment text is the contiguous source between the (optional doc
        // marker after the) opening delimiter and the closing `*/` — nested
        // delimiters stay in the text — so it can be sliced instead of built.
        let text_start = self.pos;
        let mut text_end = None;
        let mut depth = 1u32;
        while let Some(byte) = self.byte_at(self.pos) {
            match byte {
                b'/' if self.byte_at(self.pos + 1) == Some(b'*') => {
                    depth += 1;
                    self.pos += 2;
                    self.column += 2;
                }
                b'*' if self.byte_at(self.pos + 1) == Some(b'/') => {
                    depth -= 1;
                    if depth == 0 {
                        text_end = Some(self.pos);
                    }
                    self.pos += 2;
                    self.column += 2;
                    if depth == 0 {
                        break;
                    }
                }
                b'\n' => {
                    self.pos += 1;
                    self.line += 1;
                    self.column = 1;
                }
                _ => {
                    self.bump_char();
                }
            }
        }
        let Some(text_end) = text_end else {
            return Err(LexerError::UnterminatedBlockComment(start));
        };
        // The span must point at the opening `/*` (line/column of `start`),
        // with `end_line` at the closing `*/` — mirroring how multi-line
        // strings record their span. Using `self.line` for the start line
        // here would report the comment's *end* line, which misplaces
        // multi-line block comments for every span consumer (`harn fmt`,
        // the LSP, diagnostics).
        let mut span = Span::with_offsets(start_byte, self.byte_pos(), start.line, start.column);
        span.end_line = self.line;
        Ok(Token::with_span(
            TokenKind::BlockComment {
                text: self.src[text_start..text_end].to_string(),
                is_doc,
            },
            span,
        ))
    }

    /// Capture the raw source text of an interpolation hole `${ ... }`.
    ///
    /// Precondition: the lexer is positioned on the `$` of `${`. This consumes
    /// the opening `${`, the hole's expression text, and the matching closing
    /// `}`, returning the captured expression along with the line/column where
    /// it began (used to anchor diagnostics when the hole is re-lexed).
    ///
    /// The scan is string-literal aware: a `}` or `{` that appears inside a
    /// nested `"..."` string literal does not change brace depth, and a `\`
    /// inside such a literal escapes the next character (so `\"` does not close
    /// the nested string). This lets a hole contain string literals with braces
    /// or quotes, e.g. `${ items["a}b"] }` or `${ x ?? "default" }`, instead of
    /// terminating early at the first inner `}`.
    fn capture_interpolation_expr(
        &mut self,
        start: Span,
    ) -> Result<(String, usize, usize), LexerError> {
        self.pos += 2; // `${`
        self.column += 2;
        let expr_line = self.line;
        let expr_col = self.column;
        let expr_start = self.pos;
        let mut depth = 1usize;
        let mut in_string = false;
        loop {
            let Some(byte) = self.byte_at(self.pos) else {
                return Err(LexerError::UnterminatedString(start));
            };
            if in_string {
                match byte {
                    b'\\' => {
                        // Skip the backslash and the escaped char verbatim; an
                        // escaped quote must not close the nested string
                        // literal.
                        self.pos += 1;
                        self.column += 1;
                        match self.bump_char() {
                            Some('\n') => {
                                self.line += 1;
                                self.column = 1;
                            }
                            Some(_) => {}
                            None => return Err(LexerError::UnterminatedString(start)),
                        }
                        continue;
                    }
                    b'"' => in_string = false,
                    _ => {}
                }
            } else {
                match byte {
                    // A backslash is never valid in expression position. The
                    // usual cause is escaping the quotes of a nested string
                    // literal (`${x ?? \"y\"}`); inside an interpolation hole,
                    // string literals use bare double quotes (`${x ?? "y"}`).
                    // Report it precisely here rather than scanning to EOF and
                    // surfacing a misleading "unterminated string".
                    b'\\' => {
                        return Err(LexerError::UnexpectedCharacter(
                            '\\',
                            Span::with_offsets(
                                self.byte_pos(),
                                self.byte_pos() + 1,
                                self.line,
                                self.column,
                            ),
                        ));
                    }
                    b'"' => in_string = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if byte == b'\n' {
                self.pos += 1;
                self.line += 1;
                self.column = 1;
            } else {
                self.bump_char();
            }
        }
        let expr = &self.src[expr_start..self.pos];
        if expr.trim().is_empty() {
            return Err(LexerError::UnexpectedCharacter(
                '}',
                Span::with_offsets(self.byte_pos(), self.byte_pos() + 1, self.line, self.column),
            ));
        }
        self.pos += 1; // closing `}`
        self.column += 1;
        Ok((expr.to_string(), expr_line, expr_col))
    }

    /// Append the run of ordinary content bytes starting at the scan position
    /// to `value` in one `push_str`, stopping before any byte in `stop` (or a
    /// backslash / newline, which every string form treats specially). Returns
    /// whether any progress was made.
    #[inline]
    fn take_plain_run(&mut self, value: &mut String, stop: &[u8]) -> bool {
        let bytes = &self.src.as_bytes()[self.pos..];
        let len = bytes
            .iter()
            .position(|b| stop.contains(b) || *b == b'\\' || *b == b'\n')
            .unwrap_or(bytes.len());
        if len == 0 {
            return false;
        }
        let run = &self.src[self.pos..self.pos + len];
        value.push_str(run);
        self.consume_str(run);
        true
    }

    fn read_string(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos();
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);

        if self.byte_at(self.pos + 1) == Some(b'"') && self.byte_at(self.pos + 2) == Some(b'"') {
            return self.read_multi_line_string(start_byte, start);
        }

        self.pos += 1; // opening quote
        self.column += 1;

        let mut value = String::new();
        let mut segments: Vec<StringSegment> = Vec::new();
        let mut has_interpolation = false;

        loop {
            self.take_plain_run(&mut value, &[b'"', b'$']);
            let Some(byte) = self.byte_at(self.pos) else {
                return Err(LexerError::UnterminatedString(start));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    self.column += 1;
                    let span =
                        Span::with_offsets(start_byte, self.byte_pos(), start.line, start.column);
                    if has_interpolation {
                        if !value.is_empty() {
                            segments.push(StringSegment::Literal(value));
                        }
                        return Ok(Token::with_span(
                            TokenKind::InterpolatedString(segments),
                            span,
                        ));
                    }
                    return Ok(Token::with_span(TokenKind::StringLiteral(value), span));
                }
                b'$' if self.byte_at(self.pos + 1) == Some(b'{') => {
                    has_interpolation = true;
                    if !value.is_empty() {
                        segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                    }
                    let (expr, expr_line, expr_col) = self.capture_interpolation_expr(start)?;
                    segments.push(StringSegment::Expression(expr, expr_line, expr_col));
                }
                b'$' => {
                    value.push('$');
                    self.pos += 1;
                    self.column += 1;
                }
                b'\\' => {
                    self.pos += 1;
                    self.column += 1;
                    let Some(escaped) = self.bump_char() else {
                        return Err(LexerError::UnterminatedString(start));
                    };
                    match escaped {
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        '0' => value.push('\0'),
                        '\\' => value.push('\\'),
                        '"' => value.push('"'),
                        '$' => value.push('$'),
                        _ => {
                            value.push('\\');
                            value.push(escaped);
                        }
                    }
                }
                b'\n' => return Err(LexerError::UnterminatedString(start)),
                _ => unreachable!("take_plain_run stops only on delimiter bytes"),
            }
        }
    }

    fn read_multi_line_string(
        &mut self,
        start_byte: usize,
        start: Span,
    ) -> Result<Token, LexerError> {
        self.pos += 3; // opening `"""`
        self.column += 3;

        if self.byte_at(self.pos) == Some(b'\n') {
            self.pos += 1;
            self.line += 1;
            self.column = 1;
        }

        let mut value = String::new();
        let mut segments: Vec<StringSegment> = Vec::new();
        let mut has_interpolation = false;

        loop {
            self.take_plain_run(&mut value, &[b'"', b'$']);
            let Some(byte) = self.byte_at(self.pos) else {
                return Err(LexerError::UnterminatedString(start));
            };
            match byte {
                b'"' if self.byte_at(self.pos + 1) == Some(b'"')
                    && self.byte_at(self.pos + 2) == Some(b'"') =>
                {
                    self.pos += 3;
                    self.column += 3;
                    if has_interpolation {
                        if !value.is_empty() {
                            segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                        }
                        // Strip the common indent across all literal segments together so
                        // interpolation boundaries don't produce uneven dedenting.
                        let full_text: String = segments
                            .iter()
                            .map(|seg| match seg {
                                StringSegment::Literal(s) => s.as_str(),
                                _ => "",
                            })
                            .collect();
                        let indent = common_indent(&full_text);
                        let segments = if indent > 0 {
                            let stripped_segments = segments
                                .into_iter()
                                .map(|seg| match seg {
                                    StringSegment::Literal(s) => {
                                        StringSegment::Literal(strip_indent(&s, indent))
                                    }
                                    other => other,
                                })
                                .collect();
                            strip_trailing_newline_segments(stripped_segments)
                        } else {
                            strip_trailing_newline_segments(segments)
                        };
                        let mut span = Span::with_offsets(
                            start_byte,
                            self.byte_pos(),
                            start.line,
                            start.column,
                        );
                        span.end_line = self.line;
                        return Ok(Token::with_span(
                            TokenKind::InterpolatedString(segments),
                            span,
                        ));
                    }
                    let stripped = strip_common_indent(&value);
                    let mut span =
                        Span::with_offsets(start_byte, self.byte_pos(), start.line, start.column);
                    span.end_line = self.line;
                    return Ok(Token::with_span(TokenKind::StringLiteral(stripped), span));
                }
                b'$' if self.byte_at(self.pos + 1) == Some(b'{') => {
                    has_interpolation = true;
                    if !value.is_empty() {
                        segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                    }
                    let (expr, expr_line, expr_col) = self.capture_interpolation_expr(start)?;
                    segments.push(StringSegment::Expression(expr, expr_line, expr_col));
                }
                b'\\'
                    if self.byte_at(self.pos + 1) == Some(b'$')
                    && self.byte_at(self.pos + 2) == Some(b'{')
                    // Only an unpaired backslash escapes `${`; pairs remain
                    // literal text.
                    && value.chars().rev().take_while(|ch| *ch == '\\').count() % 2 == 0 =>
                {
                    self.pos += 2; // `\$`
                    self.column += 2;
                    value.push('$');
                }
                b'\n' => {
                    value.push('\n');
                    self.pos += 1;
                    self.line += 1;
                    self.column = 1;
                }
                // A `"` short of `"""`, a bare `$`, or a `\` that does not
                // escape `${`: all literal text.
                _ => {
                    value.push(byte as char);
                    self.pos += 1;
                    self.column += 1;
                }
            }
        }
    }

    /// Return the opening `#` count when `r` is followed by hashes and `"`.
    /// Returns `None` without advancing when the hash run has no closing `"`.
    fn raw_hash_count(&self) -> Option<usize> {
        let mut k = 1; // skip the leading `r`
        let mut hashes = 0;
        while self.byte_at(self.pos + k) == Some(b'#') {
            hashes += 1;
            k += 1;
        }
        if hashes >= 1 && self.byte_at(self.pos + k) == Some(b'"') {
            Some(hashes)
        } else {
            None
        }
    }

    /// Read a hashed raw string without escapes, interpolation, or newlines.
    /// It ends at `"` plus at least `hashes` hashes and consumes exactly that
    /// many hashes, leaving extras in the stream like Rust raw strings.
    /// Quotes lacking enough trailing hashes remain part of the body.
    fn read_raw_string_hashed(&mut self, hashes: usize) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos();
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);
        // Consume `r`, the `#` run, and the opening `"`.
        self.pos += 1 + hashes + 1;
        self.column += 1 + hashes + 1;

        let body_start = self.pos;
        while let Some(byte) = self.byte_at(self.pos) {
            match byte {
                b'"' => {
                    // A closing quote only ends the literal if followed by the
                    // matching `#` run; otherwise it is a literal quote.
                    let closed = (1..=hashes).all(|k| self.byte_at(self.pos + k) == Some(b'#'));
                    if closed {
                        let body = &self.src[body_start..self.pos];
                        self.pos += 1 + hashes; // closing quote + `#` run
                        self.column += 1 + hashes;
                        return Ok(Token::with_span(
                            TokenKind::RawStringLiteral(body.to_string()),
                            Span::with_offsets(
                                start_byte,
                                self.byte_pos(),
                                start.line,
                                start.column,
                            ),
                        ));
                    }
                    self.pos += 1;
                    self.column += 1;
                }
                b'\n' => return Err(LexerError::UnterminatedString(start)),
                _ => {
                    self.bump_char();
                }
            }
        }
        Err(LexerError::UnterminatedString(start))
    }

    /// Read a raw string `r"..."`: no escape processing, no interpolation.
    fn read_raw_string(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos();
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);
        self.pos += 2; // `r"`
        self.column += 2;

        let body_start = self.pos;
        while let Some(byte) = self.byte_at(self.pos) {
            match byte {
                b'"' => {
                    let body = &self.src[body_start..self.pos];
                    self.pos += 1;
                    self.column += 1;
                    return Ok(Token::with_span(
                        TokenKind::RawStringLiteral(body.to_string()),
                        Span::with_offsets(start_byte, self.byte_pos(), start.line, start.column),
                    ));
                }
                b'\n' => return Err(LexerError::UnterminatedString(start)),
                _ => {
                    self.bump_char();
                }
            }
        }
        Err(LexerError::UnterminatedString(start))
    }

    fn read_number(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos();
        let start_col = self.column;
        let num_start = self.pos;
        let mut is_float = false;

        while let Some(byte) = self.byte_at(self.pos) {
            match byte {
                b'0'..=b'9' => {}
                b'.' => {
                    if is_float {
                        break;
                    }
                    // Disambiguate `42.method` (method access) from `42.5`
                    // (float literal).
                    if !self
                        .byte_at(self.pos + 1)
                        .is_some_and(|next| next.is_ascii_digit())
                    {
                        break;
                    }
                    is_float = true;
                }
                _ => break,
            }
            self.pos += 1;
            self.column += 1;
        }

        let num_str = &self.src[num_start..self.pos];

        if !is_float {
            if let Some(ms) = self.try_duration_suffix(num_str) {
                return Ok(Token::with_span(
                    TokenKind::DurationLiteral(ms),
                    Span::with_offsets(start_byte, self.byte_pos(), self.line, start_col),
                ));
            }
        }

        let span = Span::with_offsets(start_byte, self.byte_pos(), self.line, start_col);
        if is_float {
            let n: f64 = num_str.parse().unwrap_or(0.0);
            Ok(Token::with_span(TokenKind::FloatLiteral(n), span))
        } else {
            match num_str.parse::<i64>() {
                Ok(n) => Ok(Token::with_span(TokenKind::IntLiteral(n), span)),
                Err(_) => {
                    // An integer literal that does not fit in i64 is a hard
                    // error, not a silent degrade to a lossy float: the spec
                    // grammar is `int_literal ::= digit+` with no documented
                    // float promotion, and silently widening loses both value
                    // (distinct literals collapse onto the same f64) and type.
                    // The sign is applied later by the parser, so the most
                    // negative i64 must be written as e.g. `-9223372036854775807 - 1`.
                    Err(LexerError::IntegerLiteralOutOfRange(
                        num_str.to_string(),
                        span,
                    ))
                }
            }
        }
    }

    /// Parse a duration suffix (ms, s, m, h, d, w) after a number, returning milliseconds.
    fn try_duration_suffix(&mut self, num_str: &str) -> Option<u64> {
        let n: u64 = num_str.parse().ok()?;
        let (suffix_len, ms) = match self.byte_at(self.pos)? {
            // `ms` shadows the `m` fallback, and a boundary miss after `ms`
            // (`1msfoo`) cannot fall back to minutes: the `m` fallback's own
            // boundary check would land on the `s` and miss too, so the whole
            // suffix is rejected — matching identifier-boundary behavior.
            b'm' if self.byte_at(self.pos + 1) == Some(b's') => (2, n),
            b's' => (1, n.saturating_mul(1000)),
            b'm' => (1, n.saturating_mul(60_000)),
            b'h' => (1, n.saturating_mul(3_600_000)),
            b'd' => (1, n.saturating_mul(86_400_000)),
            b'w' => (1, n.saturating_mul(604_800_000)),
            _ => return None,
        };
        if !self.is_duration_suffix_boundary(self.pos + suffix_len) {
            return None;
        }
        self.pos += suffix_len;
        self.column += suffix_len;
        Some(ms)
    }

    /// Whether the char starting at byte `index` (or EOF) ends a duration
    /// suffix: anything Unicode-alphanumeric or `_` continues an identifier
    /// instead.
    fn is_duration_suffix_boundary(&self, index: usize) -> bool {
        match self.char_at(index) {
            None => true,
            Some(ch) => !ch.is_alphanumeric() && ch != '_',
        }
    }

    fn read_identifier(&mut self) -> Token {
        let start_byte = self.byte_pos();
        let start_col = self.column;
        let ident_start = self.pos;

        loop {
            // Scan the ASCII identifier run in one tight pass; only a
            // non-ASCII byte falls back to per-char Unicode classification.
            let bytes = &self.src.as_bytes()[self.pos..];
            let run = bytes
                .iter()
                .position(|&b| !matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
                .unwrap_or(bytes.len());
            self.pos += run;
            self.column += run;
            match self.byte_at(self.pos) {
                Some(0x80..) => {
                    let ch = self.char_at(self.pos).expect("pos is a char boundary");
                    if !ch.is_alphanumeric() {
                        break;
                    }
                    self.pos += ch.len_utf8();
                    self.column += 1;
                }
                _ => break,
            }
        }

        let ident = &self.src[ident_start..self.pos];
        let kind =
            keyword_token_kind(ident).unwrap_or_else(|| TokenKind::Identifier(ident.to_string()));

        Token::with_span(
            kind,
            Span::with_offsets(start_byte, self.byte_pos(), self.line, start_col),
        )
    }

    fn try_two_char_op(&mut self) -> Option<Token> {
        let ch = self.byte_at(self.pos)?;
        let next = self.byte_at(self.pos + 1)?;

        let kind = match (ch, next) {
            (b'=', b'=') => TokenKind::Eq,
            (b'!', b'=') => TokenKind::Neq,
            (b'&', b'&') => TokenKind::And,
            (b'|', b'|') => TokenKind::Or,
            (b'|', b'>') => TokenKind::Pipe,
            (b'?', b'?') => TokenKind::NilCoal,
            (b'*', b'*') => TokenKind::Pow,
            (b'?', b'.') => TokenKind::QuestionDot,
            (b'-', b'>') => TokenKind::Arrow,
            (b'-', b'=') => TokenKind::MinusAssign,
            (b'+', b'=') => TokenKind::PlusAssign,
            (b'*', b'=') => TokenKind::StarAssign,
            (b'/', b'=') => TokenKind::SlashAssign,
            (b'%', b'=') => TokenKind::PercentAssign,
            (b'<', b'=') => TokenKind::Lte,
            (b'>', b'=') => TokenKind::Gte,
            _ => return None,
        };

        let start_byte = self.byte_pos();
        let col = self.column;
        self.pos += 2;
        self.column += 2;
        Some(Token::with_span(
            kind,
            Span::with_offsets(start_byte, self.byte_pos(), self.line, col),
        ))
    }

    fn single_char_token(byte: u8) -> Option<TokenKind> {
        match byte {
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'[' => Some(TokenKind::LBracket),
            b']' => Some(TokenKind::RBracket),
            b',' => Some(TokenKind::Comma),
            b':' => Some(TokenKind::Colon),
            b';' => Some(TokenKind::Semicolon),
            b'.' => Some(TokenKind::Dot),
            b'=' => Some(TokenKind::Assign),
            b'!' => Some(TokenKind::Not),
            b'+' => Some(TokenKind::Plus),
            b'-' => Some(TokenKind::Minus),
            b'*' => Some(TokenKind::Star),
            b'/' => Some(TokenKind::Slash),
            b'%' => Some(TokenKind::Percent),
            b'<' => Some(TokenKind::Lt),
            b'>' => Some(TokenKind::Gt),
            b'?' => Some(TokenKind::Question),
            b'|' => Some(TokenKind::Bar),
            b'&' => Some(TokenKind::Amp),
            b'@' => Some(TokenKind::At),
            _ => None,
        }
    }
}

/// Char count with a byte-length fast path for ASCII text.
#[inline]
fn char_count(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    }
}
/// Strip common leading whitespace from multi-line strings.
fn strip_common_indent(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let content_lines: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();

    if content_lines.is_empty() {
        return text.to_string();
    }

    let min_indent = content_lines
        .iter()
        .map(|line| line.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0);

    if min_indent == 0 {
        return text.strip_suffix('\n').unwrap_or(text).to_string();
    }

    #[expect(
        clippy::string_slice,
        reason = "skip counts leading ASCII space/tab chars, so it is a char-boundary byte offset"
    )]
    let stripped: String = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                ""
            } else {
                let skip = min_indent.min(line.len());
                &line[skip..]
            }
        })
        .collect::<Vec<&str>>()
        .join("\n");

    stripped.strip_suffix('\n').unwrap_or(&stripped).to_string()
}

/// Compute the common leading indent (spaces/tabs) across non-empty lines.
fn common_indent(text: &str) -> usize {
    text.split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(|line| line.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0)
}

/// Strip up to `n` leading whitespace characters from each line and remove trailing newline.
fn strip_indent(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    #[expect(
        clippy::string_slice,
        reason = "skip counts leading ASCII space/tab chars, so it is a char-boundary byte offset"
    )]
    let stripped: String = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                ""
            } else {
                let ws = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                let skip = n.min(ws);
                &line[skip..]
            }
        })
        .collect::<Vec<&str>>()
        .join("\n");
    stripped.strip_suffix('\n').unwrap_or(&stripped).to_string()
}

/// Escape `value` so it round-trips as the body of a double-quoted Harn
/// string literal: the lexer's escape set (`\n`, `\r`, `\t`, `\0`, `\\`,
/// `\"`) plus `\$` before `{` so a value containing `${…}` renders as text
/// instead of becoming live interpolation when the generated source is
/// compiled. This is the single source of truth for code generators — the
/// CLI's `harn try` scaffolding and the VM's composition/crystallize
/// codegen had each grown a private copy that forgot `${`, letting a value
/// like `${host_call(...)}` execute.
pub fn escape_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            _ => out.push(ch),
        }
    }
    out
}

/// Remove a trailing-newline-only literal segment (for multiline strings
/// where the last segment before `"""` is just whitespace).
fn strip_trailing_newline_segments(mut segments: Vec<StringSegment>) -> Vec<StringSegment> {
    if let Some(StringSegment::Literal(s)) = segments.last() {
        if s.trim().is_empty() {
            segments.pop();
        }
    }
    segments
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_string_literal_round_trips_through_the_lexer() {
        for original in [
            "plain text",
            "with \"quotes\" and \\backslash",
            "newline\ntab\tcr\r",
            "interpolation ${1 + 1} stays text",
            "already-escaped \\${x}",
            "plain $dollar and ${nested ${inner}}",
        ] {
            let literal = format!("\"{}\"", escape_string_literal(original));
            let mut lexer = Lexer::new(&literal);
            let tokens = lexer.tokenize().expect("escaped literal must lex");
            let TokenKind::StringLiteral(ref value) = tokens[0].kind else {
                panic!(
                    "expected a plain string token for {literal:?}, got {:?}",
                    tokens[0].kind
                );
            };
            assert_eq!(value, original, "round trip through {literal:?}");
        }
    }

    #[test]
    fn shebang_at_offset_zero_is_skipped() {
        let src = "#!/usr/bin/env harn\nlet x = 1";
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("shebang should be skipped");
        // Expect: Newline, Let, Identifier(x), Eq, IntLiteral(1)
        assert_eq!(tokens[0].kind, TokenKind::Newline);
        assert_eq!(tokens[1].kind, TokenKind::Let);
        assert!(matches!(&tokens[2].kind, TokenKind::Identifier(n) if n == "x"));
    }

    #[test]
    fn shebang_without_trailing_newline_is_skipped() {
        let src = "#!/usr/bin/env harn";
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("shebang at EOF should be skipped");
        // After the shebang there should be only the trailing EOF token.
        let non_eof: Vec<_> = tokens
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Eof))
            .collect();
        assert!(
            non_eof.is_empty(),
            "expected only EOF after shebang-only file, got {non_eof:?}"
        );
    }

    #[test]
    fn hash_in_middle_of_file_still_errors() {
        let src = "let x = 1\n# not a shebang\n";
        let mut lexer = Lexer::new(src);
        let result = lexer.tokenize();
        assert!(
            matches!(result, Err(LexerError::UnexpectedCharacter('#', _))),
            "got {result:?}"
        );
    }

    #[test]
    fn test_keywords() {
        let mut lexer = Lexer::new("pipeline let var if else for in require");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Pipeline);
        assert_eq!(tokens[1].kind, TokenKind::Let);
        assert_eq!(tokens[2].kind, TokenKind::Var);
        assert_eq!(tokens[3].kind, TokenKind::If);
        assert_eq!(tokens[4].kind, TokenKind::Else);
        assert_eq!(tokens[5].kind, TokenKind::For);
        assert_eq!(tokens[6].kind, TokenKind::In);
        assert_eq!(tokens[7].kind, TokenKind::Require);
    }

    #[test]
    fn generated_keyword_vocabulary_tokenizes_every_entry() {
        // Every string in KEYWORDS must lex as a non-identifier token.
        // If this fails, either KEYWORDS has a stale entry or the lexer
        // match in `identifier_or_keyword` is missing an arm.
        for kw in KEYWORDS {
            let mut lexer = Lexer::new(kw);
            let tokens = lexer.tokenize().expect("lex keyword");
            let first = &tokens[0].kind;
            assert!(
                !matches!(first, TokenKind::Identifier(_)),
                "keyword `{kw}` lexes as Identifier"
            );
        }
    }

    #[test]
    fn test_parallel_keyword() {
        let mut lexer = Lexer::new("parallel defer");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Parallel);
        assert_eq!(tokens[1].kind, TokenKind::Defer);
    }

    #[test]
    fn test_numbers() {
        let mut lexer = Lexer::new("42 3.14");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
        #[allow(clippy::approx_constant)]
        let expected = 3.14;
        assert_eq!(tokens[1].kind, TokenKind::FloatLiteral(expected));
    }

    #[test]
    fn test_int_literal_max_is_exact_and_overflow_is_an_error() {
        // i64::MAX lexes exactly as an int.
        let mut lexer = Lexer::new("9223372036854775807");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(i64::MAX));

        // i64::MAX + 1 (and anything larger) is rejected, not silently widened
        // to a lossy float. The sign is applied by the parser, so the most
        // negative i64 is unreachable as a bare literal and overflows here too.
        for src in ["9223372036854775808", "99999999999999999999999"] {
            let mut lexer = Lexer::new(src);
            assert!(
                matches!(
                    lexer.tokenize(),
                    Err(LexerError::IntegerLiteralOutOfRange(lit, _)) if lit == src
                ),
                "expected out-of-range error for {src}"
            );
        }

        // A float literal of the same magnitude is still fine.
        let mut lexer = Lexer::new("9223372036854775808.0");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::FloatLiteral(_)));
    }

    #[test]
    fn test_duration_suffix_requires_identifier_boundary() {
        let mut lexer = Lexer::new("1ms 1msfoo 2h_task 3s.ok");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::DurationLiteral(1));
        assert_eq!(tokens[1].kind, TokenKind::IntLiteral(1));
        assert!(matches!(&tokens[2].kind, TokenKind::Identifier(name) if name == "msfoo"));
        assert_eq!(tokens[3].kind, TokenKind::IntLiteral(2));
        assert!(matches!(&tokens[4].kind, TokenKind::Identifier(name) if name == "h_task"));
        assert_eq!(tokens[5].kind, TokenKind::DurationLiteral(3000));
        assert_eq!(tokens[6].kind, TokenKind::Dot);
    }

    #[test]
    fn test_duration_suffix_overflow_saturates() {
        let mut lexer = Lexer::new("18446744073709551615w");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::DurationLiteral(u64::MAX));
    }

    #[test]
    fn test_string() {
        let mut lexer = Lexer::new(r#""hello world""#);
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::StringLiteral("hello world".into())
        );
    }

    #[test]
    fn test_interpolated_string() {
        let mut lexer = Lexer::new(r#""hello ${name}!""#);
        let tokens = lexer.tokenize().unwrap();
        if let TokenKind::InterpolatedString(segs) = &tokens[0].kind {
            assert_eq!(segs.len(), 3);
            assert_eq!(segs[0], StringSegment::Literal("hello ".into()));
            assert!(matches!(&segs[1], StringSegment::Expression(e, _, _) if e == "name"));
            assert_eq!(segs[2], StringSegment::Literal("!".into()));
        } else {
            panic!("Expected interpolated string");
        }
    }

    /// Returns the captured text of the single interpolation hole in `src`.
    fn single_interpolation_expr(src: &str) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().unwrap();
        match &tokens[0].kind {
            TokenKind::InterpolatedString(segs) => segs
                .iter()
                .find_map(|s| match s {
                    StringSegment::Expression(e, _, _) => Some(e.clone()),
                    _ => None,
                })
                .expect("expected an interpolation expression segment"),
            other => panic!("expected interpolated string, got {other:?}"),
        }
    }

    #[test]
    fn test_interpolation_capture_is_string_literal_aware() {
        // A `}` (or `{`) inside a nested string literal must not end the hole.
        assert_eq!(
            single_interpolation_expr(r#""${x ?? "a}b"}""#),
            r#"x ?? "a}b""#
        );
        assert_eq!(single_interpolation_expr(r#""${f("}")}""#), r#"f("}")"#);
        assert_eq!(
            single_interpolation_expr(r#""${items["a}b"]}""#),
            r#"items["a}b"]"#
        );
        // A `\"` inside the nested string is preserved verbatim so it does not
        // close the literal early.
        assert_eq!(
            single_interpolation_expr(r#""${x ?? "a\"b"}""#),
            r#"x ?? "a\"b""#
        );
    }

    #[test]
    fn test_interpolation_escaped_outer_quote_is_rejected() {
        // Escaping the quotes of a nested string literal (`${x ?? \"y\"}`) is a
        // common mistake: inside an interpolation hole, string literals use bare
        // double quotes. A backslash is never valid in expression position, so
        // it is reported precisely at the backslash rather than scanning to EOF.
        let mut lexer = Lexer::new(r#""${x ?? \"y\"}""#);
        assert!(matches!(
            lexer.tokenize(),
            Err(LexerError::UnexpectedCharacter('\\', _))
        ));
    }

    #[test]
    fn test_empty_interpolation_rejected_in_single_and_multiline_strings() {
        let mut single = Lexer::new(r#""hello ${}""#);
        assert!(matches!(
            single.tokenize(),
            Err(LexerError::UnexpectedCharacter('}', _))
        ));

        let mut multiline = Lexer::new("\"\"\"\nhello ${}\n\"\"\"");
        assert!(matches!(
            multiline.tokenize(),
            Err(LexerError::UnexpectedCharacter('}', _))
        ));
    }

    #[test]
    fn test_multiline_string_escaped_dollar_before_interpolation() {
        let mut lexer = Lexer::new("\"\"\"\n  hi \\${VAR}\n  hello ${name}\n\"\"\"");
        let tokens = lexer.tokenize().unwrap();
        if let TokenKind::InterpolatedString(segs) = &tokens[0].kind {
            assert_eq!(segs.len(), 2);
            assert_eq!(segs[0], StringSegment::Literal("hi ${VAR}\nhello ".into()));
            assert!(matches!(&segs[1], StringSegment::Expression(e, _, _) if e == "name"));
        } else {
            panic!("Expected interpolated string");
        }
    }

    #[test]
    fn test_multiline_string_escaped_dollar_without_interpolation() {
        let mut lexer = Lexer::new("\"\"\"\n  hi \\${VAR}\n\"\"\"");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLiteral("hi ${VAR}".into()));
    }

    #[test]
    fn test_multiline_string_preserves_non_interpolation_dollar_escape() {
        let mut lexer = Lexer::new("\"\"\"\n  echo \\$PATH\n\"\"\"");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::StringLiteral("echo \\$PATH".into())
        );
    }

    #[test]
    fn test_interpolated_string_multiline_expression_tracks_lines() {
        // Regression: `${...}` inside a single-line string can itself span
        // multiple lines (e.g. `${render(\n  "x",\n  {k: v},\n)}`). The
        // lexer used to consume those inner newlines without incrementing
        // the line counter, so every token after the string reported a
        // line number too low — by the number of newlines consumed inside
        // the interpolation. Downstream lint spans pointed to wrong lines.
        let src = "const x = \"${render(\n  \"a\",\n  b,\n)}\"\nconst y = 1\n";
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().unwrap();
        // `const y` is on line 5 of the source.
        let const_y = tokens
            .iter()
            .skip(1) // the first `const` at line 1
            .find(|t| matches!(t.kind, TokenKind::Const))
            .expect("second `const`");
        assert_eq!(const_y.span.line, 5);
    }

    #[test]
    fn test_two_char_operators() {
        let mut lexer = Lexer::new("== != && || |> ?? ** -> <= >=");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Eq);
        assert_eq!(tokens[1].kind, TokenKind::Neq);
        assert_eq!(tokens[2].kind, TokenKind::And);
        assert_eq!(tokens[3].kind, TokenKind::Or);
        assert_eq!(tokens[4].kind, TokenKind::Pipe);
        assert_eq!(tokens[5].kind, TokenKind::NilCoal);
        assert_eq!(tokens[6].kind, TokenKind::Pow);
        assert_eq!(tokens[7].kind, TokenKind::Arrow);
        assert_eq!(tokens[8].kind, TokenKind::Lte);
        assert_eq!(tokens[9].kind, TokenKind::Gte);
    }

    #[test]
    fn test_block_comments() {
        let mut lexer = Lexer::new("/* outer /* nested */ still */ 42");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
    }

    #[test]
    fn test_multiline_block_comment_span_starts_at_open() {
        // A block comment that opens on line 2 and closes on line 4. Its span's
        // `line`/`column` must point at the opening `/*` (line 2), and `end_line`
        // at the closing `*/` (line 4) — matching how multi-line strings record
        // their span. Downstream consumers (`harn fmt`, the LSP) key comments by
        // `span.line`, so reporting the end line there misplaces the comment.
        let src = "const a = 1\n/* block\n   spanning\n   lines */\nconst b = 2";
        let mut lex = Lexer::new(src);
        let tokens = lex.tokenize_with_comments().unwrap();
        let block = tokens
            .iter()
            .find(|t| matches!(t.kind, TokenKind::BlockComment { .. }))
            .expect("block comment token");
        assert_eq!(block.span.line, 2, "start line should be the opening `/*`");
        assert_eq!(
            block.span.column, 1,
            "start column should be the `/*` column"
        );
        assert_eq!(
            block.span.end_line, 4,
            "end line should be the closing `*/`"
        );
    }

    #[test]
    fn test_line_comment() {
        let mut lexer = Lexer::new("42 // comment\n43");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
        assert_eq!(tokens[1].kind, TokenKind::Newline);
        assert_eq!(tokens[2].kind, TokenKind::IntLiteral(43));
    }

    #[test]
    fn test_doc_line_comment_detection() {
        let cases = [
            ("// regular", false),
            ("/// doc", true),
            ("//// separator bar", false),
            ("///// also a bar", false),
            ("///", true), // empty doc comment
        ];
        for (src, expect_doc) in cases {
            let mut lex = Lexer::new(src);
            let tokens = lex.tokenize_with_comments().unwrap();
            match &tokens[0].kind {
                TokenKind::LineComment { is_doc, .. } => {
                    assert_eq!(
                        *is_doc, expect_doc,
                        "expected is_doc={expect_doc} for input {src:?}",
                    );
                }
                other => panic!("expected LineComment for {src:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_doc_block_comment_detection() {
        let cases = [
            ("/* regular */", false),
            ("/** doc */", true),
            ("/*** not a doc */", false),
            ("/**/", false), // empty block comment, not a doc
        ];
        for (src, expect_doc) in cases {
            let mut lex = Lexer::new(src);
            let tokens = lex.tokenize_with_comments().unwrap();
            match &tokens[0].kind {
                TokenKind::BlockComment { is_doc, .. } => {
                    assert_eq!(
                        *is_doc, expect_doc,
                        "expected is_doc={expect_doc} for input {src:?}",
                    );
                }
                other => panic!("expected BlockComment for {src:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_newlines() {
        let mut lexer = Lexer::new("a\nb");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Identifier("a".into()));
        assert_eq!(tokens[1].kind, TokenKind::Newline);
        assert_eq!(tokens[2].kind, TokenKind::Identifier("b".into()));
    }

    #[test]
    fn test_unexpected_character() {
        let mut lexer = Lexer::new("`");
        let err = lexer.tokenize().unwrap_err();
        assert!(matches!(err, LexerError::UnexpectedCharacter('`', _)));
    }

    #[test]
    fn test_at_token() {
        let mut lexer = Lexer::new("@deprecated");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::At);
        assert_eq!(tokens[1].kind, TokenKind::Identifier("deprecated".into()));
    }

    #[test]
    fn test_unterminated_string() {
        let mut lexer = Lexer::new("\"unterminated");
        let err = lexer.tokenize().unwrap_err();
        assert!(matches!(err, LexerError::UnterminatedString(_)));
    }

    #[test]
    fn test_escape_sequences() {
        let mut lexer = Lexer::new(r#""a\nb\t\\""#);
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLiteral("a\nb\t\\".into()));
    }

    #[test]
    fn test_escape_carriage_return_and_null() {
        let mut lexer = Lexer::new(r#""a\rb\0c""#);
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StringLiteral("a\rb\0c".into()));
    }

    #[test]
    fn test_number_then_dot_method() {
        let mut lexer = Lexer::new("42.method");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
        assert_eq!(tokens[1].kind, TokenKind::Dot);
        assert_eq!(tokens[2].kind, TokenKind::Identifier("method".into()));
    }

    #[test]
    fn test_hashed_raw_string_basic() {
        let mut lexer = Lexer::new("r#\"abc\"#");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("abc".into()));
    }

    #[test]
    fn test_hashed_raw_string_embedded_quote() {
        // r#"a"b"# — the inner quote is not followed by `#`, so it's literal.
        let mut lexer = Lexer::new("r#\"a\"b\"#");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("a\"b".into()));
    }

    #[test]
    fn test_hashed_raw_string_regex_with_quotes() {
        // The motivating case: a regex matching quoted strings, no escaping.
        let mut lexer = Lexer::new("r#\"\"([^\"\\]*)\"\"#");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::RawStringLiteral("\"([^\"\\]*)\"".into())
        );
    }

    #[test]
    fn test_double_hashed_raw_string_holds_quote_hash() {
        // r##"a"#b"## — the `"#` run is shorter than the 2-hash delimiter,
        // so it stays literal.
        let mut lexer = Lexer::new("r##\"a\"#b\"##");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("a\"#b".into()));
    }

    #[test]
    fn test_plain_raw_string_still_works() {
        let mut lexer = Lexer::new("r\"plain\"");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("plain".into()));
    }

    #[test]
    fn test_hashed_raw_string_unterminated_errors() {
        let mut lexer = Lexer::new("r#\"no close\"");
        assert!(lexer.tokenize().is_err());
    }

    #[test]
    fn test_hashed_raw_string_newline_errors() {
        let mut lexer = Lexer::new("r#\"line1\nline2\"#");
        assert!(lexer.tokenize().is_err());
    }
}
