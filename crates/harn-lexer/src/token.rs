use std::fmt;

/// A segment of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StringSegment {
    Literal(String),
    Expression(String),
}

impl fmt::Display for StringSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StringSegment::Literal(s) => write!(f, "{s}"),
            StringSegment::Expression(e) => write!(f, "${{{e}}}"),
        }
    }
}

/// Source location for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Token kinds produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Pipeline,
    Extends,
    Override,
    Let,
    Var,
    If,
    Else,
    For,
    In,
    Match,
    Retry,
    Parallel,
    ParallelMap,
    Return,
    Import,
    True,
    False,
    Nil,
    Try,
    Catch,
    Throw,
    Fn,
    Spawn,
    While,

    // Literals
    Identifier(String),
    StringLiteral(String),
    InterpolatedString(Vec<StringSegment>),
    IntLiteral(i64),
    FloatLiteral(f64),

    // Two-character operators
    Eq,      // ==
    Neq,     // !=
    And,     // &&
    Or,      // ||
    Pipe,    // |>
    NilCoal, // ??
    Arrow,   // ->
    Lte,     // <=
    Gte,     // >=

    // Single-character operators
    Assign,   // =
    Not,      // !
    Dot,      // .
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Lt,       // <
    Gt,       // >
    Question, // ?

    // Delimiters
    LBrace,    // {
    RBrace,    // }
    LParen,    // (
    RParen,    // )
    LBracket,  // [
    RBracket,  // ]
    Comma,     // ,
    Colon,     // :
    Semicolon, // ;

    // Special
    Newline,
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Pipeline => write!(f, "pipeline"),
            TokenKind::Extends => write!(f, "extends"),
            TokenKind::Override => write!(f, "override"),
            TokenKind::Let => write!(f, "let"),
            TokenKind::Var => write!(f, "var"),
            TokenKind::If => write!(f, "if"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::For => write!(f, "for"),
            TokenKind::In => write!(f, "in"),
            TokenKind::Match => write!(f, "match"),
            TokenKind::Retry => write!(f, "retry"),
            TokenKind::Parallel => write!(f, "parallel"),
            TokenKind::ParallelMap => write!(f, "parallel_map"),
            TokenKind::Return => write!(f, "return"),
            TokenKind::Import => write!(f, "import"),
            TokenKind::True => write!(f, "true"),
            TokenKind::False => write!(f, "false"),
            TokenKind::Nil => write!(f, "nil"),
            TokenKind::Try => write!(f, "try"),
            TokenKind::Catch => write!(f, "catch"),
            TokenKind::Throw => write!(f, "throw"),
            TokenKind::Fn => write!(f, "fn"),
            TokenKind::Spawn => write!(f, "spawn"),
            TokenKind::While => write!(f, "while"),
            TokenKind::Identifier(s) => write!(f, "id({s})"),
            TokenKind::StringLiteral(s) => write!(f, "str({s})"),
            TokenKind::InterpolatedString(_) => write!(f, "istr(...)"),
            TokenKind::IntLiteral(n) => write!(f, "int({n})"),
            TokenKind::FloatLiteral(n) => write!(f, "float({n})"),
            TokenKind::Eq => write!(f, "=="),
            TokenKind::Neq => write!(f, "!="),
            TokenKind::And => write!(f, "&&"),
            TokenKind::Or => write!(f, "||"),
            TokenKind::Pipe => write!(f, "|>"),
            TokenKind::NilCoal => write!(f, "??"),
            TokenKind::Arrow => write!(f, "->"),
            TokenKind::Lte => write!(f, "<="),
            TokenKind::Gte => write!(f, ">="),
            TokenKind::Assign => write!(f, "="),
            TokenKind::Not => write!(f, "!"),
            TokenKind::Dot => write!(f, "."),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::Newline => write!(f, "\\n"),
            TokenKind::Eof => write!(f, "EOF"),
        }
    }
}

/// A token with its kind and source location.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, line: usize, column: usize) -> Self {
        Self {
            kind,
            span: Span::new(line, column),
        }
    }
}
