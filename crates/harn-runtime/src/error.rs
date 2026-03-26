use crate::value::Value;
use std::fmt;

/// Runtime errors with descriptive messages.
///
/// Error messages follow a consistent format:
/// - Variable/name errors: "Cannot <action>: <name>" or "Undefined <kind>: <name>"
/// - Type errors: "Type mismatch: expected <type>, got <value>"
/// - User errors: "Thrown: <message>"
#[derive(Debug, Clone)]
pub enum RuntimeError {
    /// Variable not found in any scope.
    UndefinedVariable {
        name: String,
        span: Option<harn_lexer::Span>,
        suggestion: Option<String>,
    },
    /// No registered builtin or user function with this name.
    UndefinedBuiltin {
        name: String,
        span: Option<harn_lexer::Span>,
        suggestion: Option<String>,
    },
    /// Attempted assignment to a `let` binding.
    ImmutableAssignment {
        name: String,
        span: Option<harn_lexer::Span>,
    },
    /// Type assertion failed (e.g., calling a non-closure as a function).
    TypeMismatch {
        expected: String,
        got: Value,
        span: Option<harn_lexer::Span>,
    },
    /// Internal: used to implement `return` (not a user-facing error).
    ReturnValue(Option<Value>),
    /// All retry attempts failed.
    RetryExhausted,
    /// User-thrown error via `throw`.
    ThrownError {
        value: Value,
        span: Option<harn_lexer::Span>,
    },
    /// Import failed.
    ImportError { path: String, reason: String },
    /// Yield: pipeline paused, waiting for host to resume with a value.
    YieldValue(Value),
}

impl RuntimeError {
    /// Create a thrown error from a string message.
    pub fn thrown(msg: impl Into<String>) -> Self {
        RuntimeError::ThrownError {
            value: Value::String(msg.into()),
            span: None,
        }
    }

    /// Returns true if this is an internal control-flow error (not user-facing).
    pub fn is_internal(&self) -> bool {
        matches!(self, RuntimeError::ReturnValue(_))
    }

    /// Get a short error category label for display.
    pub fn kind(&self) -> &'static str {
        match self {
            RuntimeError::UndefinedVariable { .. } => "NameError",
            RuntimeError::UndefinedBuiltin { .. } => "NameError",
            RuntimeError::ImmutableAssignment { .. } => "AssignmentError",
            RuntimeError::TypeMismatch { .. } => "TypeError",
            RuntimeError::ReturnValue(_) => "InternalError",
            RuntimeError::RetryExhausted => "RetryError",
            RuntimeError::ThrownError { .. } => "Error",
            RuntimeError::ImportError { .. } => "ImportError",
            RuntimeError::YieldValue(_) => "Yield",
        }
    }

    /// Get the span associated with this error, if any.
    pub fn span(&self) -> Option<&harn_lexer::Span> {
        match self {
            RuntimeError::UndefinedVariable { span, .. } => span.as_ref(),
            RuntimeError::UndefinedBuiltin { span, .. } => span.as_ref(),
            RuntimeError::ImmutableAssignment { span, .. } => span.as_ref(),
            RuntimeError::TypeMismatch { span, .. } => span.as_ref(),
            RuntimeError::ThrownError { span, .. } => span.as_ref(),
            _ => None,
        }
    }

    /// Attach a span to this error (builder pattern).
    pub fn with_span(mut self, s: harn_lexer::Span) -> Self {
        match &mut self {
            RuntimeError::UndefinedVariable { span, .. } => *span = Some(s),
            RuntimeError::UndefinedBuiltin { span, .. } => *span = Some(s),
            RuntimeError::ImmutableAssignment { span, .. } => *span = Some(s),
            RuntimeError::TypeMismatch { span, .. } => *span = Some(s),
            RuntimeError::ThrownError { span, .. } => *span = Some(s),
            _ => {}
        }
        self
    }

    /// Attach a "did you mean?" suggestion.
    pub fn with_suggestion(mut self, s: String) -> Self {
        match &mut self {
            RuntimeError::UndefinedVariable { suggestion, .. } => *suggestion = Some(s),
            RuntimeError::UndefinedBuiltin { suggestion, .. } => *suggestion = Some(s),
            _ => {}
        }
        self
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::UndefinedVariable { name, .. } => {
                write!(f, "Undefined variable: {name}")
            }
            RuntimeError::UndefinedBuiltin { name, .. } => {
                write!(f, "Undefined builtin: {name}")
            }
            RuntimeError::ImmutableAssignment { name, .. } => {
                write!(f, "Cannot assign to immutable binding: {name}")
            }
            RuntimeError::TypeMismatch { expected, got, .. } => {
                write!(f, "Type mismatch: expected {expected}, got {got}")
            }
            RuntimeError::ReturnValue(_) => write!(f, "Return from pipeline"),
            RuntimeError::RetryExhausted => write!(f, "All retry attempts exhausted"),
            RuntimeError::ThrownError { value, .. } => {
                write!(f, "Thrown: {}", value.as_string())
            }
            RuntimeError::ImportError { path, reason } => {
                write!(f, "Failed to import '{path}': {reason}")
            }
            RuntimeError::YieldValue(value) => write!(f, "Yield: {}", value.as_string()),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Unified error type for all Harn phases (lex, parse, runtime).
/// Used by the CLI to present errors consistently.
#[derive(Debug)]
pub enum HarnError {
    Lexer(harn_lexer::LexerError),
    Parser(harn_parser::ParserError),
    Runtime(RuntimeError),
}

impl HarnError {
    /// Get the span associated with this error, if it has one.
    pub fn span(&self) -> Option<harn_lexer::Span> {
        match self {
            HarnError::Runtime(e) => e.span().copied(),
            HarnError::Lexer(e) => match e {
                harn_lexer::LexerError::UnexpectedCharacter(_, span) => Some(*span),
                harn_lexer::LexerError::UnterminatedString(span) => Some(*span),
                harn_lexer::LexerError::UnterminatedBlockComment(span) => Some(*span),
            },
            HarnError::Parser(e) => match e {
                harn_parser::ParserError::Unexpected { span, .. } => Some(*span),
                harn_parser::ParserError::UnexpectedEof { .. } => None,
            },
        }
    }
}

impl fmt::Display for HarnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HarnError::Lexer(e) => write!(f, "{e}"),
            HarnError::Parser(e) => write!(f, "{e}"),
            HarnError::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HarnError {}

impl From<harn_lexer::LexerError> for HarnError {
    fn from(e: harn_lexer::LexerError) -> Self {
        HarnError::Lexer(e)
    }
}

impl From<harn_parser::ParserError> for HarnError {
    fn from(e: harn_parser::ParserError) -> Self {
        HarnError::Parser(e)
    }
}

impl From<RuntimeError> for HarnError {
    fn from(e: RuntimeError) -> Self {
        HarnError::Runtime(e)
    }
}
