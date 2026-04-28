//! AST host capability.
//!
//! Wraps tree-sitter parsing, symbol extraction, and outline generation.
//! The implementation is fully wired so AST builtins share one canonical
//! wire format.
//!
//! ## Wire format
//!
//! - Row/column coordinates are **0-based** across all three builtins,
//!   matching tree-sitter's native `Point` representation. `parse_file`,
//!   `symbols`, and `outline` share one convention.
//! - `parse_file` emits a flat node list with `parent_id` rather than
//!   nested children — keeps the wire JSON-serializable without inflating
//!   it with object copies.
//! - `symbols` and `outline` carry a `signature` string (e.g.
//!   `"fn foo(bar: i32)"`) on every entry.
//!
//! ## Languages
//!
//! [`language::Language`] covers TypeScript/TSX, JavaScript/JSX, Python,
//! Go, Rust, Java, C, C++, C#, Ruby, Kotlin, PHP, Scala, Bash, Swift, Zig,
//! Elixir, Lua, Haskell, and R. Adding/dropping languages requires
//! coordinated schema, fixture, and host-bridge updates.
//!

use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

mod bracket_balance;
mod function_body;
mod fuzzy;
mod imports;
mod language;
mod mutation;
mod outline;
mod parse;
mod parse_errors;
mod symbols;
mod symbols_call;
mod types;
mod undefined_names;

pub use language::Language;
pub use types::{OutlineItem, ParseError, ParsedNode, Symbol, SymbolKind, UndefinedName};

/// Programmatic entry point to the AST builtins. Embedders typically go
/// through the registered builtins, but tests and tools that want
/// strongly-typed access can use these helpers directly.
pub mod api {
    use std::path::Path;

    use crate::error::HostlibError;

    use super::language::Language;
    use super::outline::build_outline;
    use super::parse::{parse_source, read_source};
    use super::symbols::extract;
    use super::types::{OutlineItem, Symbol};

    /// Parse `path` (with optional language hint) and return its symbols.
    pub fn symbols(
        path: &Path,
        language_hint: Option<&str>,
    ) -> Result<(Language, Vec<Symbol>), HostlibError> {
        let language = detect(path, language_hint)?;
        let source = read_source(&path.to_string_lossy(), 0)?;
        let tree = parse_source(&source, language)?;
        Ok((language, extract(&tree, &source, language)))
    }

    /// Parse `path` and return a hierarchical outline.
    pub fn outline(
        path: &Path,
        language_hint: Option<&str>,
    ) -> Result<(Language, Vec<OutlineItem>), HostlibError> {
        let (language, symbols) = symbols(path, language_hint)?;
        Ok((language, build_outline(symbols)))
    }

    /// Parse a source `str` for `language` and return its symbols. Useful
    /// for unit tests where the input lives in-memory rather than on disk.
    pub fn symbols_from_source(
        source: &str,
        language: Language,
    ) -> Result<Vec<Symbol>, HostlibError> {
        let tree = parse_source(source, language)?;
        Ok(extract(&tree, source, language))
    }

    fn detect(path: &Path, language_hint: Option<&str>) -> Result<Language, HostlibError> {
        Language::detect(path, language_hint).ok_or_else(|| HostlibError::InvalidParameter {
            builtin: "ast::api",
            param: "language",
            message: format!(
                "could not infer a tree-sitter grammar for `{}` \
                 (extension or `language` field unrecognized)",
                path.display()
            ),
        })
    }
}

/// AST capability handle. Stateless; tree-sitter parsers are constructed
/// per-call (cheap relative to grammar lookup) so the capability itself
/// has nothing to own.
#[derive(Default)]
pub struct AstCapability;

impl HostlibCapability for AstCapability {
    fn module_name(&self) -> &'static str {
        "ast"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        register(registry, "hostlib_ast_parse_file", "parse_file", parse::run);
        register(
            registry,
            "hostlib_ast_symbols",
            "symbols",
            symbols_call::run,
        );
        register(registry, "hostlib_ast_outline", "outline", outline::run);
        register(
            registry,
            "hostlib_ast_parse_errors",
            "parse_errors",
            parse_errors::run,
        );
        register(
            registry,
            "hostlib_ast_undefined_names",
            "undefined_names",
            undefined_names::run,
        );
        register(
            registry,
            "hostlib_ast_function_body",
            "function_body",
            function_body::run_single,
        );
        register(
            registry,
            "hostlib_ast_function_bodies",
            "function_bodies",
            function_body::run_bulk,
        );
        register(
            registry,
            "hostlib_ast_extract_imports",
            "extract_imports",
            imports::run,
        );
        register(
            registry,
            "hostlib_ast_symbol_extract",
            "symbol_extract",
            mutation::run_extract,
        );
        register(
            registry,
            "hostlib_ast_symbol_delete",
            "symbol_delete",
            mutation::run_delete,
        );
        register(
            registry,
            "hostlib_ast_symbol_replace",
            "symbol_replace",
            mutation::run_replace,
        );
        register(
            registry,
            "hostlib_ast_bracket_balance",
            "bracket_balance",
            bracket_balance::run,
        );
    }
}

fn register(
    registry: &mut BuiltinRegistry,
    name: &'static str,
    method: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) {
    let handler: SyncHandler = Arc::new(runner);
    registry.register(RegisteredBuiltin {
        name,
        module: "ast",
        method,
        handler,
    });
}
