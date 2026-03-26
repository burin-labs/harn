mod ast;
pub mod diagnostic;
mod parser;
pub mod typechecker;

pub use ast::*;
pub use parser::*;
pub use typechecker::{DiagnosticSeverity, TypeChecker, TypeDiagnostic};
