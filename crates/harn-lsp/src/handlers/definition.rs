//! Go-to-definition, find-references, and rename.

use std::collections::HashMap;

use harn_lexer::{Lexer, Span, TokenKind};
use harn_modules::DefKind;
use harn_parser::{Node, SNode};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use tower_lsp::lsp_types::*;

use crate::constants::is_builtin;
use crate::helpers::{infer_dot_receiver_name, span_to_full_range, word_at_position};
use crate::references::{find_references, identifier_token_spans_within};
use crate::source_text::SourceText;
use crate::symbols::HarnSymbolKind;
use crate::HarnLsp;

impl HarnLsp {
    pub(super) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        // Inside a `render(...)` / `render_prompt(...)` string literal,
        // jump straight to the referenced `.harn.prompt` file. Honors
        // package-root forms (`@/...`, `@<alias>/...`) via the same
        // resolver the runtime and preflight checks use (#742).
        if let Some(program) = &ast {
            if let Some(loc) = resolve_prompt_asset_definition(uri, &source, position, program) {
                return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
            }
        }

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        for sym in &symbols {
            if sym.name == word
                && matches!(
                    sym.kind,
                    HarnSymbolKind::Pipeline
                        | HarnSymbolKind::Function
                        | HarnSymbolKind::Variable
                        | HarnSymbolKind::Parameter
                        | HarnSymbolKind::Enum
                        | HarnSymbolKind::Struct
                        | HarnSymbolKind::Interface
                )
            {
                let range = span_to_full_range(&sym.def_span, &source);
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })));
            }
        }

        // Namespace import: `alias.member` → export definition; bare `alias`
        // → the imported module file.
        if let Some(loc) = resolve_namespace_definition(uri, &source, position, &word) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }

        // Cross-file: the module graph transitively follows imports from
        // this file, so there's no need to pre-walk the AST here.
        if let Some(loc) = resolve_cross_file_definition(uri, &word) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }

        Ok(None)
    }

    /// `textDocument/declaration` — for Harn there's no separate
    /// declaration vs definition (no header files, no forward decls),
    /// so this delegates to `goto_definition` for parity with editor
    /// expectations.
    pub(super) async fn handle_goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        self.handle_goto_definition(params).await
    }

    /// `textDocument/typeDefinition` — jumps to the type that the
    /// symbol under the cursor *is*, which for Harn currently means
    /// the same destination as `goto_definition` (the type's struct /
    /// enum / interface declaration). The split exists so the IDE's
    /// "go to type definition" command works without misfiring.
    pub(super) async fn handle_goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Look for a *type* declaration with the same name, preferring
        // struct/enum/interface over plain variables/functions.
        for sym in &symbols {
            if sym.name == word
                && matches!(
                    sym.kind,
                    HarnSymbolKind::Struct | HarnSymbolKind::Enum | HarnSymbolKind::Interface
                )
            {
                let range = span_to_full_range(&sym.def_span, &source);
                return Ok(Some(GotoTypeDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })));
            }
        }

        if let Some(loc) = resolve_cross_file_definition(uri, &word) {
            return Ok(Some(GotoTypeDefinitionResponse::Scalar(loc)));
        }

        Ok(None)
    }

    /// `textDocument/implementation` — for Harn this maps to "every
    /// place the symbol is defined or used in a defining context",
    /// which today is the same answer as `references` filtered to the
    /// current document. Returning the references list keeps the IDE
    /// from showing a "no implementation found" toast when there is in
    /// fact one or more concrete definitions to jump to.
    pub(super) async fn handle_goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let proxy = ReferenceParams {
            text_document_position: params.text_document_position_params,
            work_done_progress_params: params.work_done_progress_params,
            partial_result_params: params.partial_result_params,
            context: ReferenceContext {
                include_declaration: true,
            },
        };
        Ok(self
            .handle_references(proxy)
            .await?
            .map(GotoImplementationResponse::Array))
    }

    /// `textDocument/documentHighlight` — highlight every occurrence
    /// of the symbol under the cursor inside the current document.
    /// Used by editors to underline the symbol's siblings when the
    /// cursor lands on it.
    pub(super) async fn handle_document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        let mut highlights: Vec<DocumentHighlight> = Vec::new();
        let mut def_offsets = std::collections::HashSet::new();
        for sym in &symbols {
            if sym.name == word {
                def_offsets.insert(sym.def_span.start);
            }
        }

        let mut lexer = Lexer::new(&source);
        if let Ok(tokens) = lexer.tokenize() {
            for token in &tokens {
                if let TokenKind::Identifier(ref name) = token.kind {
                    if name == &word {
                        let start = source.position(token.span.start);
                        let end = source.position(token.span.end);
                        let kind = if def_offsets.contains(&token.span.start) {
                            DocumentHighlightKind::WRITE
                        } else {
                            DocumentHighlightKind::READ
                        };
                        highlights.push(DocumentHighlight {
                            range: Range { start, end },
                            kind: Some(kind),
                        });
                    }
                }
            }
        }

        if highlights.is_empty() {
            Ok(None)
        } else {
            Ok(Some(highlights))
        }
    }

    pub(super) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        drop(docs);

        let word = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };

        let ref_spans = find_references(&program, &word);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        // Raw AST spans cover whole declarations for definition sites;
        // narrow each hit to the identifier token so the references list
        // doesn't highlight entire functions.
        let token_spans = identifier_token_spans_within(&source, &word, &ref_spans);
        if token_spans.is_empty() {
            return Ok(None);
        }

        let locations: Vec<Location> = token_spans
            .iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_full_range(span, &source),
            })
            .collect();

        Ok(Some(locations))
    }

    pub(super) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = &params.new_name;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.cached_ast.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let old_name = match word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Builtins must not be renamed.
        if is_builtin(&old_name) {
            return Ok(None);
        }

        let symbol_exists = symbols.iter().any(|s| s.name == old_name);
        if !symbol_exists {
            return Ok(None);
        }

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };
        let ref_spans = find_references(&program, &old_name);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        // AST reference spans cover whole declarations, so rescan the lexer
        // tokens within each span to pin down the exact identifier position.
        let mut edits: Vec<TextEdit> =
            identifier_token_spans_within(&source, &old_name, &ref_spans)
                .into_iter()
                .map(|span| TextEdit {
                    range: Range {
                        start: source.position(span.start),
                        end: source.position(span.end),
                    },
                    new_text: new_name.clone(),
                })
                .collect();

        if edits.is_empty() {
            return Ok(None);
        }

        // Sort bottom-up so applying edits doesn't shift later offsets.
        edits.sort_by(|a, b| {
            b.range
                .start
                .line
                .cmp(&a.range.start.line)
                .then(b.range.start.character.cmp(&a.range.start.character))
        });

        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }
}

/// When the cursor sits inside a string literal that's the first
/// argument to a literal `render(...)` or `render_prompt(...)` call,
/// resolve the path (source-relative or `@/...` / `@<alias>/...`) and
/// return a `Location` pointing at the prompt file's first byte. Returns
/// `None` for any other context, so callers can fall through to symbol
/// resolution.
fn resolve_prompt_asset_definition(
    uri: &Url,
    source: &SourceText,
    position: Position,
    program: &[SNode],
) -> Option<Location> {
    let offset = source.offset(position);
    let (template_path, _) = find_render_string_at_offset(program, offset)?;
    let current_path = uri.to_file_path().ok()?;
    let resolved = if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
        let anchor = current_path.parent().unwrap_or(std::path::Path::new("."));
        harn_modules::asset_paths::resolve(&asset_ref, anchor).ok()?
    } else if std::path::Path::new(&template_path).is_absolute() {
        std::path::PathBuf::from(&template_path)
    } else {
        current_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(&template_path)
    };
    if !resolved.exists() {
        return None;
    }
    let target_uri = Url::from_file_path(&resolved).ok()?;
    Some(Location {
        uri: target_uri,
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        },
    })
}

fn find_render_string_at_offset(program: &[SNode], offset: usize) -> Option<(String, Span)> {
    for node in program {
        if let Some(hit) = find_render_string_in_node(node, offset) {
            return Some(hit);
        }
    }
    None
}

fn find_render_string_in_node(node: &SNode, offset: usize) -> Option<(String, Span)> {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        if (name == "render" || name == "render_prompt") && !args.is_empty() {
            if let Node::StringLiteral(value) = &args[0].node {
                let span = args[0].span;
                if span_contains_offset(&span, offset) {
                    return Some((value.clone(), span));
                }
            }
        }
    }
    for child in node_children(node) {
        if let Some(hit) = find_render_string_in_node(child, offset) {
            return Some(hit);
        }
    }
    None
}

fn span_contains_offset(span: &Span, offset: usize) -> bool {
    offset >= span.start && offset <= span.end
}

fn node_children(node: &SNode) -> Vec<&SNode> {
    harn_parser::visit::immediate_children(node)
}

/// Resolve a namespace import alias or `alias.member` to its definition.
fn resolve_namespace_definition(
    uri: &Url,
    source: &SourceText,
    position: Position,
    word: &str,
) -> Option<Location> {
    let current_path = uri.to_file_path().ok()?;
    let module_graph = harn_modules::build(std::slice::from_ref(&current_path));
    let namespaces = module_graph.namespace_imports_for_file(&current_path)?;

    // Prefer `alias.member` when the cursor is on the member after a dot.
    if let Some(alias) = infer_dot_receiver_name(source, position) {
        if namespaces.iter().any(|ns| ns.alias == alias) {
            let def = module_graph.namespace_member_lookup(&current_path, &alias, word)?;
            let imported_source = SourceText::new(std::fs::read_to_string(&def.file).ok()?);
            let imported_uri = Url::from_file_path(&def.file).ok()?;
            return Some(Location {
                uri: imported_uri,
                range: span_to_full_range(&def.span, &imported_source),
            });
        }
    }

    // Bare alias → jump to the target module file (start of file).
    let ns = namespaces.iter().find(|ns| ns.alias == word)?;
    let module_path = ns.resolved_path.as_ref()?;
    let imported_source = SourceText::new(std::fs::read_to_string(module_path).ok()?);
    let imported_uri = Url::from_file_path(module_path).ok()?;
    Some(Location {
        uri: imported_uri,
        range: span_to_full_range(
            &harn_lexer::Span::with_offsets(0, 0, 1, 1),
            &imported_source,
        ),
    })
}

/// Resolve the symbol through the current document's imported modules using
/// `harn-modules`, and return its definition location when available.
///
/// `harn_modules::build` recursively follows import paths, so seeding it
/// with the current file is enough to discover every module reachable via
/// imports.
fn resolve_cross_file_definition(uri: &Url, word: &str) -> Option<Location> {
    let current_path = uri.to_file_path().ok()?;
    let module_graph = harn_modules::build(std::slice::from_ref(&current_path));
    let def = module_graph.definition_of(&current_path, word)?;
    if !matches!(
        def.kind,
        DefKind::Pipeline
            | DefKind::Function
            | DefKind::Variable
            | DefKind::Parameter
            | DefKind::Enum
            | DefKind::Struct
            | DefKind::Interface
    ) {
        return None;
    }
    let imported_source = SourceText::new(std::fs::read_to_string(&def.file).ok()?);
    let imported_uri = Url::from_file_path(&def.file).ok()?;
    Some(Location {
        uri: imported_uri,
        range: span_to_full_range(&def.span, &imported_source),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_parser::parse_source;

    #[test]
    fn finds_render_prompt_string_under_cursor() {
        let source = r#"
pipeline test() {
  const x = render_prompt("@/prompts/foo.harn.prompt", {})
  __io_println(x)
}
"#;
        let program = parse_source(source).expect("parse");
        // Cursor inside the string literal — anywhere in the quoted span.
        let cursor = source.find("@/prompts").unwrap() + 3;
        let (path, _) =
            find_render_string_at_offset(&program, cursor).expect("should locate the asset string");
        assert_eq!(path, "@/prompts/foo.harn.prompt");
    }

    #[test]
    fn ignores_other_function_calls() {
        let source = r#"
pipeline test() {
  const x = __io_println("@/not-a-prompt")
}
"#;
        let program = parse_source(source).expect("parse");
        let cursor = source.find("@/not-a-prompt").unwrap() + 3;
        assert!(find_render_string_at_offset(&program, cursor).is_none());
    }

    #[test]
    fn finds_render_string_outside_string_returns_none() {
        let source = r#"
pipeline test() {
  const x = render_prompt("@/prompts/foo.harn.prompt", {})
}
"#;
        let program = parse_source(source).expect("parse");
        // Cursor on `render_prompt` identifier — not inside the string.
        let cursor = source.find("render_prompt").unwrap() + 2;
        assert!(find_render_string_at_offset(&program, cursor).is_none());
    }
}
