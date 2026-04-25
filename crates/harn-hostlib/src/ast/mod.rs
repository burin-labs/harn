//! AST host capability.
//!
//! Wraps tree-sitter parsing, symbol extraction, and outline generation —
//! the Swift `Sources/ASTEngine/` surface ported into Rust. Implementation
//! lands in follow-up issue B2; this scaffold registers the contract so
//! `burin-code`'s schema-drift tests can lock the public surface today.

use crate::registry::{BuiltinRegistry, HostlibCapability};

/// AST capability handle. Stateless today; will own a tree-sitter language
/// registry and parser pool once the implementation lands.
#[derive(Default)]
pub struct AstCapability;

impl HostlibCapability for AstCapability {
    fn module_name(&self) -> &'static str {
        "ast"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register_unimplemented("hostlib_ast_parse_file", "ast", "parse_file");
        registry.register_unimplemented("hostlib_ast_symbols", "ast", "symbols");
        registry.register_unimplemented("hostlib_ast_outline", "ast", "outline");
    }
}
