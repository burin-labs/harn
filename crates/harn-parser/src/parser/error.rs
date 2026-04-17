use harn_lexer::Span;
use std::fmt;

/// Parser errors.
#[derive(Debug, Clone, PartialEq)]
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
