use crate::token::*;
use std::fmt;

/// Lexer errors.
#[derive(Debug, Clone, PartialEq)]
pub enum LexerError {
    UnexpectedCharacter(char, Span),
    UnterminatedString(Span),
    UnterminatedBlockComment(Span),
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

    /// Tokenize source code, including comment tokens.
    pub fn tokenize_with_comments(&mut self) -> Result<Vec<Token>, LexerError> {
        self.tokenize_inner(true)
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexerError> {
        self.tokenize_inner(false)
    }

    fn tokenize_inner(&mut self, keep_comments: bool) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();

        while self.pos < self.source.len() {
            let ch = self.source[self.pos];

            // Skip whitespace (not newlines)
            if ch == ' ' || ch == '\t' || ch == '\r' {
                self.advance();
                continue;
            }

            // Backslash line continuation: `\` immediately before newline joins lines
            if ch == '\\' && self.peek() == Some('\n') {
                self.advance(); // skip `\`
                self.advance(); // skip `\n`
                self.line += 1;
                self.column = 1;
                continue;
            }

            // Newlines
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

            // Comments
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

            // String literals
            if ch == '"' {
                tokens.push(self.read_string()?);
                continue;
            }

            // Numbers
            if ch.is_ascii_digit() {
                tokens.push(self.read_number());
                continue;
            }

            // Identifiers and keywords
            if ch.is_alphabetic() || ch == '_' {
                tokens.push(self.read_identifier());
                continue;
            }

            // Two-character operators
            if let Some(tok) = self.try_two_char_op() {
                tokens.push(tok);
                continue;
            }

            // Single-character operators and delimiters
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
        self.advance(); // skip first /
        self.advance(); // skip second /
        let mut text = String::new();
        while self.pos < self.source.len() && self.source[self.pos] != '\n' {
            text.push(self.source[self.pos]);
            self.advance();
        }
        Token::with_span(
            TokenKind::LineComment(text),
            Span::with_offsets(start_byte, self.byte_pos, start_line, start_col),
        )
    }

    fn read_block_comment(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(self.byte_pos, self.byte_pos, self.line, self.column);
        self.advance(); // skip /
        self.advance(); // skip *
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
        Ok(Token::with_span(
            TokenKind::BlockComment(text),
            Span::with_offsets(start_byte, self.byte_pos, self.line, start.column),
        ))
    }

    fn read_string(&mut self) -> Result<Token, LexerError> {
        let start_byte = self.byte_pos;
        let start = Span::with_offsets(start_byte, start_byte, self.line, self.column);

        // Check for triple-quote
        if self.pos + 2 < self.source.len()
            && self.source[self.pos + 1] == '"'
            && self.source[self.pos + 2] == '"'
        {
            return self.read_multi_line_string(start_byte, start);
        }

        self.advance(); // skip opening "

        let mut value = String::new();
        let mut segments: Vec<StringSegment> = Vec::new();
        let mut has_interpolation = false;

        while self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == '"' {
                self.advance(); // skip closing "
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

            // String interpolation: ${expression}
            if ch == '$' && self.peek() == Some('{') {
                has_interpolation = true;
                if !value.is_empty() {
                    segments.push(StringSegment::Literal(std::mem::take(&mut value)));
                }
                self.advance(); // skip $
                self.advance(); // skip {
                let mut depth = 1;
                let mut expr = String::new();
                while self.pos < self.source.len() && depth > 0 {
                    if self.source[self.pos] == '{' {
                        depth += 1;
                    }
                    if self.source[self.pos] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    expr.push(self.source[self.pos]);
                    self.advance();
                }
                if self.pos >= self.source.len() {
                    return Err(LexerError::UnterminatedString(start));
                }
                self.advance(); // skip closing }
                if expr.trim().is_empty() {
                    return Err(LexerError::UnexpectedCharacter(
                        '}',
                        Span::with_offsets(
                            self.byte_pos,
                            self.byte_pos + 1,
                            self.line,
                            self.column,
                        ),
                    ));
                }
                segments.push(StringSegment::Expression(expr));
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
                    't' => value.push('\t'),
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
        self.advance(); // skip first "
        self.advance(); // skip second "
        self.advance(); // skip third "

        // Skip optional newline after opening """
        if self.pos < self.source.len() && self.source[self.pos] == '\n' {
            self.advance();
            self.line += 1;
            self.column = 1;
        }

        let mut value = String::new();
        while self.pos < self.source.len() {
            if self.source[self.pos] == '"'
                && self.pos + 2 < self.source.len()
                && self.source[self.pos + 1] == '"'
                && self.source[self.pos + 2] == '"'
            {
                self.advance(); // skip first "
                self.advance(); // skip second "
                self.advance(); // skip third "
                let stripped = strip_common_indent(&value);
                return Ok(Token::with_span(
                    TokenKind::StringLiteral(stripped),
                    Span::with_offsets(start_byte, self.byte_pos, start.line, start.column),
                ));
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

    fn read_number(&mut self) -> Token {
        let start_byte = self.byte_pos;
        let start_col = self.column;
        let mut num_str = String::new();
        let mut is_float = false;

        while self.pos < self.source.len()
            && (self.source[self.pos].is_ascii_digit() || self.source[self.pos] == '.')
        {
            if self.source[self.pos] == '.' {
                if is_float {
                    break; // second dot
                }
                // Check next char is digit (otherwise it's method access like 42.method)
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

        // Check for duration suffix: ms, s, m, h
        if !is_float {
            if let Some(ms) = self.try_duration_suffix(&num_str) {
                return Token::with_span(
                    TokenKind::DurationLiteral(ms),
                    Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
                );
            }
        }

        if is_float {
            let n: f64 = num_str.parse().unwrap_or(0.0);
            Token::with_span(
                TokenKind::FloatLiteral(n),
                Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
            )
        } else {
            match num_str.parse::<i64>() {
                Ok(n) => Token::with_span(
                    TokenKind::IntLiteral(n),
                    Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
                ),
                Err(_) => {
                    // Integer overflow: fall back to float
                    let n: f64 = num_str.parse().unwrap_or(0.0);
                    Token::with_span(
                        TokenKind::FloatLiteral(n),
                        Span::with_offsets(start_byte, self.byte_pos, self.line, start_col),
                    )
                }
            }
        }
    }

    /// Try to parse a duration suffix (ms, s, m, h) after a number.
    /// Returns the duration in milliseconds if a suffix is found.
    fn try_duration_suffix(&mut self, num_str: &str) -> Option<u64> {
        let n: u64 = num_str.parse().ok()?;
        if self.pos < self.source.len() {
            let ch = self.source[self.pos];
            if ch == 'm' && self.source.get(self.pos + 1) == Some(&'s') {
                self.advance(); // m
                self.advance(); // s
                return Some(n);
            }
            if ch == 's'
                && self
                    .source
                    .get(self.pos + 1)
                    .is_none_or(|c| !c.is_alphanumeric())
            {
                self.advance(); // s
                return Some(n * 1000);
            }
            if ch == 'm'
                && self
                    .source
                    .get(self.pos + 1)
                    .is_none_or(|c| !c.is_alphanumeric() && *c != 's')
            {
                self.advance(); // m
                return Some(n * 60 * 1000);
            }
            if ch == 'h'
                && self
                    .source
                    .get(self.pos + 1)
                    .is_none_or(|c| !c.is_alphanumeric())
            {
                self.advance(); // h
                return Some(n * 60 * 60 * 1000);
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
            "var" => TokenKind::Var,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "match" => TokenKind::Match,
            "retry" => TokenKind::Retry,
            "parallel" => TokenKind::Parallel,
            "parallel_map" => TokenKind::ParallelMap,
            "return" => TokenKind::Return,
            "import" => TokenKind::Import,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "throw" => TokenKind::Throw,
            "fn" => TokenKind::Fn,
            "spawn" => TokenKind::Spawn,
            "while" => TokenKind::While,
            "type" => TokenKind::TypeKw,
            "enum" => TokenKind::Enum,
            "struct" => TokenKind::Struct,
            "interface" => TokenKind::Interface,
            "pub" => TokenKind::Pub,
            "from" => TokenKind::From,
            "thru" => TokenKind::Thru,
            "upto" => TokenKind::Upto,
            "guard" => TokenKind::Guard,
            "ask" => TokenKind::Ask,
            "deadline" => TokenKind::Deadline,
            "yield" => TokenKind::Yield,
            "mutex" => TokenKind::Mutex,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
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
            _ => None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keywords() {
        let mut lexer = Lexer::new("pipeline let var if else for in");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Pipeline);
        assert_eq!(tokens[1].kind, TokenKind::Let);
        assert_eq!(tokens[2].kind, TokenKind::Var);
        assert_eq!(tokens[3].kind, TokenKind::If);
        assert_eq!(tokens[4].kind, TokenKind::Else);
        assert_eq!(tokens[5].kind, TokenKind::For);
        assert_eq!(tokens[6].kind, TokenKind::In);
    }

    #[test]
    fn test_parallel_map_keyword() {
        let mut lexer = Lexer::new("parallel_map parallel");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::ParallelMap);
        assert_eq!(tokens[1].kind, TokenKind::Parallel);
    }

    #[test]
    fn test_numbers() {
        let mut lexer = Lexer::new("42 3.14");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
        assert_eq!(tokens[1].kind, TokenKind::FloatLiteral(3.14));
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
            assert_eq!(segs[1], StringSegment::Expression("name".into()));
            assert_eq!(segs[2], StringSegment::Literal("!".into()));
        } else {
            panic!("Expected interpolated string");
        }
    }

    #[test]
    fn test_two_char_operators() {
        let mut lexer = Lexer::new("== != && || |> ?? -> <= >=");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Eq);
        assert_eq!(tokens[1].kind, TokenKind::Neq);
        assert_eq!(tokens[2].kind, TokenKind::And);
        assert_eq!(tokens[3].kind, TokenKind::Or);
        assert_eq!(tokens[4].kind, TokenKind::Pipe);
        assert_eq!(tokens[5].kind, TokenKind::NilCoal);
        assert_eq!(tokens[6].kind, TokenKind::Arrow);
        assert_eq!(tokens[7].kind, TokenKind::Lte);
        assert_eq!(tokens[8].kind, TokenKind::Gte);
    }

    #[test]
    fn test_block_comments() {
        let mut lexer = Lexer::new("/* outer /* nested */ still */ 42");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
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
    fn test_newlines() {
        let mut lexer = Lexer::new("a\nb");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Identifier("a".into()));
        assert_eq!(tokens[1].kind, TokenKind::Newline);
        assert_eq!(tokens[2].kind, TokenKind::Identifier("b".into()));
    }

    #[test]
    fn test_backslash_continuation() {
        // Backslash before newline joins lines — no Newline token emitted
        let mut lexer = Lexer::new("10 \\\n- 3");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(10));
        assert_eq!(tokens[1].kind, TokenKind::Minus);
        assert_eq!(tokens[2].kind, TokenKind::IntLiteral(3));
        // No Newline token between 10 and -
        assert_eq!(tokens.len(), 4); // 10, -, 3, EOF
    }

    #[test]
    fn test_unexpected_character() {
        let mut lexer = Lexer::new("@");
        let err = lexer.tokenize().unwrap_err();
        assert!(matches!(err, LexerError::UnexpectedCharacter('@', _)));
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
    fn test_number_then_dot_method() {
        let mut lexer = Lexer::new("42.method");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
        assert_eq!(tokens[1].kind, TokenKind::Dot);
        assert_eq!(tokens[2].kind, TokenKind::Identifier("method".into()));
    }
}
