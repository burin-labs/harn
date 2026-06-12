use crate::token::*;
use std::fmt;

/// Lexer errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexerError {
    UnexpectedCharacter(char, Span),
    UnterminatedString(Span),
    UnterminatedBlockComment(Span),
    /// An integer literal whose magnitude does not fit in an `i64`. Reported
    /// instead of silently degrading to a lossy float.
    IntegerLiteralOutOfRange(String, Span),
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

/// Character-by-character scanner producing tokens.
pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    byte_pos: usize,
    line: usize,
    column: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            pos: 0,
            byte_pos: 0,
            line: 1,
            column: 1,
        }
    }

    /// Create a lexer that starts counting from the given source position.
    /// Useful for re-lexing interpolated expressions at their original location.
    pub fn with_position(source: &str, line: usize, column: usize) -> Self {
        Self {
            source: source.chars().collect(),
            pos: 0,
            byte_pos: 0,
            line,
            column,
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
        let mut tokens = Vec::new();

        // Skip a `#!` shebang line if present at the very start of the file.
        // Only valid at byte offset 0 — anywhere else, `#` is still an error.
        if self.pos == 0 && self.source.starts_with(&['#', '!']) {
            while self.pos < self.source.len() && self.source[self.pos] != '\n' {
                self.advance();
            }
        }

        while self.pos < self.source.len() {
            let ch = self.source[self.pos];

            if ch == ' ' || ch == '\t' || ch == '\r' {
                self.advance();
                continue;
            }

            // Backslash immediately before newline joins lines without emitting a Newline token.
            if ch == '\\' && self.peek() == Some('\n') {
                self.advance();
                self.advance();
                self.line += 1;
                self.column = 1;
                continue;
            }

            if ch == '\n' {
                let start = self.byte_pos;
                tokens.push(Token::with_span(
                    TokenKind::Newline,
                    Span::with_offsets(start, start + 1, self.line, self.column),
                ));
                self.advance();
                self.line += 1;
                self.column = 1;
                continue;
            }

            if ch == '/' {
                if self.peek() == Some('/') {
                    let tok = self.read_line_comment();
                    if keep_comments {
                        tokens.push(tok);
                    }
                    continue;
                }
                if self.peek() == Some('*') {
                    let tok = self.read_block_comment()?;
                    if keep_comments {
                        tokens.push(tok);
                    }
                    continue;
                }
            }

            if ch == 'r' && self.peek() == Some('"') {
                tokens.push(self.read_raw_string()?);
                continue;
            }

            // Hashed raw string: r#"..."#, r##"..."##, etc. The `#` run lets
            // the body hold literal `"` without escaping — the close is the
            // first `"` followed by the matching number of `#`. Only treated
            // as a raw string when the `#` run is actually followed by `"`;
            // otherwise we fall through (no `r#`-identifier syntax in Harn).
            if ch == 'r' && self.peek() == Some('#') {
                if let Some(hashes) = self.raw_hash_count() {
                    tokens.push(self.read_raw_string_hashed(hashes)?);
                    continue;
                }
            }

            if ch == '"' {
                tokens.push(self.read_string()?);
                continue;
            }

            if ch.is_ascii_digit() {
                tokens.push(self.read_number()?);
                continue;
            }

            if ch.is_alphabetic() || ch == '_' {
                tokens.push(self.read_identifier());
                continue;
            }

            if let Some(tok) = self.try_two_char_op() {
                tokens.push(tok);
                continue;
            }

            if let Some(kind) = self.single_char_token(ch) {
                let start = self.byte_pos;
                let col = self.column;
                self.advance();
                tokens.push(Token::with_span(
                    kind,
                    Span::with_offsets(start, self.byte_pos, self.line, col),
                ));
                continue;
            }

            return Err(LexerError::UnexpectedCharacter(
                ch,
                Span::with_offsets(
                    self.byte_pos,
                    self.byte_pos + ch.len_utf8(),
                    self.line,
                    self.column,
                ),
            ));
        }

        tokens.push(self.token(TokenKind::Eof));
        Ok(tokens)
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.pos + 1).copied()
    }

    fn advance(&mut self) {
        if self.pos < self.source.len() {
            self.byte_pos += self.source[self.pos].len_utf8();
        }
        self.pos += 1;
        self.column += 1;
    }

    fn token(&self, kind: TokenKind) -> Token {
        Token::with_span(
            kind,
            Span::with_offsets(self.byte_pos, self.byte_pos, self.line, self.column),
        )
    }

    fn read_line_comment(&mut self) -> Token {
        let start_byte = self.byte_pos;
        let start_col = self.column;
        let start_line = self.line;
        self.advance();
        self.advance();
        // `///foo` is a doc comment, but `////foo` (a separator bar) is not.
        let is_doc = self.source.get(self.pos).copied() == Some('/')
            && self.source.get(self.pos + 1).copied() != Some('/');
        if is_doc {
            self.advance();
        }
        let mut text = String::new();
        while self.pos < self.source.len() && self.source[self.pos] != '\n' {
            text.push(self.source[self.pos]);
            self.advance();
        }
        Token::with_span(
            TokenKind::LineComment { text, is_doc },
            Span::with_offsets(start_byte, self.byte_pos, start_line, start_col),
        )
    }

    fn read_block_comment(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(self.byte_pos, self.byte_pos, self.line, self.column);
        self.advance();
        self.advance();
        // `/** ... */` is a doc comment, but `/*** */` and `/**/` are not.
        let is_doc = self.source.get(self.pos).copied() == Some('*')
            && self.source.get(self.pos + 1).copied() != Some('*')
            && self.source.get(self.pos + 1).copied() != Some('/');
        if is_doc {
            self.advance();
        }
        let mut text = String::new();
        let mut depth = 1;
        while self.pos < self.source.len() && depth > 0 {
            if self.source[self.pos] == '/' && self.peek() == Some('*') {
                depth += 1;
                text.push('/');
                text.push('*');
                self.advance();
                self.advance();
            } else if self.source[self.pos] == '*' && self.peek() == Some('/') {
                depth -= 1;
                if depth > 0 {
                    text.push('*');
                    text.push('/');
                }
                self.advance();
                self.advance();
            } else if self.source[self.pos] == '\n' {
                text.push('\n');
                self.byte_pos += self.source[self.pos].len_utf8();
                self.line += 1;
                self.column = 1;
                self.pos += 1;
            } else {
                text.push(self.source[self.pos]);
                self.advance();
            }
        }
        if depth > 0 {
            return Err(LexerError::UnterminatedBlockComment(start));
        }
        // The span must point at the opening `/*` (line/column of `start`),
        // with `end_line` at the closing `*/` — mirroring how multi-line
        // strings record their span. Using `self.line` for the start line
        // here would report the comment's *end* line, which misplaces
        // multi-line block comments for every span consumer (`harn fmt`,
        // the LSP, diagnostics).
        let mut span = Span::with_offsets(start_byte, self.byte_pos, start.line, start.column);
        span.end_line = self.line;
        Ok(Token::with_span(
            TokenKind::BlockComment { text, is_doc },
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
        self.advance(); // consume '$'
        self.advance(); // consume '{'
        let expr_line = self.line;
        let expr_col = self.column;
        let mut depth = 1usize;
        let mut expr = String::new();
        let mut in_string = false;
        while self.pos < self.source.len() && depth > 0 {
            let ch = self.source[self.pos];
            if in_string {
                if ch == '\\' {
                    // Preserve the backslash and the escaped char verbatim; an
                    // escaped quote must not close the nested string literal.
                    expr.push(ch);
                    self.advance();
                    if self.pos >= self.source.len() {
                        return Err(LexerError::UnterminatedString(start));
                    }
                    let escaped = self.source[self.pos];
                    if escaped == '\n' {
                        self.line += 1;
                        self.column = 0; // advance() restores column to 1
                    }
                    expr.push(escaped);
                    self.advance();
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
            } else {
                match ch {
                    // A backslash is never valid in expression position. The
                    // usual cause is escaping the quotes of a nested string
                    // literal (`${x ?? \"y\"}`); inside an interpolation hole,
                    // string literals use bare double quotes (`${x ?? "y"}`).
                    // Report it precisely here rather than scanning to EOF and
                    // surfacing a misleading "unterminated string".
                    '\\' => {
                        return Err(LexerError::UnexpectedCharacter(
                            '\\',
                            Span::with_offsets(
                                self.byte_pos,
                                self.byte_pos + 1,
                                self.line,
                                self.column,
                            ),
                        ));
                    }
                    '"' => in_string = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if ch == '\n' {
                self.line += 1;
                self.column = 0; // advance() restores column to 1
            }
            expr.push(ch);
            self.advance();
        }
        if self.pos >= self.source.len() {
            return Err(LexerError::UnterminatedString(start));
        }
        if expr.trim().is_empty() {
            return Err(LexerError::UnexpectedCharacter(
                '}',
                Span::with_offsets(self.byte_pos, self.byte_pos + 1, self.line, self.column),
            ));
        }
        self.advance(); // consume closing '}'
        Ok((expr, expr_line, expr_col))
    }

    fn read_string(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);

        if self.pos + 2 < self.source.len()
            && self.source[self.pos + 1] == '"'
            && self.source[self.pos + 2] == '"'
        {
            return self.read_multi_line_string(start_byte, start);
        }

        self.advance();

        let mut value = String::new();
        let mut segments: Vec<StringSegment> = Vec::new();
        let mut has_interpolation = false;

        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == '"' {
                self.advance();
                if has_interpolation {
                    if !value.is_empty() {
                        segments.push(StringSegment::Literal(value));
                    }
                    return Ok(Token::with_span(
                        TokenKind::InterpolatedString(segments),
                        Span::with_offsets(start_byte, self.byte_pos, start.line, start.column),
                    ));
                }
                return Ok(Token::with_span(
                    TokenKind::StringLiteral(value),
                    Span::with_offsets(start_byte, self.byte_pos, start.line, start.column),
                ));
            }

            if ch == '$' && self.peek() == Some('{') {
                has_interpolation = true;
                if !value.is_empty() {
                    segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                }
                let (expr, expr_line, expr_col) = self.capture_interpolation_expr(start)?;
                segments.push(StringSegment::Expression(expr, expr_line, expr_col));
                continue;
            }

            if ch == '\\' {
                self.advance();
                if self.pos >= self.source.len() {
                    return Err(LexerError::UnterminatedString(start));
                }
                let escaped = self.source[self.pos];
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
                self.advance();
                continue;
            }

            if ch == '\n' {
                return Err(LexerError::UnterminatedString(start));
            }

            value.push(ch);
            self.advance();
        }
        Err(LexerError::UnterminatedString(start))
    }

    fn read_multi_line_string(
        &mut self,
        start_byte: usize,
        start: Span,
    ) -> Result<Token, LexerError> {
        self.advance();
        self.advance();
        self.advance();

        if self.pos < self.source.len() && self.source[self.pos] == '\n' {
            self.advance();
            self.line += 1;
            self.column = 1;
        }

        let mut value = String::new();
        let mut segments: Vec<StringSegment> = Vec::new();
        let mut has_interpolation = false;

        while self.pos < self.source.len() {
            if self.source[self.pos] == '"'
                && self.pos + 2 < self.source.len()
                && self.source[self.pos + 1] == '"'
                && self.source[self.pos + 2] == '"'
            {
                self.advance();
                self.advance();
                self.advance();
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
                    let mut span =
                        Span::with_offsets(start_byte, self.byte_pos, start.line, start.column);
                    span.end_line = self.line;
                    return Ok(Token::with_span(
                        TokenKind::InterpolatedString(segments),
                        span,
                    ));
                }
                let stripped = strip_common_indent(&value);
                let mut span =
                    Span::with_offsets(start_byte, self.byte_pos, start.line, start.column);
                span.end_line = self.line;
                return Ok(Token::with_span(TokenKind::StringLiteral(stripped), span));
            }

            if self.source[self.pos] == '$' && self.peek() == Some('{') {
                has_interpolation = true;
                if !value.is_empty() {
                    segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                }
                let (expr, expr_line, expr_col) = self.capture_interpolation_expr(start)?;
                segments.push(StringSegment::Expression(expr, expr_line, expr_col));
                continue;
            }

            if self.source[self.pos] == '\\'
                && self.peek() == Some('$')
                && self.source.get(self.pos + 2) == Some(&'{')
            {
                // Only an unpaired backslash escapes `${`; pairs remain literal text.
                let preceding_backslashes =
                    value.chars().rev().take_while(|ch| *ch == '\\').count();
                if preceding_backslashes % 2 == 0 {
                    self.advance();
                    value.push('$');
                    self.advance();
                    continue;
                }
            }

            if self.source[self.pos] == '\n' {
                value.push('\n');
                self.advance();
                self.line += 1;
                self.column = 1;
            } else {
                value.push(self.source[self.pos]);
                self.advance();
            }
        }
        Err(LexerError::UnterminatedString(start))
    }

    /// If the current `r` is followed by one-or-more `#` and then a `"`,
    /// return the `#` count (the opening delimiter width). Returns `None`
    /// when the `#` run is not closed by a `"`, so the caller falls through
    /// to ordinary tokenization. Pure lookahead — does not advance.
    fn raw_hash_count(&self) -> Option<usize> {
        let mut k = 1; // skip the leading `r`
        let mut hashes = 0;
        while self.source.get(self.pos + k) == Some(&'#') {
            hashes += 1;
            k += 1;
        }
        if hashes >= 1 && self.source.get(self.pos + k) == Some(&'"') {
            Some(hashes)
        } else {
            None
        }
    }

    /// Read a hashed raw string `r#"..."#` (with `hashes` `#` characters):
    /// no escape processing, no interpolation. The body may contain `"`;
    /// the literal ends at the first `"` followed by at least `hashes` `#`,
    /// consuming exactly `hashes` of them (any extra `#` stay in the stream,
    /// matching Rust's raw-string semantics). Like `r"..."`, the body may not
    /// span a newline.
    fn read_raw_string_hashed(&mut self, hashes: usize) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);
        // Consume `r`, the `#` run, and the opening `"`.
        self.advance(); // r
        for _ in 0..hashes {
            self.advance();
        }
        self.advance(); // opening quote

        let mut value = String::new();
        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == '"' {
                // A closing quote only ends the literal if followed by the
                // matching `#` run; otherwise it is a literal quote.
                let closed = (1..=hashes).all(|k| self.source.get(self.pos + k) == Some(&'#'));
                if closed {
                    self.advance(); // closing quote
                    for _ in 0..hashes {
                        self.advance();
                    }
                    return Ok(Token::with_span(
                        TokenKind::RawStringLiteral(value),
                        Span::with_offsets(start_byte, self.byte_pos, start.line, start.column),
                    ));
                }
            }
            if ch == '\n' {
                return Err(LexerError::UnterminatedString(start));
            }
            value.push(ch);
            self.advance();
        }
        Err(LexerError::UnterminatedString(start))
    }

    /// Read a raw string `r"..."`: no escape processing, no interpolation.
    fn read_raw_string(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);
        self.advance();
        self.advance();

        let mut value = String::new();
        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == '"' {
                self.advance();
                return Ok(Token::with_span(
                    TokenKind::RawStringLiteral(value),
                    Span::with_offsets(start_byte, self.byte_pos, start.line, start.column),
                ));
            }
            if ch == '\n' {
                return Err(LexerError::UnterminatedString(start));
            }
            value.push(ch);
            self.advance();
        }
        Err(LexerError::UnterminatedString(start))
    }

    fn read_number(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start_col = self.column;
        let mut num_str = String::new();
        let mut is_float = false;

        while self.pos < self.source.len()
            && (self.source[self.pos].is_ascii_digit() || self.source[self.pos] == '.')
        {
            if self.source[self.pos] == '.' {
                if is_float {
                    break;
                }
                // Disambiguate `42.method` (method access) from `42.5` (float literal).
                if let Some(next) = self.source.get(self.pos + 1) {
                    if !next.is_ascii_digit() {
                        break;
                    }
                } else {
                    break;
                }
                is_float = true;
            }
            num_str.push(self.source[self.pos]);
            self.advance();
        }

        if !is_float {
            if let Some(ms) = self.try_duration_suffix(&num_str) {
                return Ok(Token::with_span(
                    TokenKind::DurationLiteral(ms),
                    Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
                ));
            }
        }

        let span = Span::with_offsets(start_byte, self.byte_pos, self.line, start_col);
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
                    Err(LexerError::IntegerLiteralOutOfRange(num_str, span))
                }
            }
        }
    }

    /// Parse a duration suffix (ms, s, m, h, d, w) after a number, returning milliseconds.
    fn try_duration_suffix(&mut self, num_str: &str) -> Option<u64> {
        let n: u64 = num_str.parse().ok()?;
        if self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == 'm'
                && self.source.get(self.pos + 1) == Some(&'s')
                && is_duration_suffix_boundary(self.source.get(self.pos + 2))
            {
                self.advance();
                self.advance();
                return Some(n);
            }
            if ch == 's' && is_duration_suffix_boundary(self.source.get(self.pos + 1)) {
                self.advance();
                return Some(n.saturating_mul(1000));
            }
            if ch == 'm' && is_duration_suffix_boundary(self.source.get(self.pos + 1)) {
                self.advance();
                return Some(n.saturating_mul(60_000));
            }
            if ch == 'h' && is_duration_suffix_boundary(self.source.get(self.pos + 1)) {
                self.advance();
                return Some(n.saturating_mul(3_600_000));
            }
            if ch == 'd' && is_duration_suffix_boundary(self.source.get(self.pos + 1)) {
                self.advance();
                return Some(n.saturating_mul(86_400_000));
            }
            if ch == 'w' && is_duration_suffix_boundary(self.source.get(self.pos + 1)) {
                self.advance();
                return Some(n.saturating_mul(604_800_000));
            }
        }
        None
    }

    fn read_identifier(&mut self) -> Token {
        let start_byte = self.byte_pos;
        let start_col = self.column;
        let mut ident = String::new();

        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch.is_alphanumeric() || ch == '_' {
                ident.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        let kind = match ident.as_str() {
            "pipeline" => TokenKind::Pipeline,
            "extends" => TokenKind::Extends,
            "override" => TokenKind::Override,
            "let" => TokenKind::Let,
            "const" => TokenKind::Const,
            "var" => TokenKind::Var,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "match" => TokenKind::Match,
            "retry" => TokenKind::Retry,
            "parallel" => TokenKind::Parallel,
            "return" => TokenKind::Return,
            "import" => TokenKind::Import,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "throw" => TokenKind::Throw,
            "finally" => TokenKind::Finally,
            "fn" => TokenKind::Fn,
            "spawn" => TokenKind::Spawn,
            "while" => TokenKind::While,
            "type" => TokenKind::TypeKw,
            "enum" => TokenKind::Enum,
            "eval_pack" => TokenKind::EvalPack,
            "struct" => TokenKind::Struct,
            "interface" => TokenKind::Interface,
            "emit" => TokenKind::Emit,
            "pub" => TokenKind::Pub,
            "from" => TokenKind::From,
            "to" => TokenKind::To,
            "tool" => TokenKind::Tool,
            "exclusive" => TokenKind::Exclusive,
            "guard" => TokenKind::Guard,
            "require" => TokenKind::Require,
            "deadline" => TokenKind::Deadline,
            "defer" => TokenKind::Defer,
            "yield" => TokenKind::Yield,
            "mutex" => TokenKind::Mutex,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "select" => TokenKind::Select,
            "impl" => TokenKind::Impl,
            "skill" => TokenKind::Skill,
            "request_approval" => TokenKind::RequestApproval,
            "dual_control" => TokenKind::DualControl,
            "ask_user" => TokenKind::AskUser,
            "escalate_to" => TokenKind::EscalateTo,
            _ => TokenKind::Identifier(ident),
        };

        Token::with_span(
            kind,
            Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
        )
    }

    fn try_two_char_op(&mut self) -> Option<Token> {
        if self.pos >= self.source.len() {
            return None;
        }
        let ch = self.source[self.pos];
        let next = self.peek()?;

        let kind = match (ch, next) {
            ('=', '=') => TokenKind::Eq,
            ('!', '=') => TokenKind::Neq,
            ('&', '&') => TokenKind::And,
            ('|', '|') => TokenKind::Or,
            ('|', '>') => TokenKind::Pipe,
            ('?', '?') => TokenKind::NilCoal,
            ('*', '*') => TokenKind::Pow,
            ('?', '.') => TokenKind::QuestionDot,
            ('-', '>') => TokenKind::Arrow,
            ('-', '=') => TokenKind::MinusAssign,
            ('+', '=') => TokenKind::PlusAssign,
            ('*', '=') => TokenKind::StarAssign,
            ('/', '=') => TokenKind::SlashAssign,
            ('%', '=') => TokenKind::PercentAssign,
            ('<', '=') => TokenKind::Lte,
            ('>', '=') => TokenKind::Gte,
            _ => return None,
        };

        let start_byte = self.byte_pos;
        let col = self.column;
        self.advance();
        self.advance();
        Some(Token::with_span(
            kind,
            Span::with_offsets(start_byte, self.byte_pos, self.line, col),
        ))
    }

    fn single_char_token(&self, ch: char) -> Option<TokenKind> {
        match ch {
            '{' => Some(TokenKind::LBrace),
            '}' => Some(TokenKind::RBrace),
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            '[' => Some(TokenKind::LBracket),
            ']' => Some(TokenKind::RBracket),
            ',' => Some(TokenKind::Comma),
            ':' => Some(TokenKind::Colon),
            ';' => Some(TokenKind::Semicolon),
            '.' => Some(TokenKind::Dot),
            '=' => Some(TokenKind::Assign),
            '!' => Some(TokenKind::Not),
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '%' => Some(TokenKind::Percent),
            '<' => Some(TokenKind::Lt),
            '>' => Some(TokenKind::Gt),
            '?' => Some(TokenKind::Question),
            '|' => Some(TokenKind::Bar),
            '&' => Some(TokenKind::Amp),
            '@' => Some(TokenKind::At),
            _ => None,
        }
    }
}

fn is_duration_suffix_boundary(ch: Option<&char>) -> bool {
    ch.is_none_or(|c| !c.is_alphanumeric() && *c != '_')
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
    fn test_keywords_const_covers_lexer() {
        // Every string in KEYWORDS must lex as a non-identifier token.
        // If this fails, either KEYWORDS has a stale entry or the lexer
        // match in `identifier_or_keyword` is missing an arm.
        for kw in KEYWORDS {
            let mut lexer = Lexer::new(kw);
            let tokens = lexer.tokenize().expect("lex keyword");
            let first = &tokens[0].kind;
            assert!(
                !matches!(first, TokenKind::Identifier(_)),
                "keyword `{kw}` lexes as Identifier — KEYWORDS const and lexer match are out of sync"
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
        let src = "let x = \"${render(\n  \"a\",\n  b,\n)}\"\nlet y = 1\n";
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().unwrap();
        // `let y` is on line 5 of the source.
        let let_y = tokens
            .iter()
            .skip(1) // the first `let` at line 1
            .find(|t| matches!(t.kind, TokenKind::Let))
            .expect("second `let`");
        assert_eq!(let_y.span.line, 5);
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
        let src = "let a = 1\n/* block\n   spanning\n   lines */\nlet b = 2";
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
    fn test_backslash_continuation() {
        let mut lexer = Lexer::new("10 \\\n- 3");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(10));
        assert_eq!(tokens[1].kind, TokenKind::Minus);
        assert_eq!(tokens[2].kind, TokenKind::IntLiteral(3));
        // No Newline token between 10 and -: continuation joined them.
        assert_eq!(tokens.len(), 4);
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
