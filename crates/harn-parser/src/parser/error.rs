use harn_lexer::Span;
use std::fmt;

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
}

impl ParserError {
    /// The source span the error points at.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            ParserError::Unexpected { span, .. } | ParserError::UnexpectedEof { span, .. } => *span,
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
        }
    }
}

impl std::error::Error for ParserError {}
