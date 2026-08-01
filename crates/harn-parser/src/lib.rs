pub mod acp_ambient_globals;
pub mod analysis;
mod ast;
pub mod ast_json;
pub mod builtin_signatures;
pub mod const_eval;
pub mod diagnostic;
pub mod diagnostic_codes;
pub mod interpolation;
pub mod lexical;
mod parser;
pub mod stdlib_metadata;
pub mod typechecker;
pub mod visit;

pub use ast::*;
pub use diagnostic_codes::{
    Category as DiagnosticCodeCategory, Code as DiagnosticCode, ParseRepairSafetyError, Repair,
    RepairId, RepairSafety, RepairTemplate, REPAIR_REGISTRY,
};
pub use parser::*;
pub use stdlib_metadata::{
    parse_for_span as parse_stdlib_metadata, synthesize_example, StdlibMetadata,
};
pub use typechecker::{
    block_definitely_exits, format_type, stmt_definitely_exits, substitute_type_expr,
    DiagnosticDetails, DiagnosticSeverity, InlayHintInfo, NamespaceImportBinding, TypeChecker,
    TypeDiagnostic,
};

pub use builtin_signatures::install_builtin_manifest;

/// Explicit process-level bridge for downstream packages that have not yet
/// completed the typed `Harness` capability migration.
///
/// The bridge is intentionally opt-in and keeps the strict source surface as
/// the default. Defining the variable is not enough: it must be set to one of
/// `1`, `true`, `yes`, or `on`, so an empty or `0` value leaves strict
/// enforcement in place.
pub const HARN_LEGACY_AMBIENT_CAPABILITIES_ENV: &str = "HARN_LEGACY_AMBIENT_CAPABILITIES";

pub fn legacy_ambient_capabilities_enabled() -> bool {
    std::env::var(HARN_LEGACY_AMBIENT_CAPABILITIES_ENV).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether an old hostlib wire name projects a method from the authoritative
/// typed host-capability registry. This keeps compatibility recognition exact:
/// arbitrary `hostlib_*` spellings remain undefined.
pub fn is_registered_legacy_hostlib_name(name: &str) -> bool {
    if name == "hostlib_enable" {
        return true;
    }
    harn_builtin_meta::host_capabilities::capability_binding_for_legacy_hostlib_name(name).is_some()
}

/// Exact behavior-preserving spellings removed during the typed-Harness
/// cutover. The compiler lowers these to the canonical manifest name only in
/// compatibility mode; strict source continues to reject them.
pub fn legacy_builtin_alias_target(name: &str) -> Option<&'static str> {
    match name {
        "regex_replace_all" => Some("regex_replace"),
        "task_current" => Some("runtime_context"),
        _ => None,
    }
}

/// Returns `true` if `name` is a builtin recognized by the parser's static analyzer.
pub fn is_known_builtin(name: &str) -> bool {
    builtin_signatures::is_builtin(name)
}

/// Opt-in ambient bridge: treat a registered builtin as known without a typed
/// `Harness` capability import.
pub fn is_legacy_ambient_builtin(name: &str) -> bool {
    legacy_ambient_capabilities_enabled() && is_known_builtin(name)
}

/// Every builtin name known to the parser, alphabetically. Enables bidirectional
/// drift checks against the VM's runtime registry.
pub fn known_builtin_names() -> impl Iterator<Item = &'static str> {
    builtin_signatures::iter_builtin_names()
}

pub fn known_builtin_metadata() -> impl Iterator<Item = builtin_signatures::BuiltinMetadata> {
    builtin_signatures::iter_builtin_metadata()
}

/// Names sourced only from the parser's hand-written static fallback tables
/// (not the driver-installed `#[harn_builtin]` registry). Lets cross-crate
/// drift guards assert the static tables don't overlap with macro-published
/// or `runtime_only` builtins.
pub fn static_signature_names() -> impl Iterator<Item = &'static str> {
    builtin_signatures::static_signature_names()
}

/// Error from a source processing pipeline stage. Wraps the inner error
/// types so callers can dispatch on the failing stage.
#[derive(Debug)]
pub enum PipelineError {
    Lex(harn_lexer::LexerError),
    Parse(ParserError),
    /// Boxed to keep the enum small on the stack — TypeDiagnostic contains
    /// a Vec<FixEdit>.
    TypeCheck(Box<TypeDiagnostic>),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::Lex(e) => e.fmt(f),
            PipelineError::Parse(e) => e.fmt(f),
            PipelineError::TypeCheck(diag) => write!(f, "type error: {}", diag.message),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<harn_lexer::LexerError> for PipelineError {
    fn from(e: harn_lexer::LexerError) -> Self {
        PipelineError::Lex(e)
    }
}

impl From<ParserError> for PipelineError {
    fn from(e: ParserError) -> Self {
        PipelineError::Parse(e)
    }
}

impl PipelineError {
    /// Extract the source span, if any, for diagnostic rendering.
    pub fn span(&self) -> Option<&harn_lexer::Span> {
        match self {
            PipelineError::Lex(e) => match e {
                harn_lexer::LexerError::UnexpectedCharacter(_, span)
                | harn_lexer::LexerError::UnterminatedString(span)
                | harn_lexer::LexerError::IntegerLiteralOutOfRange(_, span)
                | harn_lexer::LexerError::UnterminatedBlockComment(span) => Some(span),
            },
            PipelineError::Parse(e) => match e {
                ParserError::Unexpected { span, .. } => Some(span),
                ParserError::UnexpectedEof { span, .. } => Some(span),
            },
            PipelineError::TypeCheck(diag) => diag.span.as_ref(),
        }
    }
}

/// Lex and parse source into an AST.
pub fn parse_source(source: &str) -> Result<Vec<SNode>, PipelineError> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    Ok(parser.parse()?)
}

/// Lex, parse, and type-check source. Returns the AST and any type
/// diagnostics (which may include warnings even on success).
pub fn check_source(source: &str) -> Result<(Vec<SNode>, Vec<TypeDiagnostic>), PipelineError> {
    let program = parse_source(source)?;
    let diagnostics = TypeChecker::new().check_with_source(&program, source);
    Ok((program, diagnostics))
}

/// Lex, parse, and type-check, bailing on the first type error.
pub fn check_source_strict(source: &str) -> Result<Vec<SNode>, PipelineError> {
    let (program, diagnostics) = check_source(source)?;
    for diag in &diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            return Err(PipelineError::TypeCheck(Box::new(diag.clone())));
        }
    }
    Ok(program)
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[test]
    fn parse_source_valid() {
        let program = parse_source("const x = 1").unwrap();
        assert!(!program.is_empty());
    }

    #[test]
    fn parse_source_lex_error() {
        let err = parse_source("let x = `").unwrap_err();
        assert!(matches!(err, PipelineError::Lex(_)));
        assert!(err.span().is_some());
        assert!(err.to_string().contains("Unexpected character"));
    }

    #[test]
    fn parse_source_parse_error() {
        let err = parse_source("let = 1").unwrap_err();
        assert!(matches!(err, PipelineError::Parse(_)));
        assert!(err.span().is_some());
    }

    #[test]
    fn check_source_returns_diagnostics() {
        let (program, _diagnostics) = check_source("const x = 1").unwrap();
        assert!(!program.is_empty());
    }

    #[test]
    fn check_source_strict_passes_valid_code() {
        let program = check_source_strict("const x = 1\nlog(x)").unwrap();
        assert!(!program.is_empty());
    }

    #[test]
    fn check_source_strict_catches_lex_error() {
        let err = check_source_strict("`").unwrap_err();
        assert!(matches!(err, PipelineError::Lex(_)));
    }

    #[test]
    fn pipeline_error_display_is_informative() {
        let err = parse_source("`").unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains('`') || msg.contains("Unexpected"));
    }

    #[test]
    fn pipeline_error_size_is_bounded() {
        // TypeCheck is boxed; guard against accidental growth of the other variants.
        assert!(
            std::mem::size_of::<PipelineError>() <= 96,
            "PipelineError grew to {} bytes — consider boxing large variants",
            std::mem::size_of::<PipelineError>()
        );
    }

    #[test]
    fn legacy_hostlib_names_must_resolve_through_the_typed_registry() {
        assert!(is_registered_legacy_hostlib_name(
            "hostlib_terminal_session_capture"
        ));
        assert!(is_registered_legacy_hostlib_name(
            "hostlib_code_index_agent_heartbeat"
        ));
        assert!(is_registered_legacy_hostlib_name("hostlib_enable"));
        assert!(!is_registered_legacy_hostlib_name(
            "hostlib_terminal_session_not_registered"
        ));
        assert!(!is_registered_legacy_hostlib_name("hostlib_unknown_ping"));
    }
}
