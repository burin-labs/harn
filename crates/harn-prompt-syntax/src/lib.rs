//! Prompt-template syntax shared by the renderer, lint, CLI, and language server.
//!
//! Parsing and tooling need no runtime values or host authority. Rendering,
//! includes, capability-dependent sections, and provenance belong to the VM.

pub mod ast;
mod error;
mod expr_parser;
pub mod filters;
mod lexer;
pub mod lint;
pub mod outline;
pub mod parser;
pub mod vocabulary;

pub use error::{TemplateError, TemplateParseError};

/// Maximum nested template AST and expression depth. Runtime limit reports
/// project this same ceiling.
pub const MAX_AST_DEPTH: usize = 128;

/// Validate syntax without resolving include targets or evaluating bindings.
pub fn validate_template_syntax(src: &str) -> Result<(), String> {
    parser::parse(src)
        .map(|_| ())
        .map_err(|error| error.message())
}
