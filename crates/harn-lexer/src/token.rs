use std::fmt;

/// A segment of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StringSegment {
    Literal(String),
    /// An interpolated expression with its source position (line, column).
    Expression(String, usize, usize),
}

impl fmt::Display for StringSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StringSegment::Literal(s) => write!(f, "{s}"),
            StringSegment::Expression(e, _, _) => write!(f, "${{{e}}}"),
        }
    }
}

/// Source location for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset from start of source (inclusive).
    pub start: usize,
    /// Byte offset from start of source (exclusive).
    pub end: usize,
    /// 1-based line number of start position.
    pub line: usize,
    /// 1-based column number of start position.
    pub column: usize,
    /// 1-based line number of end position (for multiline span detection).
    pub end_line: usize,
}

impl Span {
    pub fn with_offsets(start: usize, end: usize, line: usize, column: usize) -> Self {
        Self {
            start,
            end,
            line,
            column,
            end_line: line,
        }
    }

    /// Create a span covering two spans (from start of `a` to end of `b`).
    pub fn merge(a: Span, b: Span) -> Span {
        Span {
            start: a.start,
            end: b.end,
            line: a.line,
            column: a.column,
            end_line: b.end_line,
        }
    }

    /// A dummy span for synthetic/generated nodes.
    pub fn dummy() -> Self {
        Self {
            start: 0,
            end: 0,
            line: 0,
            column: 0,
            end_line: 0,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A machine-applicable text replacement for autofixing diagnostics.
#[derive(Debug, Clone)]
pub struct FixEdit {
    /// The source span to replace.
    pub span: Span,
    /// The replacement text (empty string = deletion).
    pub replacement: String,
}

/// Canonical list of Harn language keywords.
///
/// This is the single source of truth for keyword tokens. The lexer's
/// identifier-to-keyword match in `lexer.rs` must stay in sync; the unit test
/// `test_keywords_const_covers_lexer` verifies parity between the two.
///
/// Tooling that needs the keyword set (syntax highlighters, the LSP, etc.)
/// should read `KEYWORDS` rather than hard-coding a duplicate list.
pub const KEYWORDS: &[&str] = &[
    "break",
    "catch",
    "continue",
    "deadline",
    "defer",
    "else",
    "enum",
    "exclusive",
    "extends",
    "false",
    "finally",
    "fn",
    "for",
    "from",
    "guard",
    "if",
    "impl",
    "import",
    "in",
    "interface",
    "let",
    "match",
    "mutex",
    "nil",
    "override",
    "parallel",
    "pipeline",
    "pub",
    "require",
    "retry",
    "return",
    "select",
    "spawn",
    "struct",
    "throw",
    "to",
    "tool",
    "true",
    "try",
    "type",
    "var",
    "while",
    "yield",
];

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
    Return,
    Import,
    True,
    False,
    Nil,
    Try,
    Catch,
    Throw,
    Finally,
    Fn,
    Spawn,
    While,
    TypeKw,
    Enum,
    Struct,
    Interface,
    Pub,
    From,
    To,
    Tool,
    Exclusive,
    Guard,
    Require,
    Deadline,
    Defer,
    Yield,
    Mutex,
    Break,
    Continue,
    Select,
    Impl,

    // Literals
    Identifier(String),
    StringLiteral(String),
    InterpolatedString(Vec<StringSegment>),
    /// Raw string literal `r"..."` — no escape processing, no interpolation.
    RawStringLiteral(String),
    IntLiteral(i64),
    FloatLiteral(f64),
    /// Duration literal in milliseconds: 500ms, 5s, 30m, 2h, 1d, 1w
    DurationLiteral(u64),

    // Two-character operators
    Eq,            // ==
    Neq,           // !=
    And,           // &&
    Or,            // ||
    Pipe,          // |>
    NilCoal,       // ??
    Pow,           // **
    QuestionDot,   // ?.
    Arrow,         // ->
    Lte,           // <=
    Gte,           // >=
    PlusAssign,    // +=
    MinusAssign,   // -=
    StarAssign,    // *=
    SlashAssign,   // /=
    PercentAssign, // %=

    // Single-character operators
    Assign,   // =
    Not,      // !
    Dot,      // .
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Lt,       // <
    Gt,       // >
    Question, // ?
    Bar,      // |  (for union types)

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

    // Comments
    LineComment { text: String, is_doc: bool },  // // text or /// text
    BlockComment { text: String, is_doc: bool }, // /* text */ or /** text */

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
            TokenKind::Return => write!(f, "return"),
            TokenKind::Import => write!(f, "import"),
            TokenKind::True => write!(f, "true"),
            TokenKind::False => write!(f, "false"),
            TokenKind::Nil => write!(f, "nil"),
            TokenKind::Try => write!(f, "try"),
            TokenKind::Catch => write!(f, "catch"),
            TokenKind::Throw => write!(f, "throw"),
            TokenKind::Finally => write!(f, "finally"),
            TokenKind::Fn => write!(f, "fn"),
            TokenKind::Spawn => write!(f, "spawn"),
            TokenKind::While => write!(f, "while"),
            TokenKind::TypeKw => write!(f, "type"),
            TokenKind::Enum => write!(f, "enum"),
            TokenKind::Struct => write!(f, "struct"),
            TokenKind::Interface => write!(f, "interface"),
            TokenKind::Pub => write!(f, "pub"),
            TokenKind::From => write!(f, "from"),
            TokenKind::To => write!(f, "to"),
            TokenKind::Tool => write!(f, "tool"),
            TokenKind::Exclusive => write!(f, "exclusive"),
            TokenKind::Guard => write!(f, "guard"),
            TokenKind::Require => write!(f, "require"),
            TokenKind::Deadline => write!(f, "deadline"),
            TokenKind::Defer => write!(f, "defer"),
            TokenKind::Yield => write!(f, "yield"),
            TokenKind::Mutex => write!(f, "mutex"),
            TokenKind::Break => write!(f, "break"),
            TokenKind::Continue => write!(f, "continue"),
            TokenKind::Select => write!(f, "select"),
            TokenKind::Impl => write!(f, "impl"),
            TokenKind::Identifier(s) => write!(f, "id({s})"),
            TokenKind::StringLiteral(s) => write!(f, "str({s})"),
            TokenKind::InterpolatedString(_) => write!(f, "istr(...)"),
            TokenKind::RawStringLiteral(s) => write!(f, "rstr({s})"),
            TokenKind::IntLiteral(n) => write!(f, "int({n})"),
            TokenKind::FloatLiteral(n) => write!(f, "float({n})"),
            TokenKind::DurationLiteral(ms) => write!(f, "duration({ms}ms)"),
            TokenKind::Eq => write!(f, "=="),
            TokenKind::Neq => write!(f, "!="),
            TokenKind::And => write!(f, "&&"),
            TokenKind::Or => write!(f, "||"),
            TokenKind::Pipe => write!(f, "|>"),
            TokenKind::NilCoal => write!(f, "??"),
            TokenKind::Pow => write!(f, "**"),
            TokenKind::QuestionDot => write!(f, "?."),
            TokenKind::Arrow => write!(f, "->"),
            TokenKind::Lte => write!(f, "<="),
            TokenKind::Gte => write!(f, ">="),
            TokenKind::PlusAssign => write!(f, "+="),
            TokenKind::MinusAssign => write!(f, "-="),
            TokenKind::StarAssign => write!(f, "*="),
            TokenKind::SlashAssign => write!(f, "/="),
            TokenKind::PercentAssign => write!(f, "%="),
            TokenKind::Assign => write!(f, "="),
            TokenKind::Not => write!(f, "!"),
            TokenKind::Dot => write!(f, "."),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Percent => write!(f, "%"),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::Bar => write!(f, "|"),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::LineComment { text, is_doc } => {
                let prefix = if *is_doc { "///" } else { "//" };
                write!(f, "{prefix} {text}")
            }
            TokenKind::BlockComment { text, is_doc } => {
                let prefix = if *is_doc { "/**" } else { "/*" };
                write!(f, "{prefix} {text} */")
            }
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
    pub fn with_span(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
