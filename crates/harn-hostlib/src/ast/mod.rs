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
//! [`language::Language`] covers the general-purpose languages
//! (TypeScript/TSX, JavaScript/JSX, Python, Go, Rust, Java, C, C++, C#,
//! Ruby, Kotlin, PHP, Scala, Bash, Swift, Zig, Elixir, Lua, Haskell, R)
//! plus data/markup/config grammars (JSON, YAML, TOML, CSS, HTML, SQL,
//! Markdown). The latter support the query-driven edit primitives but
//! carry no symbol-graph projection — see
//! [`language::Language::edit_capabilities`] for the per-language matrix.
//! Adding/dropping languages requires coordinated schema, fixture, and
//! host-bridge updates.
//!

use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

mod apply_node;
mod bracket_balance;
mod capabilities;
mod dry_run;
mod edit_common;
mod function_body;
mod fuzzy;
mod imports;
mod insert_at_anchor;
mod language;
mod mutation;
mod outline;
mod parse;
mod parse_errors;
mod symbols;
mod symbols_call;
mod types;
mod undefined_names;
mod unified_diff;

pub use language::{EditCapabilities, Language, TEXT_PATCH_FALLBACK};
pub use types::{OutlineItem, ParseError, ParsedNode, Symbol, SymbolKind, UndefinedName};

/// Programmatic entry point to the AST builtins. Embedders typically go
/// through the registered builtins, but tests and tools that want
/// strongly-typed access can use these helpers directly.
pub mod api {
    use std::path::Path;

    use tree_sitter::Tree;

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

    /// Parse a source `str` for `language` and return the raw tree-sitter
    /// tree. Used by the typed symbol graph in
    /// [`crate::code_index::symbol_graph`] to sweep for call sites
    /// without re-doing the work the AST symbol extractor already did.
    pub fn parse_tree(source: &str, language: Language) -> Result<Tree, HostlibError> {
        parse_source(source, language)
    }

    /// Parse `source` once, then return the tree plus the symbol list
    /// extracted from it. Lets a caller (e.g. the typed symbol graph)
    /// avoid paying the parse cost twice when it needs both products.
    pub fn parse_with_symbols(
        source: &str,
        language: Language,
    ) -> Result<(Tree, Vec<Symbol>), HostlibError> {
        let tree = parse_source(source, language)?;
        let symbols = extract(&tree, source, language);
        Ok((tree, symbols))
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
        // These two write edited source back to disk, so they share the
        // deterministic-tools gate with `tools::*` file I/O.
        register_gated(
            registry,
            "hostlib_ast_apply_node",
            "apply_node",
            apply_node::run,
        );
        register_gated(
            registry,
            "hostlib_ast_insert_at_anchor",
            "insert_at_anchor",
            insert_at_anchor::run,
        );
        register(registry, "hostlib_ast_dry_run", "dry_run", dry_run::run);
        register(
            registry,
            "hostlib_ast_capabilities",
            "capabilities",
            capabilities::run,
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

fn register_gated(
    registry: &mut BuiltinRegistry,
    name: &'static str,
    method: &'static str,
    runner: fn(&[VmValue]) -> Result<VmValue, HostlibError>,
) {
    registry.register(RegisteredBuiltin {
        name,
        module: "ast",
        method,
        handler: crate::tools::permissions::gated_handler(name, runner),
    });
}
