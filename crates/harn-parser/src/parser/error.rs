use harn_lexer::Span;
use std::fmt;

/// The retired range keyword, kept for diagnostics and the migration.
pub const REMOVED_RANGE_KEYWORD: &str = "to";

/// Parser errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserError {
    Unexpected {
        got: String,
        expected: String,
        span: Span,
    },
    UnexpectedEof {
        expected: String,
        span: Span,
    },
    /// The retired `start to end [exclusive]` range spelling. `span` covers
    /// the `to` keyword.
    RemovedRangeSyntax {
        span: Span,
    },
}

impl ParserError {
    /// The source span the error points at.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            ParserError::Unexpected { span, .. }
            | ParserError::UnexpectedEof { span, .. }
            | ParserError::RemovedRangeSyntax { span } => *span,
        }
    }
}

impl fmt::Display for ParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParserError::Unexpected {
                got,
                expected,
                span,
            } => write!(
                f,
                "Expected {expected}, got {got} at {}:{}",
                span.line, span.column
            ),
            ParserError::UnexpectedEof { expected, .. } => {
                write!(f, "Unexpected end of file, expected {expected}")
            }
            ParserError::RemovedRangeSyntax { span } => write!(
                f,
                "`{REMOVED_RANGE_KEYWORD}` ranges were removed; write `start..=end` (or `start..end` for `exclusive`) at {}:{}",
                span.line, span.column
            ),
        }
    }
}

impl std::error::Error for ParserError {}
