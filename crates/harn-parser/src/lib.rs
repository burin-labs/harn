mod ast;
pub(crate) mod builtin_signatures;
pub mod diagnostic;
mod parser;
pub mod typechecker;

pub use ast::*;
pub use parser::*;
pub use typechecker::{
    format_type, DiagnosticSeverity, InlayHintInfo, TypeChecker, TypeDiagnostic,
};

/// Returns `true` if `name` is a builtin recognized by the parser's static
/// analyzer. Exposed for cross-crate drift tests (see
/// `crates/harn-vm/tests/builtin_registry_alignment.rs`) and any future
/// tooling that needs to validate builtin references without running the
/// VM.
pub fn is_known_builtin(name: &str) -> bool {
    builtin_signatures::is_builtin(name)
}

/// Iterator over every builtin name known to the parser, in alphabetical
/// order. Enables bidirectional drift checks against the VM's runtime
/// registry — a parser entry with no runtime counterpart means a stale
/// signature that should be removed.
pub fn known_builtin_names() -> impl Iterator<Item = &'static str> {
    builtin_signatures::iter_builtin_names()
}

pub fn known_builtin_metadata() -> impl Iterator<Item = builtin_signatures::BuiltinMetadata> {
    builtin_signatures::iter_builtin_metadata()
}
