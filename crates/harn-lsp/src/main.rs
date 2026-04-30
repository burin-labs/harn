mod call_hierarchy;
mod constants;
mod document;
mod folding;
mod handlers;
mod helpers;
mod references;
mod semantic_tokens;
mod symbols;

use std::collections::HashMap;
use std::sync::Mutex;

use document::DocumentState;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LspService, Server};

struct HarnLsp {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
    pending_reparse_versions: Mutex<HashMap<Url, u64>>,
}

impl HarnLsp {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
            pending_reparse_versions: Mutex::new(HashMap::new()),
        }
    }
}

#[tokio::main]
async fn main() {
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
            "let x = 1\n\nfn greet(name: string) -> string {\n  return \"Hello, \" + name\n}\n";
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
            "let p = Point({x: 1, y: 2})\n",
            "let s = p.sum()\n",
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
        let source = "// Main entry point.\npipeline main() {\n  println(\"hello\")\n}\n";
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
