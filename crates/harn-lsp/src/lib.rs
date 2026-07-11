//! Harn language server.
//!
//! Exposed as a library so the single multi-call `harn` binary can dispatch
//! into the LSP server when launched under the `harn-lsp` name (see
//! `harn-cli`'s `main`), instead of shipping a second fully-linked binary.
//! The thin `src/main.rs` shim keeps `harn-lsp` buildable as its own binary
//! for local development (`cargo run -p harn-lsp`).

mod call_hierarchy;
mod constants;
mod document;
mod folding;
mod handlers;
mod helpers;
mod references;
mod rules;
mod semantic_tokens;
mod symbols;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use document::DocumentState;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LspService, Server};

pub(crate) struct HarnLsp {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
    pending_reparse_versions: Mutex<HashMap<Url, u64>>,
    rule_workspace: Mutex<rules::RuleWorkspace>,
    /// Whether the client advertised dynamic-registration support for
    /// `workspace/didChangeWatchedFiles` in its `initialize` capabilities.
    /// When true we register a `**/*.harn` file watcher in `initialized`
    /// so external edits (git checkout, another tool) re-validate open docs.
    watched_files_dynamic_registration: AtomicBool,
}

impl HarnLsp {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
            pending_reparse_versions: Mutex::new(HashMap::new()),
            rule_workspace: Mutex::new(rules::RuleWorkspace::default()),
            watched_files_dynamic_registration: AtomicBool::new(false),
        }
    }
}

/// Run the Harn language server over stdio until the client disconnects.
///
/// Builds a multi-threaded Tokio runtime and blocks on it, mirroring the
/// `#[tokio::main]` the standalone binary used. Called by the `harn-lsp`
/// binary shim and by the `harn` multi-call binary when invoked as `harn-lsp`.
pub fn run() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime for harn-lsp");
    runtime.block_on(serve());
}

async fn serve() {
    // Defeat rlib dead-code stripping of the linkme distributed slice
    // (linkme issue #36) before reading `all_builtin_signatures()`.
    harn_vm::stdlib::force_link();
    // Install the macro-emitted builtin signature slice into the parser
    // registry so hover/completion/diagnostics see the full builtin set.
    harn_parser::install_builtin_signatures(harn_vm::stdlib::all_builtin_signatures());

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(HarnLsp::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{lsp_position_to_offset, word_at_position};
    use crate::symbols::HarnSymbolKind;

    /// Build document state and find the best hover symbol for `word` at
    /// the given 0-based line/column.
    fn hover_symbol_at(
        source: &str,
        line: u32,
        col: u32,
        word: &str,
    ) -> Option<symbols::SymbolInfo> {
        let state = DocumentState::new(source.to_string());
        let position = Position::new(line, col);

        let extracted = word_at_position(source, position);
        assert_eq!(
            extracted.as_deref(),
            Some(word),
            "word_at_position mismatch"
        );

        let cursor_offset = lsp_position_to_offset(source, position);

        // Mirrors handlers::hover's scope resolution: tightest scope wins.
        let mut best: Option<&symbols::SymbolInfo> = None;
        for sym in &state.symbols {
            if sym.name != word {
                continue;
            }
            let in_scope = if sym.impl_type.is_some() {
                true
            } else {
                match sym.scope_span {
                    Some(sp) => cursor_offset >= sp.start && cursor_offset <= sp.end,
                    None => true,
                }
            };
            if !in_scope {
                continue;
            }
            match best {
                None => best = Some(sym),
                Some(prev) => {
                    let prev_size = prev
                        .scope_span
                        .map_or(usize::MAX, |sp| sp.end.saturating_sub(sp.start));
                    let this_size = sym
                        .scope_span
                        .map_or(usize::MAX, |sp| sp.end.saturating_sub(sp.start));
                    if this_size < prev_size {
                        best = Some(sym);
                    }
                }
            }
        }

        best.cloned()
    }

    #[test]
    fn hover_top_level_fn() {
        let source = "fn greet(name: string) -> string {\n  return \"Hello, \" + name\n}\n\nlet result = greet(\"World\")\n";
        let sym = hover_symbol_at(source, 4, 14, "greet").expect("should find greet");
        assert_eq!(sym.kind, HarnSymbolKind::Function);
        assert_eq!(
            sym.signature.as_deref(),
            Some("fn greet(name: string) -> string")
        );
        assert!(sym.scope_span.is_none(), "top-level fn has no scope_span");
        assert!(sym.impl_type.is_none());
    }

    #[test]
    fn hover_fn_with_default_param() {
        let source =
            "fn greet(name: string = \"World\") -> string {\n  return \"Hello, \" + name\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "greet" && s.kind == HarnSymbolKind::Function)
            .expect("should find greet");
        assert_eq!(
            fn_sym.signature.as_deref(),
            Some("fn greet(name: string = \"World\") -> string")
        );
    }

    #[test]
    fn hover_fn_with_doc_comment() {
        let source = "/// Greets a person by name.\n/// Returns a greeting string.\nfn greet(name: string) -> string {\n  return \"Hello, \" + name\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "greet" && s.kind == HarnSymbolKind::Function)
            .expect("should find greet");
        assert_eq!(
            fn_sym.doc_comment.as_deref(),
            Some("Greets a person by name.\nReturns a greeting string.")
        );
    }

    #[test]
    fn hover_fn_exposes_stdlib_metadata_block() {
        let source = "\
/**
 * Read a file as UTF-8.
 *
 * @effects: [fs.read]
 * @errors: [FileNotFound, PermissionDenied]
 * @example: const s = read_to_string(\"/tmp/x\")
 */
fn read_to_string(path: string) -> string {
  return \"\"
}
";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "read_to_string" && s.kind == HarnSymbolKind::Function)
            .expect("should find fn");
        let meta = fn_sym.stdlib_metadata.as_ref().expect("metadata present");
        assert!(meta.is_complete(), "missing: {:?}", meta.missing_fields());
        assert_eq!(meta.effects.as_deref(), Some(&["fs.read".to_string()][..]));
        // Authored example wins over the derived one in the rendered block.
        let md = meta.to_markdown_with_derived_example(fn_sym.derived_example.as_deref());
        assert!(md.contains("const s = read_to_string(\"/tmp/x\")"));
        assert!(!md.contains("derived from signature"));
    }

    #[test]
    fn hover_fn_without_example_gets_derived_one() {
        let source = "\
/**
 * Read a file as UTF-8.
 *
 * @effects: [fs.read]
 * @errors: [FileNotFound]
 */
fn read_to_string(path: string) -> string {
  return \"\"
}
";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "read_to_string" && s.kind == HarnSymbolKind::Function)
            .expect("should find fn");
        assert_eq!(
            fn_sym.derived_example.as_deref(),
            Some("const out = read_to_string(path)")
        );
        let meta = fn_sym.stdlib_metadata.as_ref().expect("metadata present");
        let md = meta.to_markdown_with_derived_example(fn_sym.derived_example.as_deref());
        assert!(md.contains("derived from signature"));
        assert!(md.contains("const out = read_to_string(path)"));
    }

    #[test]
    fn hover_undocumented_fn_still_derives_example() {
        let source = "fn notify(event) {\n  return\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "notify" && s.kind == HarnSymbolKind::Function)
            .expect("should find fn");
        assert!(fn_sym.stdlib_metadata.is_none());
        assert_eq!(fn_sym.derived_example.as_deref(), Some("notify(event)"));
    }

    #[test]
    fn hover_fn_with_plain_comment_fallback() {
        let source = "// Greets a person by name.\nfn greet(name: string) -> string {\n  return \"Hello, \" + name\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "greet" && s.kind == HarnSymbolKind::Function)
            .expect("should find greet");
        assert_eq!(
            fn_sym.doc_comment.as_deref(),
            Some("Greets a person by name.")
        );
    }

    #[test]
    fn hover_fn_no_doc_comment() {
        let source =
            "const x = 1\n\nfn greet(name: string) -> string {\n  return \"Hello, \" + name\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "greet" && s.kind == HarnSymbolKind::Function)
            .expect("should find greet");
        assert!(
            fn_sym.doc_comment.is_none(),
            "non-comment line above should not produce doc_comment"
        );
    }

    #[test]
    fn hover_impl_method_visible_outside() {
        let source = concat!(
            "struct Point { x: int, y: int }\n",
            "\n",
            "impl Point {\n",
            "  // Returns the sum of x and y.\n",
            "  fn sum(self) -> int {\n",
            "    return self.x + self.y\n",
            "  }\n",
            "}\n",
            "\n",
            "const p = Point({x: 1, y: 2})\n",
            "const s = p.sum()\n",
        );
        let sym = hover_symbol_at(source, 10, 12, "sum").expect("should find sum method");
        assert_eq!(sym.kind, HarnSymbolKind::Function);
        assert_eq!(sym.signature.as_deref(), Some("fn sum(self) -> int"));
        assert_eq!(sym.impl_type.as_deref(), Some("Point"));
        assert_eq!(
            sym.doc_comment.as_deref(),
            Some("Returns the sum of x and y.")
        );
    }

    #[test]
    fn hover_fn_untyped_params() {
        let source = "fn add(a, b) {\n  return a + b\n}\n";
        let state = DocumentState::new(source.to_string());
        let fn_sym = state
            .symbols
            .iter()
            .find(|s| s.name == "add" && s.kind == HarnSymbolKind::Function)
            .expect("should find add");
        assert_eq!(fn_sym.signature.as_deref(), Some("fn add(a, b)"));
    }

    #[test]
    fn hover_pipeline() {
        let source = "// Main entry point.\npipeline main() {\n  __io_println(\"hello\")\n}\n";
        let state = DocumentState::new(source.to_string());
        let sym = state
            .symbols
            .iter()
            .find(|s| s.name == "main" && s.kind == HarnSymbolKind::Pipeline)
            .expect("should find main pipeline");
        assert_eq!(sym.signature.as_deref(), Some("pipeline main"));
        assert_eq!(sym.doc_comment.as_deref(), Some("Main entry point."));
    }

    #[test]
    fn hover_public_pipeline_signature() {
        let source = "pub pipeline build(task) extends base {\n  return\n}\n";
        let state = DocumentState::new(source.to_string());
        let sym = state
            .symbols
            .iter()
            .find(|s| s.name == "build" && s.kind == HarnSymbolKind::Pipeline)
            .expect("should find build pipeline");
        assert_eq!(sym.signature.as_deref(), Some("pub pipeline build(task)"));
    }

    #[test]
    fn hover_captures_flow_predicate_attributes() {
        let source = concat!(
            "@invariant\n",
            "@deterministic\n",
            "@archivist(evidence: [\"https://example.com/spec\"], confidence: 0.9, source_date: \"2026-04-01\")\n",
            "fn no_secrets(slice) -> bool {\n",
            "  return true\n",
            "}\n",
        );
        let state = DocumentState::new(source.to_string());
        let sym = state
            .symbols
            .iter()
            .find(|s| s.name == "no_secrets" && s.kind == HarnSymbolKind::Function)
            .expect("should find no_secrets");
        let names: Vec<&str> = sym.attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["invariant", "deterministic", "archivist"]);
        let block = crate::symbols::format_flow_attributes_block(&sym.attributes)
            .expect("flow metadata block");
        assert!(block.contains("@invariant"));
        assert!(block.contains("@deterministic"));
        assert!(block.contains("@archivist"));
        assert!(block.contains("evidence"));
        assert!(block.contains("https://example.com/spec"));
    }

    #[test]
    fn hover_generic_interface_signature() {
        let source = "interface Repository<T> {\n  fn map<U>(value: T, f: fn(T) -> U) -> U\n}\n";
        let state = DocumentState::new(source.to_string());
        let sym = state
            .symbols
            .iter()
            .find(|s| s.name == "Repository" && s.kind == HarnSymbolKind::Interface)
            .expect("should find Repository interface");
        assert_eq!(
            sym.signature.as_deref(),
            Some("interface Repository<T> { fn map<U>(value, f) }")
        );
    }
}
